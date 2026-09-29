use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::GoopyRegistry;
use crate::goopy::Goopy;
use crate::instance_event::{EventOutcome, EventPhase, InstanceEvent};
use crate::shared_types::*;
use crate::usage_stats::{DailyUsage, UsageCounter, UsageCounts, UsageStats};

/// Ordered list of migration steps.
///
/// Each entry is `(target_version, sql)`.  The runner applies every step whose
/// `target_version` is greater than the current `user_version`, then sets
/// `user_version` to `target_version` after each successful step.
///
/// Version 1 establishes the pre-versioning schema as the baseline: it creates
/// `goopies` and `allocated_ports` outright.  `IF NOT EXISTS` keeps it tolerant
/// of databases created before versioning existed, whose `user_version` is
/// still 0 even though the tables are already present.
///
/// Version 2 adds `instance_events` (#118).  Version 3 adds
/// `goopies.build_sha` (#119).  Version 4 adds the usage counters,
/// `usage_daily` and `usage_totals` (#172).  A fresh database is fully
/// initialised by walking every step in order, so new steps append here rather
/// than editing an existing one — an already-migrated database never re-runs a
/// step it has passed.
///
/// Each step runs inside a transaction (see [`migrate`]), which constrains what
/// its SQL may contain: no explicit `BEGIN`/`COMMIT`, and no statement SQLite
/// forbids inside a transaction — notably `VACUUM` and `PRAGMA journal_mode`.
const MIGRATIONS: &[(u32, &str)] = &[
    (
        1,
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
        port INTEGER PRIMARY KEY,
        slug TEXT    NOT NULL UNIQUE
    );
    ",
    ),
    (
        2,
        // Append-only record of what happened to an instance, kept after the
        // instance is gone (#118).
        //
        // No foreign key to `goopies` on purpose: the row this describes is
        // routinely deleted, and outliving it is the whole point. `slug` is
        // therefore a plain column and may name an instance that no longer
        // exists — or, once a slug is reused, more than one instance over
        // time. `occurred_at` is what separates them.
        //
        // The `occurred_at` index serves both readers: newest-first listing,
        // and the sweep's retention delete.
        "
    CREATE TABLE IF NOT EXISTS instance_events (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        slug        TEXT    NOT NULL,
        occurred_at TEXT    NOT NULL,
        phase       TEXT    NOT NULL,
        outcome     TEXT    NOT NULL,
        code        TEXT    NOT NULL,
        detail      TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_instance_events_slug
        ON instance_events(slug);
    CREATE INDEX IF NOT EXISTS idx_instance_events_occurred_at
        ON instance_events(occurred_at);
    ",
    ),
    (
        3,
        // The gl commit that provisioned each instance (#119), alongside
        // `service_version`, which is the provisioned service's own version
        // and stays exactly that.
        //
        // Nullable with no default on purpose: rows that predate this step
        // were made by a binary that recorded nothing, and NULL says so.
        // Backfilling `'unknown'` would claim an unstamped build made them.
        // Not idempotent — `ADD COLUMN` fails if re-run — which the
        // `user_version` guard in [`migrate`] makes safe.
        "
    ALTER TABLE goopies ADD COLUMN build_sha TEXT;
    ",
    ),
    (
        4,
        // Usage counters (#172): a row per UTC day plus a single all-time row.
        //
        // Not derived from `instance_events`, for two reasons. It has no
        // success row — only failures and reaps are recorded — so it cannot
        // count what was provisioned. And it is pruned at
        // `event_retention_days`, which would erase the all-time figure.
        //
        // Storage grows with the days kept, not with the instances: the sweep
        // prunes `usage_daily` to `stats_retention_days`, and `usage_totals` is
        // one row forever. `CHECK (id = 1)` makes a second totals row
        // impossible rather than merely unexpected.
        //
        // `day` is `YYYY-MM-DD`, so TEXT order is date order, which is what
        // the retention delete's `day < ?` relies on.
        //
        // `IF NOT EXISTS` and `OR IGNORE`, as in steps 1 and 2, keep this
        // tolerant of a database whose `user_version` was reset under tables
        // that are still there.
        "
    CREATE TABLE IF NOT EXISTS usage_daily (
        day         TEXT    PRIMARY KEY,
        provisioned INTEGER NOT NULL DEFAULT 0,
        failed      INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE IF NOT EXISTS usage_totals (
        id          INTEGER PRIMARY KEY CHECK (id = 1),
        provisioned INTEGER NOT NULL,
        failed      INTEGER NOT NULL
    );

    INSERT OR IGNORE INTO usage_totals (id, provisioned, failed) VALUES (1, 0, 0);
    ",
    ),
];

/// The latest schema version understood by this build.
///
/// Derived from [`MIGRATIONS`] rather than hand-maintained, so adding a step
/// cannot leave the two out of sync.
const LATEST_VERSION: u32 = MIGRATIONS[MIGRATIONS.len() - 1].0;

/// Read the schema version SQLite records in the database header.
fn read_user_version(conn: &Connection) -> Result<u32, Error> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(Error::SchemaMigration)
}

/// Apply any pending schema migrations from `migrations` and bump
/// `PRAGMA user_version` after each successful step.
///
/// Steps are applied in order; each step is skipped when the DB's current
/// `user_version` is already at or above the step's target version.  The
/// function is idempotent: calling it on an up-to-date database is a no-op.
///
/// `migrations` is a parameter rather than a direct reference to [`MIGRATIONS`]
/// so the runner can be tested against a multi-step chain independently of the
/// production schema.
///
/// # Atomicity
///
/// Each step's DDL and its `user_version` bump commit together in one
/// transaction, so a step either lands completely or not at all.  Applying them
/// as separate autocommit statements would let a crash in between leave the DB
/// DDL-applied-but-not-version-bumped, and the next start would re-run that
/// step — a hard failure for any step that is not idempotent.  Both SQLite DDL
/// and `user_version` are transactional, so the rollback is complete.
///
/// This is why migration SQL must not open its own transaction; see
/// [`MIGRATIONS`].
///
/// # Errors
///
/// Returns [`Error::SchemaVersionTooNew`] when the DB's `user_version` exceeds
/// the highest version in `migrations` — i.e. the database was written by a
/// newer build than this one.  The check runs before any DDL is issued, so a
/// database we cannot interpret is left untouched.
fn migrate(conn: &mut Connection, migrations: &[(u32, &str)]) -> Result<(), Error> {
    let latest = migrations.last().map(|&(target, _)| target).unwrap_or(0);

    let mut current = read_user_version(conn)?;

    if current > latest {
        return Err(Error::SchemaVersionTooNew {
            found: current,
            supported: latest,
        });
    }

    for &(target, sql) in migrations {
        if current >= target {
            continue;
        }

        // `Immediate` takes the write lock up front rather than on the first
        // write, so a concurrent opener waits out `busy_timeout` instead of
        // failing a lock upgrade with SQLITE_BUSY.
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(Error::SchemaMigration)?;

        // `current` was read before the loop; re-read under the write lock in
        // case another process (gl-serv and gl-cli share the same DB file)
        // applied this step while we were waiting for it.
        let actual = read_user_version(&tx)?;
        if actual >= target {
            // Dropping `tx` rolls back, but nothing was applied to roll back.
            current = actual;
            continue;
        }

        tx.execute_batch(sql).map_err(Error::SchemaMigration)?;

        // `PRAGMA user_version = <n>` does not accept bound parameters, so we
        // format the integer directly.  `target` is a `u32` literal so there
        // is no injection risk here.
        tx.execute_batch(&format!("PRAGMA user_version = {target};"))
            .map_err(Error::SchemaMigration)?;

        tx.commit().map_err(Error::SchemaMigration)?;

        current = target;
    }

    Ok(())
}

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

        let manager = SqliteConnectionManager::file(db_path).with_init(|conn| {
            conn.execute_batch("PRAGMA busy_timeout = 5000;")?;
            Ok(())
        });

        // Only the literal ":memory:" path is recognised as in-memory; URI-form
        // in-memory databases (file::memory:?cache=shared) are not supported.
        let pool_size = if is_memory { 1 } else { 8 };

        let pool = Pool::builder()
            .max_size(pool_size)
            .build(manager)
            .map_err(|e| Error::Registry {
                context: "pool build",
                source: e.into(),
            })?;

        let mut conn = pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        if !is_memory {
            let mode: String = conn
                .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
                .map_err(|e| Error::Registry {
                    context: "wal mode check",
                    source: e.into(),
                })?;
            if mode != "wal" {
                return Err(Error::Registry {
                    context: "wal mode check",
                    source: RegistrySource::WalModeUnavailable(mode),
                });
            }
        }

        // Both file-backed and in-memory databases go through the same runner:
        // a fresh `:memory:` connection reports `user_version = 0`, so every
        // step is walked in order.  Special-casing in-memory here would break
        // as soon as a second migration step is added.
        migrate(&mut conn, MIGRATIONS)?;

        tracing::debug!(
            db = %db_path.display(),
            schema_version = LATEST_VERSION,
            "registry schema ready"
        );

        Ok(Self { pool })
    }

    /// Run `body` inside one `IMMEDIATE` write transaction, committing only if
    /// it returns `Ok`.
    ///
    /// `Immediate` takes the write lock up front so a concurrent opener —
    /// gl-serv and gl-cli share the file — waits out `busy_timeout` instead of
    /// failing a lock upgrade with `SQLITE_BUSY`, the same reason the migration
    /// runner uses it.
    ///
    /// `context` names the operation for the [`Error::Registry`] a failed
    /// begin or commit carries.
    fn in_write_transaction<T>(
        &self,
        context: &'static str,
        body: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| Error::Registry {
                context,
                source: e.into(),
            })?;

        let out = body(&tx)?;

        tx.commit().map_err(|e| Error::Registry {
            context,
            source: e.into(),
        })?;

        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Row → Goopy conversion helper
// ---------------------------------------------------------------------------
#[allow(clippy::too_many_arguments)]
fn parse_row(
    slug: String,
    life_in_days: i64,
    created_at_str: String,
    status_str: String,
    working_dir_str: String,
    port: i64,
    provisioner_kind_str: String,
    service_version: String,
    build_sha: Option<String>,
) -> Result<Goopy, Error> {
    let created_at = created_at_str.parse::<DateTime<Utc>>().map_err(|e| {
        tracing::error!(
            slug = %slug,
            field = "created_at",
            value = %created_at_str,
            error = %e,
            "row parse failed"
        );
        Error::RowParse {
            slug: slug.clone(),
            field: "created_at",
            value: created_at_str.clone(),
        }
    })?;

    let status = Status::from_str(&status_str).map_err(|_| {
        tracing::error!(slug = %slug, field = "status", value = %status_str, "row parse failed");
        Error::RowParse {
            slug: slug.clone(),
            field: "status",
            value: status_str.clone(),
        }
    })?;

    let provisioner_kind = ProvisionerKind::from_str(&provisioner_kind_str).map_err(|_| {
        tracing::error!(
            slug = %slug,
            field = "provisioner_kind",
            value = %provisioner_kind_str,
            "row parse failed"
        );
        Error::RowParse {
            slug: slug.clone(),
            field: "provisioner_kind",
            value: provisioner_kind_str.clone(),
        }
    })?;

    Ok(Goopy {
        slug,
        life_in_days: life_in_days as i32,
        created_at,
        status,
        working_dir: PathBuf::from(working_dir_str),
        port: port as u32,
        provisioner_kind,
        service_version,
        build_sha,
    })
}

// ---------------------------------------------------------------------------
// Statement helpers
// ---------------------------------------------------------------------------
//
// These take a bare `&Connection` so the same SQL serves both the autocommit
// methods and the `save_within_caps` transaction. Keeping one copy of each
// statement is what guarantees the cap enforced under the write lock is the
// same one `count_*` reports.

/// Statuses that mean an instance is resident in RAM. See
/// [`GoopyRegistry::count_active`] for why `Despawning` is one of them.
const ACTIVE_STATUSES: &str = "('Spawning', 'Done', 'Despawning')";

/// Count every row in `goopies`, regardless of status.
fn count_provisioned_in(conn: &Connection) -> Result<u32, Error> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM goopies", [], |row| row.get(0))
        .map_err(|e| Error::Registry {
            context: "count provisioned",
            source: e.into(),
        })?;

    Ok(count as u32)
}

/// Count rows whose status is in [`ACTIVE_STATUSES`].
fn count_active_in(conn: &Connection) -> Result<u32, Error> {
    let sql = format!("SELECT COUNT(*) FROM goopies WHERE status IN {ACTIVE_STATUSES}");
    let count: i64 = conn
        .query_row(&sql, [], |row| row.get(0))
        .map_err(|e| Error::Registry {
            context: "count active",
            source: e.into(),
        })?;

    Ok(count as u32)
}

/// Set `slug`'s status, reporting a missing row as [`Error::NotFound`].
fn set_status_in(conn: &Connection, slug: &str, new_status: Status) -> Result<(), Error> {
    let n = conn
        .execute(
            "UPDATE goopies SET status = ?1 WHERE slug = ?2",
            params![new_status.to_string(), slug],
        )
        .map_err(|e| Error::Registry {
            context: "update status",
            source: e.into(),
        })?;

    if n == 0 {
        tracing::error!(slug = %slug, "update_status: not found");
        return Err(Error::NotFound);
    }

    tracing::debug!(slug = %slug, status = %new_status, "updated status");
    Ok(())
}

/// Remove `slug` from `goopies`, reporting a missing row as
/// [`Error::NotFound`].
fn delete_goopy_in(conn: &Connection, slug: &str) -> Result<(), Error> {
    let n = conn
        .execute("DELETE FROM goopies WHERE slug = ?1", params![slug])
        .map_err(|e| Error::Registry {
            context: "delete",
            source: e.into(),
        })?;

    if n == 0 {
        tracing::error!(slug = %slug, "delete: not found");
        return Err(Error::NotFound);
    }

    tracing::debug!(slug = %slug, "deleted goopy");
    Ok(())
}

/// Append one row to `instance_events`.
///
/// Only ever an `INSERT`: nothing in this module updates or deletes an
/// individual event. The one deletion is retention, which goes by age.
fn insert_event_in(conn: &Connection, event: &InstanceEvent) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO instance_events
         (slug, occurred_at, phase, outcome, code, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.slug,
            event.occurred_at.to_rfc3339(),
            event.phase.to_string(),
            event.outcome.to_string(),
            event.code,
            event.detail,
        ],
    )
    .map_err(|e| Error::Registry {
        context: "record event",
        source: e.into(),
    })?;

    tracing::debug!(
        slug = %event.slug,
        phase = %event.phase,
        outcome = %event.outcome,
        code = %event.code,
        "recorded instance event"
    );
    Ok(())
}

/// Whether `event` says nothing the slug's newest event does not already say:
/// same phase, outcome, code and detail. The timestamp is deliberately not
/// compared, because a retry that fails the same way differs only in that.
///
/// Uses the same ordering as `events()`, so "newest" means what a reader of
/// `gl-cli events` sees at the top.
fn newest_event_repeats(conn: &Connection, event: &InstanceEvent) -> Result<bool, Error> {
    let newest = conn
        .query_row(
            "SELECT phase, outcome, code, detail
             FROM instance_events
             WHERE slug = ?1
             ORDER BY occurred_at DESC, id DESC
             LIMIT 1",
            params![event.slug],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|e| Error::Registry {
            context: "newest event",
            source: e.into(),
        })?;

    Ok(newest.is_some_and(|(phase, outcome, code, detail)| {
        phase == event.phase.to_string()
            && outcome == event.outcome.to_string()
            && code == event.code
            && detail == event.detail
    }))
}

/// Count one `counter` against `day` and against the all-time total.
///
/// Callers run this inside the transaction that makes the counted thing true —
/// the `Done` or `Failed` status write — so the count cannot drift from the
/// rows it describes.
///
/// One literal statement pair per counter rather than a column name formatted
/// into shared SQL, so nothing is ever spliced into a statement.
///
/// The totals write is an upsert even though migration 4 seeds the row: a
/// missing totals row must not be able to fail the status write it rides with,
/// which would leave a working instance stuck in `Spawning` over a statistic.
fn bump_usage_in(conn: &Connection, day: NaiveDate, counter: UsageCounter) -> Result<(), Error> {
    let (daily_sql, totals_sql) = match counter {
        UsageCounter::Provisioned => (
            "INSERT INTO usage_daily (day, provisioned) VALUES (?1, 1)
             ON CONFLICT(day) DO UPDATE SET provisioned = provisioned + 1",
            "INSERT INTO usage_totals (id, provisioned, failed) VALUES (1, 1, 0)
             ON CONFLICT(id) DO UPDATE SET provisioned = provisioned + 1",
        ),
        UsageCounter::Failed => (
            "INSERT INTO usage_daily (day, failed) VALUES (?1, 1)
             ON CONFLICT(day) DO UPDATE SET failed = failed + 1",
            "INSERT INTO usage_totals (id, provisioned, failed) VALUES (1, 0, 1)
             ON CONFLICT(id) DO UPDATE SET failed = failed + 1",
        ),
    };

    conn.execute(daily_sql, params![day.to_string()])
        .map_err(|e| Error::Registry {
            context: "count usage (daily)",
            source: e.into(),
        })?;
    conn.execute(totals_sql, []).map_err(|e| Error::Registry {
        context: "count usage (total)",
        source: e.into(),
    })?;

    tracing::debug!(%day, ?counter, "counted usage");
    Ok(())
}

/// Today, as the UTC calendar day the usage counters bucket by.
fn utc_today() -> NaiveDate {
    Utc::now().date_naive()
}

/// Rebuild an [`InstanceEvent`] from its stored columns.
fn parse_event_row(
    slug: String,
    occurred_at_str: String,
    phase_str: String,
    outcome_str: String,
    code: String,
    detail: Option<String>,
) -> Result<InstanceEvent, Error> {
    let occurred_at = occurred_at_str.parse::<DateTime<Utc>>().map_err(|e| {
        tracing::error!(
            slug = %slug,
            field = "occurred_at",
            value = %occurred_at_str,
            error = %e,
            "event row parse failed"
        );
        Error::RowParse {
            slug: slug.clone(),
            field: "occurred_at",
            value: occurred_at_str.clone(),
        }
    })?;

    let phase = EventPhase::from_str(&phase_str).map_err(|_| {
        tracing::error!(slug = %slug, field = "phase", value = %phase_str, "event row parse failed");
        Error::RowParse {
            slug: slug.clone(),
            field: "phase",
            value: phase_str.clone(),
        }
    })?;

    let outcome = EventOutcome::from_str(&outcome_str).map_err(|_| {
        tracing::error!(
            slug = %slug,
            field = "outcome",
            value = %outcome_str,
            "event row parse failed"
        );
        Error::RowParse {
            slug: slug.clone(),
            field: "outcome",
            value: outcome_str.clone(),
        }
    })?;

    Ok(InstanceEvent {
        slug,
        occurred_at,
        phase,
        outcome,
        code,
        detail,
    })
}

/// Insert `gp`, mapping a UNIQUE violation on `slug` to [`Error::AlreadyExists`]
/// so callers can retry with a fresh slug.
fn insert_goopy(conn: &Connection, gp: &Goopy) -> Result<(), Error> {
    let result = conn.execute(
        "INSERT OR FAIL INTO goopies
         (slug, life_in_days, created_at, status, working_dir, port,
          provisioner_kind, service_version, build_sha)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            gp.slug,
            gp.life_in_days as i64,
            gp.created_at.to_rfc3339(),
            gp.status.to_string(),
            gp.working_dir.to_string_lossy().as_ref(),
            gp.port as i64,
            gp.provisioner_kind.to_string(),
            gp.service_version,
            gp.build_sha,
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
            Err(Error::Registry {
                context: "save",
                source: e.into(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// GoopyRegistry impl
// ---------------------------------------------------------------------------
impl GoopyRegistry for SqliteRegistry {
    #[tracing::instrument(skip(self))]
    fn save(&self, gp: &Goopy) -> Result<(), Error> {
        let conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        insert_goopy(&conn, gp)
    }

    #[tracing::instrument(skip(self))]
    fn load(&self, slug: &str) -> Result<Option<Goopy>, Error> {
        let conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        let result = conn.query_row(
            "SELECT slug, life_in_days, created_at, status, working_dir,
                    port, provisioner_kind, service_version, build_sha
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
                    row.get::<_, Option<String>>("build_sha")?,
                ))
            },
        );

        match result {
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(Error::Registry {
                context: "load",
                source: e.into(),
            }),
            Ok((
                slug,
                life_in_days,
                created_at_str,
                status_str,
                working_dir_str,
                port,
                provisioner_kind_str,
                service_version,
                build_sha,
            )) => {
                let gp = parse_row(
                    slug,
                    life_in_days,
                    created_at_str,
                    status_str,
                    working_dir_str,
                    port,
                    provisioner_kind_str,
                    service_version,
                    build_sha,
                )?;
                tracing::debug!(slug = %gp.slug, "loaded goopy");
                Ok(Some(gp))
            }
        }
    }

    #[tracing::instrument(skip(self))]
    fn update_status(&self, slug: &str, new_status: Status) -> Result<(), Error> {
        let conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        set_status_in(&conn, slug, new_status)
    }

    #[tracing::instrument(skip(self))]
    fn complete_spawn(&self, slug: &str) -> Result<(), Error> {
        self.in_write_transaction("complete_spawn", |tx| {
            set_status_in(tx, slug, Status::Done)?;
            bump_usage_in(tx, utc_today(), UsageCounter::Provisioned)
        })
    }

    #[tracing::instrument(skip(self))]
    fn delete(&self, slug: &str) -> Result<(), Error> {
        let conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        delete_goopy_in(&conn, slug)
    }

    #[tracing::instrument(skip(self))]
    fn list(&self) -> Result<Vec<Goopy>, Error> {
        let conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT slug, life_in_days, created_at, status, working_dir,
                        port, provisioner_kind, service_version, build_sha
                 FROM goopies ORDER BY created_at",
            )
            .map_err(|e| Error::Registry {
                context: "list prepare",
                source: e.into(),
            })?;

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
                    row.get::<_, Option<String>>("build_sha")?,
                ))
            })
            .map_err(|e| Error::Registry {
                context: "list query",
                source: e.into(),
            })?
            .map(|r| {
                let (
                    slug,
                    life_in_days,
                    created_at_str,
                    status_str,
                    working_dir_str,
                    port,
                    provisioner_kind_str,
                    service_version,
                    build_sha,
                ) = r.map_err(|e| Error::Registry {
                    context: "list row",
                    source: e.into(),
                })?;
                parse_row(
                    slug,
                    life_in_days,
                    created_at_str,
                    status_str,
                    working_dir_str,
                    port,
                    provisioner_kind_str,
                    service_version,
                    build_sha,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(goopies)
    }

    #[tracing::instrument(skip(self))]
    fn acquire_port(&self, slug: &str, range_start: u32, range_end: u32) -> Result<u32, Error> {
        let mut conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        let tx = conn.transaction().map_err(|e| Error::Registry {
            context: "transaction",
            source: e.into(),
        })?;

        // O(n) scan across the range. Acceptable for ranges of a few hundred ports;
        // for larger ranges a single-query approach (SELECT MIN unused port) is preferable.
        for port in range_start..range_end {
            let result = tx.execute(
                "INSERT OR IGNORE INTO allocated_ports (port, slug) VALUES (?1, ?2)",
                params![port as i64, slug],
            );

            match result {
                Ok(1) => {
                    tx.commit().map_err(|e| Error::Registry {
                        context: "commit",
                        source: e.into(),
                    })?;
                    tracing::debug!(port = port, "acquired port");
                    return Ok(port);
                }
                Ok(_) => {
                    // Row already existed (OR IGNORE silently skipped it)
                    continue;
                }
                Err(e) => {
                    return Err(Error::Registry {
                        context: "acquire port",
                        source: e.into(),
                    });
                }
            }
        }

        tracing::error!("port range {range_start}..{range_end} exhausted");
        Err(Error::PortExhausted)
    }

    #[tracing::instrument(skip(self))]
    fn release_port(&self, port: u32) -> Result<(), Error> {
        let conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        let n = conn
            .execute(
                "DELETE FROM allocated_ports WHERE port = ?1",
                params![port as i64],
            )
            .map_err(|e| Error::Registry {
                context: "release port",
                source: e.into(),
            })?;

        if n == 0 {
            return Err(Error::NotFound);
        }

        tracing::debug!(port = port, "released port");
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn count_provisioned(&self) -> Result<u32, Error> {
        let conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        count_provisioned_in(&conn)
    }

    #[tracing::instrument(skip(self))]
    fn count_active(&self) -> Result<u32, Error> {
        let conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        count_active_in(&conn)
    }

    #[tracing::instrument(skip(self))]
    fn save_within_caps(
        &self,
        gp: &Goopy,
        max_provisioned: u32,
        max_active: u32,
    ) -> Result<(), Error> {
        let mut conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        // `Immediate` takes the write lock before the first read, so the counts
        // below and the insert that follows see one consistent snapshot. With a
        // deferred transaction two spawners could both read the same free slot
        // and only collide on write — exactly the overshoot this exists to
        // prevent. Contention waits out the pool's `busy_timeout`.
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| Error::Registry {
                context: "begin save_within_caps transaction",
                source: e.into(),
            })?;

        let provisioned = count_provisioned_in(&tx)?;
        if provisioned >= max_provisioned {
            tracing::warn!(
                provisioned,
                max_provisioned,
                slug = %gp.slug,
                "spawn refused: max_provisioned cap reached"
            );
            return Err(Error::CapacityFull {
                kind: CapacityKind::Provisioned,
            });
        }

        let active = count_active_in(&tx)?;
        if active >= max_active {
            tracing::warn!(
                active,
                max_active,
                slug = %gp.slug,
                "spawn refused: max_active cap reached"
            );
            return Err(Error::CapacityFull {
                kind: CapacityKind::Active,
            });
        }

        insert_goopy(&tx, gp)?;

        tx.commit().map_err(|e| Error::Registry {
            context: "commit save_within_caps transaction",
            source: e.into(),
        })
    }

    // -- the instance event log (#118) ------------------------------------

    #[tracing::instrument(skip(self))]
    fn record_event(&self, event: &InstanceEvent) -> Result<(), Error> {
        let conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        insert_event_in(&conn, event)
    }

    #[tracing::instrument(skip(self))]
    fn fail_with_event(&self, slug: &str, event: &InstanceEvent) -> Result<(), Error> {
        self.in_write_transaction("fail_with_event", |tx| {
            set_status_in(tx, slug, Status::Failed)?;
            // The sweep retries every `Failed` row, so an instance that cannot
            // be torn down fails the same way on every run. One row per reason
            // keeps `gl-cli events` about what is new rather than about the
            // one slug that is stuck.
            if newest_event_repeats(tx, event)? {
                return Ok(());
            }
            insert_event_in(tx, event)?;
            // Counted only past the duplicate check above. A spawn fails once
            // per instance, so a spawn event is never in practice a repeat —
            // but if one ever were, it would be the same failure seen twice,
            // and counting it twice would inflate the figure. Despawn and
            // sweep failures are cleanup problems, not failed provisions.
            if event.phase == EventPhase::Spawn {
                bump_usage_in(tx, utc_today(), UsageCounter::Failed)?;
            }
            Ok(())
        })
    }

    #[tracing::instrument(skip(self))]
    fn delete_with_event(&self, slug: &str, event: &InstanceEvent) -> Result<(), Error> {
        self.in_write_transaction("delete_with_event", |tx| {
            delete_goopy_in(tx, slug)?;
            insert_event_in(tx, event)
        })
    }

    #[tracing::instrument(skip(self))]
    fn prune_events_before(&self, cutoff: DateTime<Utc>) -> Result<u32, Error> {
        let conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        // Both sides of the comparison are RFC 3339 with a `+00:00` offset, so
        // the lexicographic comparison SQLite does on TEXT is a chronological
        // one. That is the same assumption `ORDER BY created_at` has always
        // made about `goopies`.
        let n = conn
            .execute(
                "DELETE FROM instance_events WHERE occurred_at < ?1",
                params![cutoff.to_rfc3339()],
            )
            .map_err(|e| Error::Registry {
                context: "prune events",
                source: e.into(),
            })?;

        Ok(n as u32)
    }

    #[tracing::instrument(skip(self))]
    fn events(&self, slug: Option<&str>, limit: u32) -> Result<Vec<InstanceEvent>, Error> {
        let conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        // `id DESC` breaks ties: two events for one instance can land in the
        // same RFC 3339 instant, and a teardown's order — reaped, then the
        // port release that failed after it — is the part worth reading.
        let mut stmt = conn
            .prepare(
                "SELECT slug, occurred_at, phase, outcome, code, detail
                 FROM instance_events
                 WHERE ?1 IS NULL OR slug = ?1
                 ORDER BY occurred_at DESC, id DESC
                 LIMIT ?2",
            )
            .map_err(|e| Error::Registry {
                context: "events prepare",
                source: e.into(),
            })?;

        let events = stmt
            .query_map(params![slug, limit as i64], |row| {
                Ok((
                    row.get::<_, String>("slug")?,
                    row.get::<_, String>("occurred_at")?,
                    row.get::<_, String>("phase")?,
                    row.get::<_, String>("outcome")?,
                    row.get::<_, String>("code")?,
                    row.get::<_, Option<String>>("detail")?,
                ))
            })
            .map_err(|e| Error::Registry {
                context: "events query",
                source: e.into(),
            })?
            .map(|r| {
                let (slug, occurred_at, phase, outcome, code, detail) =
                    r.map_err(|e| Error::Registry {
                        context: "events row",
                        source: e.into(),
                    })?;
                parse_event_row(slug, occurred_at, phase, outcome, code, detail)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(events)
    }

    // -- usage counters (#172) --------------------------------------------

    #[tracing::instrument(skip(self))]
    fn usage_stats(&self, today: NaiveDate) -> Result<UsageStats, Error> {
        let mut conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        // One read transaction so the total and the daily rows come from the
        // same snapshot: a spawn completing between two autocommit reads would
        // otherwise show up in one and not the other.
        let tx = conn.transaction().map_err(|e| Error::Registry {
            context: "usage stats",
            source: e.into(),
        })?;

        // Migration 4 seeds the row, but reading its absence as zero keeps a
        // hand-damaged table from turning `GET /stats` into a 500.
        let all_time = tx
            .query_row(
                "SELECT provisioned, failed FROM usage_totals WHERE id = 1",
                [],
                |row| {
                    Ok(UsageCounts {
                        provisioned: row.get::<_, i64>(0)? as u64,
                        failed: row.get::<_, i64>(1)? as u64,
                    })
                },
            )
            .optional()
            .map_err(|e| Error::Registry {
                context: "usage totals",
                source: e.into(),
            })?
            .unwrap_or_default();

        let mut stmt = tx
            .prepare("SELECT day, provisioned, failed FROM usage_daily")
            .map_err(|e| Error::Registry {
                context: "usage daily prepare",
                source: e.into(),
            })?;

        let daily = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| Error::Registry {
                context: "usage daily query",
                source: e.into(),
            })?
            .map(|r| {
                let (day_str, provisioned, failed) = r.map_err(|e| Error::Registry {
                    context: "usage daily row",
                    source: e.into(),
                })?;
                let day = day_str.parse::<NaiveDate>().map_err(|_| {
                    tracing::error!(field = "day", value = %day_str, "usage row parse failed");
                    Error::RowParse {
                        slug: String::new(),
                        field: "day",
                        value: day_str.clone(),
                    }
                })?;
                Ok(DailyUsage {
                    day,
                    counts: UsageCounts {
                        provisioned: provisioned as u64,
                        failed: failed as u64,
                    },
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        Ok(UsageStats::from_rows(all_time, daily, today))
    }

    #[tracing::instrument(skip(self))]
    fn prune_usage_before(&self, day: NaiveDate) -> Result<u32, Error> {
        let conn = self.pool.get().map_err(|e| Error::Registry {
            context: "pool get",
            source: e.into(),
        })?;

        // `YYYY-MM-DD` on both sides, so TEXT comparison is date comparison.
        // `usage_totals` is deliberately not touched: pruning the window must
        // never shrink the all-time figure.
        let n = conn
            .execute(
                "DELETE FROM usage_daily WHERE day < ?1",
                params![day.to_string()],
            )
            .map_err(|e| Error::Registry {
                context: "prune usage",
                source: e.into(),
            })?;

        Ok(n as u32)
    }
}

#[cfg(test)]
impl SqliteRegistry {
    /// Count one `counter` against an arbitrary `day`, as the real write paths
    /// do against today — for tests that need rows on dates they cannot wait
    /// for.
    pub(crate) fn bump_usage_on(&self, day: NaiveDate, counter: UsageCounter) {
        self.in_write_transaction("test bump usage", |tx| bump_usage_in(tx, day, counter))
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_goopy(slug: &str) -> Goopy {
        Goopy {
            slug: slug.to_string(),
            life_in_days: 7,
            created_at: chrono::Utc::now(),
            working_dir: PathBuf::from(format!("/tmp/{slug}")),
            port: 8080,
            status: Status::Spawning,
            provisioner_kind: ProvisionerKind::Hello,
            service_version: "0.1.0".to_string(),
            build_sha: Some("c50c932aa1b2c3d4e5f60718293a4b5c6d7e8f90".to_string()),
        }
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
    fn count_provisioned_counts_all_rows_including_failed() {
        let r = registry();
        for (slug, status) in [
            ("c-spawning", Status::Spawning),
            ("c-done", Status::Done),
            ("c-failed", Status::Failed),
            ("c-despawning", Status::Despawning),
        ] {
            let mut gp = make_goopy(slug);
            gp.status = status;
            r.save(&gp).unwrap();
        }
        // All four rows count toward the disk-bound provisioned cap.
        assert_eq!(r.count_provisioned().unwrap(), 4);
    }

    #[test]
    fn count_active_counts_resident_statuses_only() {
        let r = registry();
        for (slug, status) in [
            ("a-spawning", Status::Spawning),
            ("a-done", Status::Done),
            ("a-failed", Status::Failed),
            ("a-despawning", Status::Despawning),
        ] {
            let mut gp = make_goopy(slug);
            gp.status = status;
            r.save(&gp).unwrap();
        }
        // Spawning, Done and Despawning are all resident: a Despawning
        // instance's process stays up until the teardown thread finishes.
        // Only Failed has no process left.
        assert_eq!(r.count_active().unwrap(), 3);
    }

    #[test]
    fn save_within_caps_inserts_when_both_caps_have_room() {
        let r = registry();
        r.save_within_caps(&make_goopy("roomy-slug"), 10, 10)
            .unwrap();
        assert!(r.load("roomy-slug").unwrap().is_some());
        assert_eq!(r.count_provisioned().unwrap(), 1);
    }

    #[test]
    fn save_within_caps_rejects_when_provisioned_cap_met() {
        let r = registry();
        // A Failed row occupies a provisioned slot but no active one, so this
        // can only be the provisioned cap tripping.
        let mut occupant = make_goopy("failed-occupant");
        occupant.status = Status::Failed;
        r.save(&occupant).unwrap();

        let err = r
            .save_within_caps(&make_goopy("rejected-slug"), 1, 10)
            .unwrap_err();

        assert!(
            matches!(
                err,
                Error::CapacityFull {
                    kind: CapacityKind::Provisioned
                }
            ),
            "expected CapacityFull(Provisioned), got {err:?}"
        );
        assert!(
            r.load("rejected-slug").unwrap().is_none(),
            "a refused insert must not leave a row behind"
        );
    }

    #[test]
    fn save_within_caps_rejects_when_active_cap_met() {
        let r = registry();
        let mut occupant = make_goopy("done-occupant");
        occupant.status = Status::Done;
        r.save(&occupant).unwrap();

        // Provisioned has room (10); only the active cap of 1 is met.
        let err = r
            .save_within_caps(&make_goopy("rejected-slug"), 10, 1)
            .unwrap_err();

        assert!(
            matches!(
                err,
                Error::CapacityFull {
                    kind: CapacityKind::Active
                }
            ),
            "expected CapacityFull(Active), got {err:?}"
        );
        assert!(r.load("rejected-slug").unwrap().is_none());
    }

    #[test]
    fn save_within_caps_reports_slug_collision_not_capacity() {
        let r = registry();
        r.save(&make_goopy("taken-slug")).unwrap();

        let err = r
            .save_within_caps(&make_goopy("taken-slug"), 10, 10)
            .unwrap_err();

        assert!(
            matches!(err, Error::AlreadyExists),
            "a collision inside the cap transaction must still be retryable, got {err:?}"
        );
    }

    #[test]
    fn counts_are_zero_on_empty_registry() {
        let r = registry();
        assert_eq!(r.count_provisioned().unwrap(), 0);
        assert_eq!(r.count_active().unwrap(), 0);
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
        let p1 = r.acquire_port("slug-a", 9000, 9010).unwrap();
        let p2 = r.acquire_port("slug-b", 9000, 9010).unwrap();
        assert_ne!(p1, p2);
        assert!((9000..9010).contains(&p1));
        assert!((9000..9010).contains(&p2));
    }

    #[test]
    fn acquire_port_exhaustion() {
        let r = registry();
        r.acquire_port("slug-a", 9100, 9102).unwrap();
        r.acquire_port("slug-b", 9100, 9102).unwrap();
        let err = r.acquire_port("slug-c", 9100, 9102).unwrap_err();
        assert!(matches!(err, Error::PortExhausted));
    }

    #[test]
    fn release_port() {
        let r = registry();
        let p = r.acquire_port("slug-a", 9200, 9201).unwrap(); // only 1 port in range
        assert!(r.acquire_port("slug-b", 9200, 9201).is_err()); // range is exhausted
        r.release_port(p).unwrap();
        assert!(r.acquire_port("slug-c", 9200, 9201).is_ok()); // port is available again
    }

    #[test]
    fn release_port_missing() {
        let r = registry();
        let err = r.release_port(7777).unwrap_err();
        assert!(matches!(err, Error::NotFound));
    }

    #[test]
    fn concurrent_port_acquisition_produces_no_duplicates() {
        use std::sync::Arc;
        use std::thread;

        // WAL mode requires a file-based DB.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("concurrent.db");
        let r = Arc::new(SqliteRegistry::new(&db_path).unwrap());

        let handles: Vec<_> = (9400u32..9450)
            .map(|i| {
                let r = Arc::clone(&r);
                thread::spawn(move || r.acquire_port(&format!("concurrent-{i}"), 9400, 9450))
            })
            .collect();

        let results: Vec<u32> = handles
            .into_iter()
            .map(|h| {
                h.join()
                    .expect("thread should not panic")
                    .expect("acquire should succeed")
            })
            .collect();

        let mut sorted = results.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            results.len(),
            "all acquired ports should be unique"
        );
        assert_eq!(sorted.len(), 50, "all 50 ports should be acquired");
    }

    #[test]
    fn acquire_port_stores_slug() {
        let r = registry();
        let port = r.acquire_port("sunny-bright-fox", 9300, 9310).unwrap();
        let conn = r.pool.get().unwrap();
        let stored_slug: String = conn
            .query_row(
                "SELECT slug FROM allocated_ports WHERE port = ?1",
                params![port as i64],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_slug, "sunny-bright-fox");
    }

    #[test]
    fn parse_row_bad_created_at_returns_row_parse_error() {
        let r = registry();
        {
            let conn = r.pool.get().unwrap();
            conn.execute(
                "INSERT INTO goopies (slug, life_in_days, created_at, status, working_dir, port, provisioner_kind, service_version) \
                 VALUES ('bad-ts', 7, 'not-a-date', 'Spawning', '/tmp', 8080, 'Hello', '0.1.0')",
                [],
            )
            .unwrap();
        }
        let err = r.load("bad-ts").unwrap_err();
        assert!(
            matches!(err, Error::RowParse { field, .. } if field == "created_at"),
            "{err:?}"
        );
    }

    #[test]
    fn parse_row_bad_status_returns_row_parse_error() {
        let r = registry();
        {
            let conn = r.pool.get().unwrap();
            conn.execute(
                "INSERT INTO goopies (slug, life_in_days, created_at, status, working_dir, port, provisioner_kind, service_version) \
                 VALUES ('bad-status', 7, '2024-01-01T00:00:00Z', 'Bogus', '/tmp', 8080, 'Hello', '0.1.0')",
                [],
            )
            .unwrap();
        }
        let err = r.load("bad-status").unwrap_err();
        assert!(
            matches!(err, Error::RowParse { field, .. } if field == "status"),
            "{err:?}"
        );
    }

    #[test]
    fn parse_row_bad_provisioner_kind_returns_row_parse_error() {
        let r = registry();
        {
            let conn = r.pool.get().unwrap();
            conn.execute(
                "INSERT INTO goopies (slug, life_in_days, created_at, status, working_dir, port, provisioner_kind, service_version) \
                 VALUES ('bad-pk', 7, '2024-01-01T00:00:00Z', 'Spawning', '/tmp', 8080, 'Unknown', '0.1.0')",
                [],
            )
            .unwrap();
        }
        let err = r.load("bad-pk").unwrap_err();
        assert!(
            matches!(err, Error::RowParse { field, .. } if field == "provisioner_kind"),
            "{err:?}"
        );
    }

    // -------------------------------------------------------------------------
    // The instance event log (#118)
    // -------------------------------------------------------------------------

    fn failure(slug: &str, phase: EventPhase, err: &Error) -> InstanceEvent {
        InstanceEvent::failed(slug, phase, err)
    }

    #[test]
    fn record_event_round_trips_every_field() {
        let r = registry();
        let err = Error::Subprocess("ghost install: EACCES".into());
        let event = failure("e-round-trip", EventPhase::Spawn, &err);

        r.record_event(&event).unwrap();

        let read = r.events(Some("e-round-trip"), 10).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].slug, "e-round-trip");
        assert_eq!(read[0].phase, EventPhase::Spawn);
        assert_eq!(read[0].outcome, EventOutcome::Failed);
        assert_eq!(read[0].code, "subprocess");
        assert_eq!(
            read[0].detail.as_deref(),
            Some("subprocess error: ghost install: EACCES")
        );
        assert_eq!(read[0].occurred_at, event.occurred_at);
    }

    #[test]
    fn record_event_keeps_a_null_detail_null() {
        let r = registry();
        r.record_event(&InstanceEvent::reaped("e-null", EventPhase::Sweep))
            .unwrap();

        let read = r.events(Some("e-null"), 10).unwrap();
        assert_eq!(read[0].detail, None);
        assert_eq!(read[0].code, InstanceEvent::NO_ERROR);
    }

    /// The point of the whole table: the `goopies` row is reaped and the
    /// reason is still there afterwards.
    #[test]
    fn an_event_outlives_the_instance_it_describes() {
        let r = registry();
        r.save(&make_goopy("e-outlives")).unwrap();

        r.fail_with_event(
            "e-outlives",
            &failure("e-outlives", EventPhase::Spawn, &Error::PortExhausted),
        )
        .unwrap();
        r.delete_with_event(
            "e-outlives",
            &InstanceEvent::reaped("e-outlives", EventPhase::Sweep),
        )
        .unwrap();

        assert!(
            r.load("e-outlives").unwrap().is_none(),
            "the instance row must be gone"
        );

        let read = r.events(Some("e-outlives"), 10).unwrap();
        assert_eq!(read.len(), 2, "both events must survive the deletion");
        assert!(
            read.iter()
                .any(|e| e.outcome == EventOutcome::Failed && e.code == "port_exhausted"),
            "the reason must still be readable: {read:?}"
        );
    }

    #[test]
    fn fail_with_event_writes_the_status_and_the_event_together() {
        let r = registry();
        r.save(&make_goopy("e-fail")).unwrap();

        r.fail_with_event(
            "e-fail",
            &failure("e-fail", EventPhase::Despawn, &Error::Invalid),
        )
        .unwrap();

        assert_eq!(r.load("e-fail").unwrap().unwrap().status, Status::Failed);
        assert_eq!(r.events(Some("e-fail"), 10).unwrap().len(), 1);
    }

    #[test]
    fn fail_with_event_does_not_repeat_an_identical_reason() {
        let r = registry();
        r.save(&make_goopy("e-stuck")).unwrap();

        for _ in 0..3 {
            r.fail_with_event(
                "e-stuck",
                &failure("e-stuck", EventPhase::Sweep, &Error::Invalid),
            )
            .unwrap();
        }

        assert_eq!(r.load("e-stuck").unwrap().unwrap().status, Status::Failed);
        assert_eq!(
            r.events(Some("e-stuck"), 10).unwrap().len(),
            1,
            "the same failure, retried, is one reason and not three"
        );
    }

    #[test]
    fn fail_with_event_records_a_changed_reason() {
        let r = registry();
        r.save(&make_goopy("e-changed")).unwrap();

        r.fail_with_event(
            "e-changed",
            &failure("e-changed", EventPhase::Sweep, &Error::Invalid),
        )
        .unwrap();
        r.fail_with_event(
            "e-changed",
            &failure("e-changed", EventPhase::Sweep, &Error::NotFound),
        )
        .unwrap();
        // Back to the first reason: it is new again relative to the newest
        // event, so it is recorded — the log shows the instance flip-flopping.
        r.fail_with_event(
            "e-changed",
            &failure("e-changed", EventPhase::Sweep, &Error::Invalid),
        )
        .unwrap();

        let codes: Vec<_> = r
            .events(Some("e-changed"), 10)
            .unwrap()
            .into_iter()
            .map(|e| e.code)
            .collect();
        assert_eq!(codes, ["invalid", "not_found", "invalid"]);
    }

    /// The discriminating half of "in one transaction": the row operation
    /// fails, so the event must not be left behind on its own.
    #[test]
    fn fail_with_event_on_a_missing_row_writes_no_event() {
        let r = registry();

        let err = r
            .fail_with_event(
                "e-ghost",
                &failure("e-ghost", EventPhase::Despawn, &Error::Invalid),
            )
            .unwrap_err();

        assert!(matches!(err, Error::NotFound), "{err:?}");
        assert!(
            r.events(Some("e-ghost"), 10).unwrap().is_empty(),
            "a rolled-back status change must roll back its event too"
        );
    }

    /// The same guard on the other side: a reap that did not happen must not
    /// be recorded as one (#117).
    #[test]
    fn delete_with_event_on_a_missing_row_writes_no_event() {
        let r = registry();

        let err = r
            .delete_with_event(
                "e-never",
                &InstanceEvent::reaped("e-never", EventPhase::Sweep),
            )
            .unwrap_err();

        assert!(matches!(err, Error::NotFound), "{err:?}");
        assert!(
            r.events(Some("e-never"), 10).unwrap().is_empty(),
            "a rolled-back delete must not leave a `reaped` event"
        );
    }

    #[test]
    fn events_are_newest_first_and_respect_the_limit() {
        let r = registry();
        let base = Utc::now();
        for (i, code) in ["oldest", "middle", "newest"].iter().enumerate() {
            let mut event = InstanceEvent::reaped("e-order", EventPhase::Sweep);
            event.occurred_at = base + chrono::Duration::seconds(i as i64);
            event.detail = Some((*code).to_string());
            r.record_event(&event).unwrap();
        }

        let all = r.events(Some("e-order"), 10).unwrap();
        let order: Vec<_> = all.iter().map(|e| e.detail.clone().unwrap()).collect();
        assert_eq!(order, vec!["newest", "middle", "oldest"]);

        let capped = r.events(Some("e-order"), 2).unwrap();
        assert_eq!(capped.len(), 2, "limit must be applied");
        assert_eq!(capped[0].detail.as_deref(), Some("newest"));
    }

    #[test]
    fn events_without_a_slug_returns_every_instance() {
        let r = registry();
        r.record_event(&InstanceEvent::reaped("e-one", EventPhase::Sweep))
            .unwrap();
        r.record_event(&InstanceEvent::reaped("e-two", EventPhase::Sweep))
            .unwrap();

        assert_eq!(r.events(None, 10).unwrap().len(), 2);
        assert_eq!(r.events(Some("e-one"), 10).unwrap().len(), 1);
    }

    #[test]
    fn prune_events_before_drops_only_older_rows() {
        let r = registry();
        let now = Utc::now();

        let mut old = InstanceEvent::reaped("e-old", EventPhase::Sweep);
        old.occurred_at = now - chrono::Duration::days(40);
        r.record_event(&old).unwrap();

        let mut recent = InstanceEvent::reaped("e-recent", EventPhase::Sweep);
        recent.occurred_at = now - chrono::Duration::days(2);
        r.record_event(&recent).unwrap();

        let pruned = r
            .prune_events_before(now - chrono::Duration::days(30))
            .unwrap();

        assert_eq!(pruned, 1, "only the 40-day-old event is past the cutoff");
        let left = r.events(None, 10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].slug, "e-recent");
    }

    #[test]
    fn prune_events_before_a_cutoff_older_than_everything_removes_nothing() {
        let r = registry();
        r.record_event(&InstanceEvent::reaped("e-keep", EventPhase::Sweep))
            .unwrap();

        let pruned = r
            .prune_events_before(Utc::now() - chrono::Duration::days(365))
            .unwrap();

        assert_eq!(pruned, 0);
        assert_eq!(r.events(None, 10).unwrap().len(), 1);
    }

    // -------------------------------------------------------------------------
    // Usage counters (#172)
    // -------------------------------------------------------------------------

    /// Break the next counter write by removing the table it goes to, so a
    /// test can watch the status write it rides with roll back.
    fn break_usage_writes(r: &SqliteRegistry) {
        let conn = r.pool.get().unwrap();
        conn.execute_batch("DROP TABLE usage_daily;").unwrap();
    }

    #[test]
    fn usage_stats_on_an_empty_registry_is_all_zeros() {
        let stats = registry().usage_stats(utc_today()).unwrap();

        assert_eq!(stats.all_time, UsageCounts::new(0, 0));
        assert_eq!(stats.last_7_days, UsageCounts::new(0, 0));
        assert_eq!(stats.today, UsageCounts::new(0, 0));
        assert!(stats.daily.is_empty());
    }

    #[test]
    fn complete_spawn_marks_done_and_counts_one_provision() {
        let r = registry();
        r.save(&make_goopy("u-done")).unwrap();

        r.complete_spawn("u-done").unwrap();

        assert_eq!(r.load("u-done").unwrap().unwrap().status, Status::Done);
        let stats = r.usage_stats(utc_today()).unwrap();
        assert_eq!(stats.today, UsageCounts::new(1, 0));
        assert_eq!(stats.all_time, UsageCounts::new(1, 0));
        assert_eq!(stats.daily.len(), 1);
        assert_eq!(stats.daily[0].day, utc_today());
    }

    #[test]
    fn complete_spawn_on_a_missing_row_counts_nothing() {
        let r = registry();

        let err = r.complete_spawn("u-never").unwrap_err();

        assert!(matches!(err, Error::NotFound), "{err:?}");
        assert_eq!(
            r.usage_stats(utc_today()).unwrap().all_time,
            UsageCounts::new(0, 0)
        );
    }

    /// The acceptance criterion's "one transaction": a count that cannot be
    /// written takes the status write down with it, so `Done` never appears
    /// without being counted.
    #[test]
    fn complete_spawn_rolls_back_the_status_when_the_count_write_fails() {
        let r = registry();
        r.save(&make_goopy("u-rollback")).unwrap();
        break_usage_writes(&r);

        let err = r.complete_spawn("u-rollback").unwrap_err();

        assert!(matches!(err, Error::Registry { .. }), "{err:?}");
        assert_eq!(
            r.load("u-rollback").unwrap().unwrap().status,
            Status::Spawning,
            "the status write must roll back with the count"
        );
    }

    #[test]
    fn a_spawn_failure_counts_one_failed_provision() {
        let r = registry();
        r.save(&make_goopy("u-failed")).unwrap();

        r.fail_with_event(
            "u-failed",
            &failure("u-failed", EventPhase::Spawn, &Error::PortExhausted),
        )
        .unwrap();

        let stats = r.usage_stats(utc_today()).unwrap();
        assert_eq!(stats.today, UsageCounts::new(0, 1));
        assert_eq!(stats.all_time, UsageCounts::new(0, 1));
    }

    /// Cleanup problems are not failed provisions: the instance was served,
    /// or never got that far, and either way `provisioned`/`failed` already
    /// said so when it happened.
    #[test]
    fn despawn_and_sweep_failures_are_not_counted() {
        let r = registry();
        r.save(&make_goopy("u-cleanup")).unwrap();

        r.fail_with_event(
            "u-cleanup",
            &failure("u-cleanup", EventPhase::Despawn, &Error::Invalid),
        )
        .unwrap();
        r.fail_with_event(
            "u-cleanup",
            &failure("u-cleanup", EventPhase::Sweep, &Error::NotFound),
        )
        .unwrap();

        assert_eq!(r.events(Some("u-cleanup"), 10).unwrap().len(), 2);
        assert_eq!(
            r.usage_stats(utc_today()).unwrap().all_time,
            UsageCounts::new(0, 0)
        );
    }

    #[test]
    fn a_spawn_failure_skipped_as_a_duplicate_is_not_counted_again() {
        let r = registry();
        r.save(&make_goopy("u-dup")).unwrap();
        let event = failure("u-dup", EventPhase::Spawn, &Error::PortExhausted);

        r.fail_with_event("u-dup", &event).unwrap();
        r.fail_with_event("u-dup", &event).unwrap();

        assert_eq!(r.events(Some("u-dup"), 10).unwrap().len(), 1);
        assert_eq!(
            r.usage_stats(utc_today()).unwrap().all_time,
            UsageCounts::new(0, 1)
        );
    }

    #[test]
    fn fail_with_event_rolls_back_status_and_event_when_the_count_write_fails() {
        let r = registry();
        r.save(&make_goopy("u-fail-rollback")).unwrap();
        break_usage_writes(&r);

        let err = r
            .fail_with_event(
                "u-fail-rollback",
                &failure("u-fail-rollback", EventPhase::Spawn, &Error::Invalid),
            )
            .unwrap_err();

        assert!(matches!(err, Error::Registry { .. }), "{err:?}");
        assert_eq!(
            r.load("u-fail-rollback").unwrap().unwrap().status,
            Status::Spawning
        );
        assert!(r.events(Some("u-fail-rollback"), 10).unwrap().is_empty());
    }

    #[test]
    fn usage_stats_sums_the_week_and_lists_days_newest_first() {
        let r = registry();
        let t = utc_today();
        r.bump_usage_on(t, UsageCounter::Provisioned);
        r.bump_usage_on(t - chrono::Duration::days(6), UsageCounter::Failed);
        r.bump_usage_on(t - chrono::Duration::days(7), UsageCounter::Provisioned);

        let stats = r.usage_stats(t).unwrap();

        assert_eq!(stats.today, UsageCounts::new(1, 0));
        assert_eq!(
            stats.last_7_days,
            UsageCounts::new(1, 1),
            "day -7 is outside"
        );
        assert_eq!(stats.all_time, UsageCounts::new(2, 1));
        let days: Vec<_> = stats.daily.iter().map(|d| d.day).collect();
        assert_eq!(
            days,
            [
                t,
                t - chrono::Duration::days(6),
                t - chrono::Duration::days(7)
            ]
        );
    }

    #[test]
    fn prune_usage_before_drops_older_days_and_never_the_total() {
        let r = registry();
        let t = utc_today();
        r.bump_usage_on(t - chrono::Duration::days(200), UsageCounter::Provisioned);
        r.bump_usage_on(t - chrono::Duration::days(90), UsageCounter::Failed);
        r.bump_usage_on(t - chrono::Duration::days(89), UsageCounter::Provisioned);

        let pruned = r
            .prune_usage_before(t - chrono::Duration::days(89))
            .unwrap();

        assert_eq!(pruned, 2, "only days strictly before the cutoff go");
        let stats = r.usage_stats(t).unwrap();
        assert_eq!(stats.daily.len(), 1);
        assert_eq!(stats.daily[0].day, t - chrono::Duration::days(89));
        assert_eq!(
            stats.all_time,
            UsageCounts::new(2, 1),
            "the total is never pruned"
        );
    }

    /// Droplets are at `user_version = 3`; the tables and the seeded totals
    /// row have to arrive by migration.
    #[test]
    fn migration_adds_usage_tables_to_a_version_3_database() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("v3.db");

        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            for (_, sql) in &MIGRATIONS[..3] {
                conn.execute_batch(sql).unwrap();
            }
            conn.execute_batch("PRAGMA user_version = 3;").unwrap();
            assert!(!table_exists(&conn, "usage_daily"));
        }

        let r = SqliteRegistry::new(&db_path).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        assert_eq!(user_version(&conn), LATEST_VERSION);
        assert!(table_exists(&conn, "usage_daily"));
        assert!(table_exists(&conn, "usage_totals"));
        assert_eq!(
            r.usage_stats(utc_today()).unwrap().all_time,
            UsageCounts::new(0, 0)
        );

        // And the upgraded database counts.
        r.save(&make_goopy("u-upgraded")).unwrap();
        r.complete_spawn("u-upgraded").unwrap();
        assert_eq!(
            r.usage_stats(utc_today()).unwrap().all_time,
            UsageCounts::new(1, 0)
        );
    }

    #[test]
    fn usage_totals_holds_at_most_one_row() {
        let r = registry();
        let conn = r.pool.get().unwrap();

        let err = conn
            .execute(
                "INSERT INTO usage_totals (id, provisioned, failed) VALUES (2, 0, 0)",
                [],
            )
            .unwrap_err();

        assert!(
            matches!(err, rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::ConstraintViolation),
            "{err:?}"
        );
    }

    /// Existing droplets are at `user_version = 1`, so the table has to arrive
    /// by migration rather than only on a fresh database.
    #[test]
    fn migration_adds_instance_events_to_a_version_1_database() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("v1.db");

        // Reproduce a pre-#118 database: step 1's schema, stamped at 1.
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(MIGRATIONS[0].1).unwrap();
            conn.execute_batch("PRAGMA user_version = 1;").unwrap();
            assert!(
                !table_exists(&conn, "instance_events"),
                "the fixture must start without the table"
            );
        }

        let r = SqliteRegistry::new(&db_path).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        assert_eq!(user_version(&conn), LATEST_VERSION);
        assert!(table_exists(&conn, "instance_events"));
        for index in [
            "idx_instance_events_slug",
            "idx_instance_events_occurred_at",
        ] {
            let found: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    params![index],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(found, 1, "{index} should exist after migration");
        }

        // And the upgraded database actually takes a write.
        r.record_event(&InstanceEvent::reaped("e-upgraded", EventPhase::Sweep))
            .unwrap();
        assert_eq!(r.events(Some("e-upgraded"), 10).unwrap().len(), 1);
    }

    #[test]
    fn build_sha_round_trips_through_save_load_and_list() {
        let r = registry();
        let stamped = make_goopy("stamped");
        let dirty = Goopy {
            build_sha: Some("c50c932aa1b2c3d4e5f60718293a4b5c6d7e8f90-dirty".to_string()),
            ..make_goopy("dirty")
        };
        r.save(&stamped).unwrap();
        r.save(&dirty).unwrap();

        assert_eq!(
            r.load("stamped").unwrap().unwrap().build_sha,
            stamped.build_sha
        );
        assert_eq!(r.load("dirty").unwrap().unwrap().build_sha, dirty.build_sha);

        let listed: Vec<_> = r.list().unwrap().into_iter().map(|g| g.build_sha).collect();
        assert!(listed.contains(&stamped.build_sha));
        assert!(listed.contains(&dirty.build_sha));
    }

    /// Rows provisioned before #119 must read back as "not recorded", not as
    /// the recorded answer `unknown` — the two mean different things.
    #[test]
    fn migration_adds_build_sha_leaving_existing_rows_unrecorded() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("v2.db");

        // Reproduce a pre-#119 database holding one instance: steps 1-2, stamped at 2.
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(MIGRATIONS[0].1).unwrap();
            conn.execute_batch(MIGRATIONS[1].1).unwrap();
            conn.execute_batch("PRAGMA user_version = 2;").unwrap();
            conn.execute(
                "INSERT INTO goopies (slug, life_in_days, created_at, status, working_dir, port, provisioner_kind, service_version) \
                 VALUES ('old-row', 7, '2026-06-01T00:00:00Z', 'Done', '/tmp/old-row', 8080, 'Hello', '0.1.0')",
                [],
            )
            .unwrap();
        }

        let r = SqliteRegistry::new(&db_path).unwrap();
        assert_eq!(
            user_version(&rusqlite::Connection::open(&db_path).unwrap()),
            LATEST_VERSION
        );

        let old = r
            .load("old-row")
            .unwrap()
            .expect("old row survives migration");
        assert_eq!(old.build_sha, None);
        assert_eq!(old.service_version, "0.1.0", "service_version is untouched");

        // And the upgraded database records the build for new rows.
        let new = make_goopy("new-row");
        r.save(&new).unwrap();
        assert_eq!(r.load("new-row").unwrap().unwrap().build_sha, new.build_sha);
    }

    // -------------------------------------------------------------------------
    // Migration harness tests
    // -------------------------------------------------------------------------

    /// Simulates a pre-migration database by opening a file-backed DB,
    /// dropping the tables to reproduce an "old" schema shape, resetting
    /// `user_version` to 0, then calling `SqliteRegistry::new` again.
    /// Asserts that the migration runs, the tables are (re-)created, and
    /// `user_version` is bumped to the latest version.
    #[test]
    fn migration_runs_on_outdated_file_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("migrate_test.db");

        // First open: creates a fully-initialised DB at LATEST_VERSION.
        SqliteRegistry::new(&db_path).unwrap();

        // Simulate an "old" database: drop both tables and reset user_version.
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "
                DROP TABLE IF EXISTS goopies;
                DROP TABLE IF EXISTS allocated_ports;
                PRAGMA user_version = 0;
                ",
            )
            .unwrap();

            // Confirm the user tables are gone before we open again.
            // Note: sqlite_sequence (created by AUTOINCREMENT) may still be
            // present — we only check for the two user-defined tables.
            let table_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('goopies', 'allocated_ports')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                table_count, 0,
                "user tables should be absent before migration"
            );

            assert_eq!(
                user_version(&conn),
                0,
                "user_version should be 0 before migration"
            );
        }

        // Second open: triggers migrate() and should re-create the tables.
        SqliteRegistry::new(&db_path).unwrap();

        // Verify the outcome directly via a raw connection.
        let conn = rusqlite::Connection::open(&db_path).unwrap();

        assert_eq!(
            user_version(&conn),
            LATEST_VERSION,
            "user_version should equal LATEST_VERSION after migration"
        );

        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('goopies', 'allocated_ports')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 2, "both tables should exist after migration");
    }

    /// Asserts that opening an already up-to-date file-backed DB a second time
    /// is a no-op: `new()` succeeds without error and `user_version` stays at
    /// the latest value.
    #[test]
    fn migration_is_noop_on_up_to_date_file_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("noop_test.db");

        // First open: initialise to latest version.
        SqliteRegistry::new(&db_path).unwrap();

        // Capture the version before the second open.
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            assert_eq!(user_version(&conn), LATEST_VERSION);
        }

        // Second open: must succeed without error.
        SqliteRegistry::new(&db_path).unwrap();

        // Version must be unchanged.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        assert_eq!(
            user_version(&conn),
            LATEST_VERSION,
            "user_version must be unchanged on a no-op open"
        );
    }

    /// Regression guard: `:memory:` databases must go through the same
    /// migration runner as file-backed ones, not a special-cased shortcut that
    /// applies only one step's SQL.
    #[test]
    fn new_on_memory_db_initialises_to_latest_version() {
        let r = registry();
        let conn = r.pool.get().unwrap();

        assert_eq!(
            user_version(&conn),
            LATEST_VERSION,
            "an in-memory DB must be stamped at LATEST_VERSION"
        );

        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('goopies', 'allocated_ports')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            table_count, 2,
            "both tables should exist in an in-memory DB"
        );
    }

    // -------------------------------------------------------------------------
    // Migration runner tests
    //
    // These drive `migrate` against a synthetic multi-step chain rather than
    // the real `MIGRATIONS`, so the step-by-step behaviour is covered even
    // while production sits at a single version.
    // -------------------------------------------------------------------------

    /// A three-step chain used to exercise the runner.
    ///
    /// Two properties make it a meaningful test fixture:
    ///
    /// * Step 3 is an `ALTER TABLE` on the table created by step 1, so applying
    ///   the steps out of order — or skipping earlier ones — fails loudly with
    ///   `no such table` instead of passing silently.
    /// * No step uses `IF NOT EXISTS`, so re-applying an already-applied step
    ///   errors.  A no-op run therefore has to genuinely skip, not just be
    ///   idempotent by luck.
    const TEST_MIGRATIONS: &[(u32, &str)] = &[
        (1, "CREATE TABLE t1 (a INTEGER);"),
        (2, "CREATE TABLE t2 (b INTEGER);"),
        (3, "ALTER TABLE t1 ADD COLUMN c TEXT;"),
    ];

    fn user_version(conn: &Connection) -> u32 {
        read_user_version(conn).unwrap()
    }

    fn table_exists(conn: &Connection, table: &str) -> bool {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                params![table],
                |row| row.get(0),
            )
            .unwrap();
        n > 0
    }

    fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                params![table, column],
                |row| row.get(0),
            )
            .unwrap();
        n > 0
    }

    /// Apply the SQL of every step up to and including `through`, then stamp
    /// `user_version`, to simulate a DB left behind by an older build.
    fn seed_at_version(conn: &Connection, through: u32) {
        for &(target, sql) in TEST_MIGRATIONS {
            if target > through {
                break;
            }
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(&format!("PRAGMA user_version = {through};"))
            .unwrap();
    }

    /// Runs `case` against both a `:memory:` connection and a file-backed one,
    /// since the two paths differ in how SQLite persists `user_version`.
    fn for_each_backing(case: impl Fn(&mut Connection)) {
        let mut mem = Connection::open_in_memory().unwrap();
        case(&mut mem);

        let dir = tempfile::tempdir().unwrap();
        let mut file = Connection::open(dir.path().join("runner_test.db")).unwrap();
        case(&mut file);
    }

    #[test]
    fn migrate_on_fresh_db_applies_every_step() {
        for_each_backing(|conn| {
            assert_eq!(user_version(conn), 0, "a fresh DB starts at version 0");

            migrate(conn, TEST_MIGRATIONS).unwrap();

            assert_eq!(user_version(conn), 3);
            assert!(table_exists(conn, "t1"));
            assert!(table_exists(conn, "t2"));
            assert!(
                column_exists(conn, "t1", "c"),
                "step 3 must have run after step 1"
            );
        });
    }

    #[test]
    fn migrate_from_v1_applies_only_remaining_steps() {
        for_each_backing(|conn| {
            seed_at_version(conn, 1);

            migrate(conn, TEST_MIGRATIONS).unwrap();

            assert_eq!(user_version(conn), 3);
            assert!(table_exists(conn, "t2"), "step 2 should have run");
            assert!(column_exists(conn, "t1", "c"), "step 3 should have run");
        });
    }

    #[test]
    fn migrate_from_v2_applies_only_remaining_steps() {
        for_each_backing(|conn| {
            seed_at_version(conn, 2);

            // Step 1 and 2 are non-idempotent DDL, so this only succeeds if
            // both are skipped.
            migrate(conn, TEST_MIGRATIONS).unwrap();

            assert_eq!(user_version(conn), 3);
            assert!(column_exists(conn, "t1", "c"), "step 3 should have run");
        });
    }

    #[test]
    fn migrate_on_up_to_date_db_is_a_noop() {
        for_each_backing(|conn| {
            seed_at_version(conn, 3);

            // Every step is non-idempotent, so re-running any of them would
            // surface as an error here.
            migrate(conn, TEST_MIGRATIONS).unwrap();

            assert_eq!(user_version(conn), 3);
        });
    }

    #[test]
    fn migrate_rejects_db_stamped_newer_than_this_build() {
        for_each_backing(|conn| {
            conn.execute_batch("PRAGMA user_version = 99;").unwrap();

            let err = migrate(conn, TEST_MIGRATIONS).unwrap_err();
            assert!(
                matches!(
                    err,
                    Error::SchemaVersionTooNew {
                        found: 99,
                        supported: 3
                    }
                ),
                "{err:?}"
            );

            // The DB must be left untouched: no DDL, no version rewrite.
            assert!(
                !table_exists(conn, "t1"),
                "no step should have been applied"
            );
            assert_eq!(user_version(conn), 99, "user_version must not be rewritten");
        });
    }

    #[test]
    fn migrate_with_empty_migration_list_is_a_noop() {
        for_each_backing(|conn| {
            migrate(conn, &[]).unwrap();
            assert_eq!(user_version(conn), 0);
        });
    }

    // -------------------------------------------------------------------------
    // Per-step atomicity
    // -------------------------------------------------------------------------

    /// A chain whose step 2 batch succeeds partway and then fails: `t2` is
    /// created, then re-creating the existing `t1` errors.  Applied without a
    /// transaction, this leaves `t2` behind; applied atomically, it leaves no
    /// trace.
    const FAILING_MIGRATIONS: &[(u32, &str)] = &[
        (1, "CREATE TABLE t1 (a INTEGER);"),
        (
            2,
            "CREATE TABLE t2 (b INTEGER); CREATE TABLE t1 (dup INTEGER);",
        ),
    ];

    /// Pins the SQLite behaviour the runner's atomicity depends on: both DDL
    /// and `user_version` are transactional, so a rollback undoes the pair.  If
    /// this ever stopped holding, `migrate` would be silently non-atomic again.
    #[test]
    fn sqlite_rolls_back_user_version_and_ddl_together() {
        let conn = Connection::open_in_memory().unwrap();

        conn.execute_batch(
            "
            BEGIN;
            PRAGMA user_version = 7;
            CREATE TABLE rolled_back (a INTEGER);
            ROLLBACK;
            ",
        )
        .unwrap();

        assert_eq!(user_version(&conn), 0, "user_version must roll back");
        assert!(!table_exists(&conn, "rolled_back"), "DDL must roll back");
    }

    #[test]
    fn migrate_rolls_back_a_failed_step_entirely() {
        for_each_backing(|conn| {
            let err = migrate(conn, FAILING_MIGRATIONS).unwrap_err();
            assert!(matches!(err, Error::SchemaMigration(_)), "{err:?}");

            // Step 1 committed on its own, so the DB stays at version 1.
            assert_eq!(
                user_version(conn),
                1,
                "a failed step must not bump user_version"
            );
            assert!(table_exists(conn, "t1"), "step 1 should have committed");

            // The discriminating assertion: `t2` was created by step 2's first
            // statement before the second one failed.  Without a per-step
            // transaction it survives, leaving the DB in a state no version
            // number describes.
            assert!(
                !table_exists(conn, "t2"),
                "a failed step must leave no partial DDL behind"
            );
        });
    }

    /// Stands in for a concurrent migrator: another process applied step 2 and
    /// bumped `user_version` after this one had already read the old value.
    /// The runner must notice when it takes the write lock and skip the step
    /// rather than re-running its non-idempotent DDL.
    #[test]
    fn migrate_skips_a_step_applied_by_another_process() {
        for_each_backing(|conn| {
            seed_at_version(conn, 1);

            // Simulate the other process having finished step 2.  Its DDL is
            // applied here too, so re-running step 2 would fail with
            // "table t2 already exists".
            conn.execute_batch("CREATE TABLE t2 (b INTEGER); PRAGMA user_version = 2;")
                .unwrap();

            migrate(conn, TEST_MIGRATIONS).unwrap();

            assert_eq!(user_version(conn), 3);
            assert!(
                column_exists(conn, "t1", "c"),
                "step 3 should still have run"
            );
        });
    }
}
