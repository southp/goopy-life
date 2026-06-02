use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;

use crate::goopy::Goopy;
use crate::shared_types::*;
use super::GoopyRegistry;

/// SQLite-backed implementation of [`GoopyRegistry`].
///
/// Uses an `r2d2` connection pool so multiple threads can hold separate read
/// connections simultaneously while writes serialise at the SQLite WAL level.
///
/// Pass `":memory:"` as `db_path` to get an in-process ephemeral store
/// suitable for tests (pool size is capped at 1 for in-memory databases).
pub struct SqliteRegistry {
    pool: Pool<SqliteConnectionManager>,
}

impl SqliteRegistry {
    /// Open (or create) the SQLite database at `db_path`, enable WAL mode,
    /// and run the schema migration to ensure the required tables exist.
    pub fn new(db_path: &Path) -> Result<Self, Error> {
        let is_memory = db_path == Path::new(":memory:");

        let manager = SqliteConnectionManager::file(db_path)
            .with_init(|conn| {
                conn.execute_batch("PRAGMA busy_timeout = 5000;")?;
                Ok(())
            });

        // Only the literal ":memory:" path is recognised as in-memory; URI-form
        // in-memory databases (file::memory:?cache=shared) are not supported.
        let pool_size = if is_memory { 1 } else { 8 };

        let pool = Pool::builder()
            .max_size(pool_size)
            .build(manager)
            .map_err(|e| Error::Registry(Box::new(e)))?;

        let conn = pool.get()
            .map_err(|e| Error::Registry(Box::new(e)))?;

        if !is_memory {
            let mode: String = conn
                .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
                .map_err(|e| Error::Registry(Box::new(e)))?;
            if mode != "wal" {
                return Err(Error::Registry(
                    format!("WAL mode not available (got: {mode})").into(),
                ));
            }
        }

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS goopies (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                slug             TEXT    UNIQUE NOT NULL,
                life_in_days     INTEGER NOT NULL,
                created_at       TEXT    NOT NULL,
                status           TEXT    NOT NULL,
                working_dir      TEXT    NOT NULL,
                port             INTEGER NOT NULL,
                provisioner_kind TEXT    NOT NULL,
                service_version  TEXT    NOT NULL
            );

            CREATE TABLE IF NOT EXISTS allocated_ports (
                port INTEGER PRIMARY KEY
            );
            ",
        )
        .map_err(Error::SchemaMigration)?;

        Ok(Self { pool })
    }
}

// ---------------------------------------------------------------------------
// Helper: map a rusqlite row to a Goopy via `from_stored`
// ---------------------------------------------------------------------------
fn row_to_goopy(
    slug: String,
    life_in_days: i64,
    created_at_str: String,
    status_str: String,
    working_dir_str: String,
    port: i64,
    provisioner_kind_str: String,
    service_version: String,
) -> Result<Goopy, Error> {
    let created_at: DateTime<Utc> = created_at_str
        .parse::<DateTime<Utc>>()
        .map_err(|_| Error::Invalid)?;

    let status = Status::from_str(&status_str)?;
    let provisioner_kind = ProvisionerKind::from_str(&provisioner_kind_str)?;

    Ok(Goopy::from_stored(
        slug,
        life_in_days as i32,
        created_at,
        Path::new(&working_dir_str),
        port as u32,
        status,
        provisioner_kind,
        service_version,
    ))
}

// ---------------------------------------------------------------------------
// GoopyRegistry impl
// ---------------------------------------------------------------------------
impl GoopyRegistry for SqliteRegistry {
    #[tracing::instrument(skip(self))]
    fn save(&self, gp: &Goopy) -> Result<(), Error> {
        let conn = self.pool.get()
            .map_err(|e| Error::Registry(Box::new(e)))?;

        let result = conn.execute(
            "INSERT OR FAIL INTO goopies
             (slug, life_in_days, created_at, status, working_dir, port,
              provisioner_kind, service_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                gp.slug,
                gp.life_in_days as i64,
                gp.created_at.to_rfc3339(),
                gp.status.to_string(),
                gp.working_dir.to_string_lossy().as_ref(),
                gp.port as i64,
                gp.provisioner_kind.to_string(),
                gp.service_version,
            ],
        );

        match result {
            Ok(_) => {
                tracing::debug!(slug = %gp.slug, "saved goopy");
                Ok(())
            }
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                tracing::warn!(slug = %gp.slug, "save failed: already exists");
                Err(Error::AlreadyExists)
            }
            Err(e) => {
                tracing::error!(slug = %gp.slug, "save failed: {e}");
                Err(Error::Registry(Box::new(e)))
            }
        }
    }

    #[tracing::instrument(skip(self))]
    fn load(&self, slug: &str) -> Result<Option<Goopy>, Error> {
        let conn = self.pool.get()
            .map_err(|e| Error::Registry(Box::new(e)))?;

        let result = conn.query_row(
            "SELECT slug, life_in_days, created_at, status, working_dir,
                    port, provisioner_kind, service_version
             FROM goopies WHERE slug = ?1",
            params![slug],
            |row| {
                Ok((
                    row.get::<_, String>("slug")?,
                    row.get::<_, i64>("life_in_days")?,
                    row.get::<_, String>("created_at")?,
                    row.get::<_, String>("status")?,
                    row.get::<_, String>("working_dir")?,
                    row.get::<_, i64>("port")?,
                    row.get::<_, String>("provisioner_kind")?,
                    row.get::<_, String>("service_version")?,
                ))
            },
        );

        match result {
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(Error::Registry(Box::new(e))),
            Ok((slug, life_in_days, created_at, status, working_dir, port, pk, sv)) => {
                let gp = row_to_goopy(slug, life_in_days, created_at, status, working_dir, port, pk, sv)?;
                tracing::debug!(slug = %gp.slug, "loaded goopy");
                Ok(Some(gp))
            }
        }
    }

    #[tracing::instrument(skip(self))]
    fn update_status(&self, slug: &str, new_status: Status) -> Result<(), Error> {
        let conn = self.pool.get()
            .map_err(|e| Error::Registry(Box::new(e)))?;

        let n = conn
            .execute(
                "UPDATE goopies SET status = ?1 WHERE slug = ?2",
                params![new_status.to_string(), slug],
            )
            .map_err(|e| Error::Registry(Box::new(e)))?;

        if n == 0 {
            tracing::error!(slug = %slug, "update_status: not found");
            Err(Error::NotFound)
        } else {
            tracing::debug!(slug = %slug, status = %new_status, "updated status");
            Ok(())
        }
    }

    #[tracing::instrument(skip(self))]
    fn delete(&self, slug: &str) -> Result<(), Error> {
        let conn = self.pool.get()
            .map_err(|e| Error::Registry(Box::new(e)))?;

        let n = conn
            .execute("DELETE FROM goopies WHERE slug = ?1", params![slug])
            .map_err(|e| Error::Registry(Box::new(e)))?;

        if n == 0 {
            tracing::error!(slug = %slug, "delete: not found");
            Err(Error::NotFound)
        } else {
            tracing::debug!(slug = %slug, "deleted goopy");
            Ok(())
        }
    }

    #[tracing::instrument(skip(self))]
    fn list(&self) -> Result<Vec<Goopy>, Error> {
        let conn = self.pool.get()
            .map_err(|e| Error::Registry(Box::new(e)))?;

        let mut stmt = conn
            .prepare(
                "SELECT slug, life_in_days, created_at, status, working_dir,
                        port, provisioner_kind, service_version
                 FROM goopies ORDER BY created_at",
            )
            .map_err(|e| Error::Registry(Box::new(e)))?;

        let goopies = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>("slug")?,
                    row.get::<_, i64>("life_in_days")?,
                    row.get::<_, String>("created_at")?,
                    row.get::<_, String>("status")?,
                    row.get::<_, String>("working_dir")?,
                    row.get::<_, i64>("port")?,
                    row.get::<_, String>("provisioner_kind")?,
                    row.get::<_, String>("service_version")?,
                ))
            })
            .map_err(|e| Error::Registry(Box::new(e)))?
            .map(|r| {
                let (slug, life_in_days, created_at, status, working_dir, port, pk, sv) =
                    r.map_err(|e| Error::Registry(Box::new(e)))?;
                row_to_goopy(slug, life_in_days, created_at, status, working_dir, port, pk, sv)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(goopies)
    }

    #[tracing::instrument(skip(self))]
    fn acquire_port(&self, range_start: u32, range_end: u32) -> Result<u32, Error> {
        let mut conn = self.pool.get()
            .map_err(|e| Error::Registry(Box::new(e)))?;

        let tx = conn
            .transaction()
            .map_err(|e| Error::Registry(Box::new(e)))?;

        // O(n) scan across the range. Acceptable for ranges of a few hundred ports;
        // for larger ranges a single-query approach (SELECT MIN unused port) is preferable.
        for port in range_start..range_end {
            let result = tx.execute(
                "INSERT OR IGNORE INTO allocated_ports (port) VALUES (?1)",
                params![port as i64],
            );

            match result {
                Ok(1) => {
                    tx.commit()
                        .map_err(|e| Error::Registry(Box::new(e)))?;
                    tracing::debug!(port = port, "acquired port");
                    return Ok(port);
                }
                Ok(_) => {
                    // Row already existed (OR IGNORE silently skipped it)
                    continue;
                }
                Err(e) => {
                    return Err(Error::Registry(Box::new(e)));
                }
            }
        }

        tracing::error!("port range {range_start}..{range_end} exhausted");
        Err(Error::PortExhausted)
    }

    #[tracing::instrument(skip(self))]
    fn release_port(&self, port: u32) -> Result<(), Error> {
        let conn = self.pool.get()
            .map_err(|e| Error::Registry(Box::new(e)))?;

        let n = conn
            .execute(
                "DELETE FROM allocated_ports WHERE port = ?1",
                params![port as i64],
            )
            .map_err(|e| Error::Registry(Box::new(e)))?;

        if n == 0 {
            return Err(Error::NotFound);
        }

        tracing::debug!(port = port, "released port");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_goopy(slug: &str) -> Goopy {
        Goopy::new(
            slug.to_string(),
            7,
            chrono::Utc::now(),
            &PathBuf::from(format!("/tmp/{slug}")),
            8080,
            Status::Spawning,
            ProvisionerKind::Ghost,
            "0.1.0".to_string(),
        )
        .unwrap()
    }

    fn registry() -> SqliteRegistry {
        SqliteRegistry::new(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn save_and_load() {
        let r = registry();
        let gp = make_goopy("test-slug");
        r.save(&gp).unwrap();
        let loaded = r.load("test-slug").unwrap().unwrap();
        assert_eq!(loaded.slug, gp.slug);
        assert_eq!(loaded.life_in_days, gp.life_in_days);
        assert_eq!(loaded.status, gp.status);
        assert_eq!(loaded.port, gp.port);
        assert_eq!(loaded.working_dir, gp.working_dir);
        assert_eq!(loaded.provisioner_kind, gp.provisioner_kind);
        assert_eq!(loaded.service_version, gp.service_version);
        assert_eq!(loaded.created_at, gp.created_at);
    }

    #[test]
    fn load_missing() {
        let r = registry();
        assert!(r.load("nonexistent").unwrap().is_none());
    }

    #[test]
    fn save_duplicate_returns_already_exists() {
        let r = registry();
        let gp = make_goopy("dup-slug");
        r.save(&gp).unwrap();
        let err = r.save(&gp).unwrap_err();
        assert!(matches!(err, Error::AlreadyExists));
    }

    #[test]
    fn update_status() {
        let r = registry();
        let gp = make_goopy("status-slug");
        r.save(&gp).unwrap();
        r.update_status("status-slug", Status::Done).unwrap();
        let loaded = r.load("status-slug").unwrap().unwrap();
        assert_eq!(loaded.status, Status::Done);
    }

    #[test]
    fn update_status_missing() {
        let r = registry();
        let err = r.update_status("no-such", Status::Done).unwrap_err();
        assert!(matches!(err, Error::NotFound));
    }

    #[test]
    fn delete() {
        let r = registry();
        let gp = make_goopy("del-slug");
        r.save(&gp).unwrap();
        r.delete("del-slug").unwrap();
        assert!(r.load("del-slug").unwrap().is_none());
    }

    #[test]
    fn delete_missing() {
        let r = registry();
        let err = r.delete("missing").unwrap_err();
        assert!(matches!(err, Error::NotFound));
    }

    #[test]
    fn list() {
        let r = registry();
        r.save(&make_goopy("alpha")).unwrap();
        r.save(&make_goopy("beta")).unwrap();
        let goopies = r.list().unwrap();
        assert_eq!(goopies.len(), 2);
        let slugs: Vec<&str> = goopies.iter().map(|g| g.slug.as_str()).collect();
        assert!(slugs.contains(&"alpha"));
        assert!(slugs.contains(&"beta"));
    }

    #[test]
    fn acquire_port_basic() {
        let r = registry();
        let p1 = r.acquire_port(9000, 9010).unwrap();
        let p2 = r.acquire_port(9000, 9010).unwrap();
        assert_ne!(p1, p2);
        assert!(p1 >= 9000 && p1 < 9010);
        assert!(p2 >= 9000 && p2 < 9010);
    }

    #[test]
    fn acquire_port_exhaustion() {
        let r = registry();
        r.acquire_port(9100, 9102).unwrap();
        r.acquire_port(9100, 9102).unwrap();
        let err = r.acquire_port(9100, 9102).unwrap_err();
        assert!(matches!(err, Error::PortExhausted));
    }

    #[test]
    fn release_port() {
        let r = registry();
        let p = r.acquire_port(9200, 9201).unwrap(); // only 1 port in range
        assert!(r.acquire_port(9200, 9201).is_err()); // range is exhausted
        r.release_port(p).unwrap();
        assert!(r.acquire_port(9200, 9201).is_ok()); // port is available again
    }

    #[test]
    fn release_port_missing() {
        let r = registry();
        let err = r.release_port(7777).unwrap_err();
        assert!(matches!(err, Error::NotFound));
    }
}
