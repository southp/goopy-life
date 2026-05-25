use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

use crate::goopy::Goopy;
use crate::shared_types::*;
use super::GoopyRegistry;

/// SQLite-backed implementation of [`GoopyRegistry`].
///
/// The database is opened (or created) at `db_path`.  Pass `":memory:"` to get
/// an in-process ephemeral store suitable for tests or the CLI sandbox mode.
pub struct SqliteStore {
    conn: std::sync::Mutex<Connection>,
}

impl SqliteStore {
    /// Open (or create) the SQLite database at `db_path` and run the schema
    /// migration to ensure the required tables exist.
    pub fn new(db_path: &Path) -> Result<Self, Error> {
        let conn = Connection::open(db_path)
            .map_err(|e| Error::Other(format!("sqlite open failed: {e}")))?;

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS goopies (
                slug             TEXT    PRIMARY KEY,
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
        .map_err(|e| Error::Other(format!("schema creation failed: {e}")))?;

        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
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
impl GoopyRegistry for SqliteStore {
    fn save(&self, gp: &Goopy) -> Result<(), Error> {
        let conn = self.conn.lock().unwrap();

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
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(Error::AlreadyExists)
            }
            Err(e) => Err(Error::Other(format!("save failed: {e}"))),
        }
    }

    fn load(&self, slug: &String) -> Result<Option<Goopy>, Error> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT slug, life_in_days, created_at, status, working_dir,
                        port, provisioner_kind, service_version
                 FROM goopies WHERE slug = ?1",
            )
            .map_err(|e| Error::Other(format!("prepare failed: {e}")))?;

        let mut rows = stmt
            .query(params![slug])
            .map_err(|e| Error::Other(format!("query failed: {e}")))?;

        match rows.next().map_err(|e| Error::Other(format!("row error: {e}")))? {
            None => Ok(None),
            Some(row) => {
                let gp = row_to_goopy(
                    row.get(0).map_err(|e| Error::Other(e.to_string()))?,
                    row.get(1).map_err(|e| Error::Other(e.to_string()))?,
                    row.get(2).map_err(|e| Error::Other(e.to_string()))?,
                    row.get(3).map_err(|e| Error::Other(e.to_string()))?,
                    row.get(4).map_err(|e| Error::Other(e.to_string()))?,
                    row.get(5).map_err(|e| Error::Other(e.to_string()))?,
                    row.get(6).map_err(|e| Error::Other(e.to_string()))?,
                    row.get(7).map_err(|e| Error::Other(e.to_string()))?,
                )?;
                Ok(Some(gp))
            }
        }
    }

    fn update_status(&self, slug: &String, new_status: Status) -> Result<(), Error> {
        let conn = self.conn.lock().unwrap();

        let n = conn
            .execute(
                "UPDATE goopies SET status = ?1 WHERE slug = ?2",
                params![new_status.to_string(), slug],
            )
            .map_err(|e| Error::Other(format!("update_status failed: {e}")))?;

        if n == 0 {
            Err(Error::NotFound)
        } else {
            Ok(())
        }
    }

    fn delete(&self, slug: &String) -> Result<(), Error> {
        let conn = self.conn.lock().unwrap();

        let n = conn
            .execute("DELETE FROM goopies WHERE slug = ?1", params![slug])
            .map_err(|e| Error::Other(format!("delete failed: {e}")))?;

        if n == 0 {
            Err(Error::NotFound)
        } else {
            Ok(())
        }
    }

    fn list(&self) -> Result<Vec<Goopy>, Error> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT slug, life_in_days, created_at, status, working_dir,
                        port, provisioner_kind, service_version
                 FROM goopies ORDER BY created_at",
            )
            .map_err(|e| Error::Other(format!("prepare failed: {e}")))?;

        let goopies = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .map_err(|e| Error::Other(format!("list query failed: {e}")))?
            .map(|r| {
                let (slug, life_in_days, created_at, status, working_dir, port, pk, sv) =
                    r.map_err(|e| Error::Other(format!("row error: {e}")))?;
                row_to_goopy(slug, life_in_days, created_at, status, working_dir, port, pk, sv)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(goopies)
    }

    fn acquire_port(&self, range_start: u32, range_end: u32) -> Result<u32, Error> {
        let conn = self.conn.lock().unwrap();

        // Use a transaction so the read+write is atomic.
        // We iterate candidate ports in order and try to INSERT each one;
        // the first successful INSERT wins.
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| Error::Other(format!("transaction failed: {e}")))?;

        for port in range_start..range_end {
            let result = tx.execute(
                "INSERT OR IGNORE INTO allocated_ports (port) VALUES (?1)",
                params![port as i64],
            );

            match result {
                Ok(1) => {
                    tx.commit()
                        .map_err(|e| Error::Other(format!("commit failed: {e}")))?;
                    return Ok(port);
                }
                Ok(_) => {
                    // Row already existed (OR IGNORE silently skipped it)
                    continue;
                }
                Err(e) => {
                    return Err(Error::Other(format!("acquire_port insert failed: {e}")));
                }
            }
        }

        Err(Error::Other("port range exhausted".into()))
    }

    fn release_port(&self, port: u32) -> Result<(), Error> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "DELETE FROM allocated_ports WHERE port = ?1",
            params![port as i64],
        )
        .map_err(|e| Error::Other(format!("release_port failed: {e}")))?;

        Ok(())
    }
}
