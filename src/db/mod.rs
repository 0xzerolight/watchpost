mod migrations;
pub mod queries;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::errors::DbError;
use crate::state::lock_recover;

// open_in_memory and call are unused outside tests until Task 3 wires in
// queries on top of Db::call.
#[allow(dead_code)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

#[allow(dead_code)]
impl Db {
    /// Open (creating if missing) the sqlite file at `path`, apply pragmas,
    /// back up if there are pending migrations against an existing db, then
    /// migrate to the current schema version.
    pub fn open(path: &Path) -> Result<Self, DbError> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir).map_err(|e| DbError::NotWritable(e.to_string()))?;
        }
        let mut conn = Connection::open(path).map_err(map_open_err)?;
        apply_pragmas(&conn)?;
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        migrations::guard_schema_version(v, migrations::MIGRATIONS.len())?;
        if v < migrations::MIGRATIONS.len() as i64 && v > 0 {
            backup_before_migrate(&conn, path, v)?;
        }
        migrations::migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// In-memory db for tests: same pragmas/migrations, no dir creation or
    /// backup (there is nothing on disk to back up).
    pub fn open_in_memory() -> Result<Self, DbError> {
        let mut conn = Connection::open_in_memory()?;
        apply_pragmas(&conn)?;
        migrations::migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// The ONE async bridge. rusqlite is sync; run on the blocking pool so
    /// collector inserts never stall the axum runtime (ghstats blocks the
    /// runtime with a std Mutex inside async fns — deliberate fix).
    pub async fn call<T, F>(&self, f: F) -> Result<T, DbError>
    where
        F: FnOnce(&mut Connection) -> Result<T, DbError> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = lock_recover(&conn);
            f(&mut guard)
        })
        .await?
    }
}

fn apply_pragmas(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;",
    )?;
    Ok(())
}

/// `Connection::open` reports an unwritable directory (e.g. a bind-mounted
/// data/ owned by another uid) as an opaque sqlite error; surface it as
/// `NotWritable` so main's exit path can print the chown hint.
fn map_open_err(e: rusqlite::Error) -> DbError {
    if e.to_string().contains("unable to open database file") {
        DbError::NotWritable(e.to_string())
    } else {
        DbError::Sqlite(e)
    }
}

const BACKUP_PREFIX: &str = "watchpost.v";
const BACKUP_SUFFIX: &str = ".bak";
const KEEP_BACKUPS: usize = 3;

/// rusqlite backup API (WAL-safe; `fs::copy` is not, since it can read a
/// half-checkpointed file). Name: `watchpost.v{from_v}.{UTC timestamp}.bak`,
/// written next to `db_path`. Prunes down to the newest `KEEP_BACKUPS`.
pub fn backup_before_migrate(
    conn: &Connection,
    db_path: &Path,
    from_v: i64,
) -> Result<Option<PathBuf>, DbError> {
    let dir = match db_path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup_path = dir.join(format!("{BACKUP_PREFIX}{from_v}.{ts}{BACKUP_SUFFIX}"));
    conn.backup(rusqlite::MAIN_DB, &backup_path, None)
        .map_err(|e| DbError::Backup(e.to_string()))?;
    tracing::info!(path = %backup_path.display(), "pre-migration backup created");
    prune_backups(dir)?;
    Ok(Some(backup_path))
}

/// Keep only the newest `KEEP_BACKUPS` backup files in `dir`.
///
/// Age comes from the embedded timestamp, not from the whole filename: the
/// name leads with the schema version it was taken at, and `v10` sorts before
/// `v2`, so ordering by name would start discarding a v10 database's newest
/// backups while stale v2 ones survived. A file whose name carries no
/// timestamp has no knowable age and goes first.
fn prune_backups(dir: &Path) -> Result<(), DbError> {
    let mut backups: Vec<(Option<String>, String, PathBuf)> = std::fs::read_dir(dir)
        .map_err(|e| DbError::Backup(e.to_string()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?.to_owned();
            if !(name.starts_with(BACKUP_PREFIX) && name.ends_with(BACKUP_SUFFIX)) {
                return None;
            }
            let ts = backup_timestamp(&name).map(str::to_owned);
            Some((ts, name, path))
        })
        .collect();
    // `None` sorts before `Some`, so the undatable files are the first to go;
    // the name breaks ties within a timestamp (two versions migrated in the
    // same second).
    backups.sort();
    if backups.len() > KEEP_BACKUPS {
        for (_, _, old) in &backups[..backups.len() - KEEP_BACKUPS] {
            std::fs::remove_file(old).map_err(|e| DbError::Backup(e.to_string()))?;
        }
    }
    Ok(())
}

/// The `YYYYMMDDTHHMMSSZ` segment of a backup filename, if it has one.
///
/// Matched by shape rather than by position, so it keeps working if the name
/// ever grows another dotted segment.
fn backup_timestamp(name: &str) -> Option<&str> {
    name.split('.').find(|seg| {
        let b = seg.as_bytes();
        b.len() == 16
            && b[8] == b'T'
            && b[15] == b'Z'
            && b[..8].iter().all(u8::is_ascii_digit)
            && b[9..15].iter().all(u8::is_ascii_digit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_dir_and_migrates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/watchpost.db");
        let db = Db::open(&path).unwrap();
        let v: i64 = db
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, migrations::MIGRATIONS.len() as i64);
    }

    #[test]
    fn fresh_db_needs_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watchpost.db");
        Db::open(&path).unwrap();
        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".bak"))
            .collect();
        assert!(backups.is_empty(), "fresh db must not be backed up");
    }

    /// A downgraded binary must stop at the door rather than serve a schema it
    /// does not know.
    #[test]
    fn open_refuses_a_database_written_by_a_newer_build() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watchpost.db");
        Db::open(&path).unwrap();
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();

        let err = match Db::open(&path) {
            Ok(_) => panic!("a newer schema must not open"),
            Err(e) => e,
        };
        assert!(
            matches!(err, DbError::SchemaTooNew { found: 99, .. }),
            "{err}"
        );
    }

    #[tokio::test]
    async fn call_runs_on_blocking_pool_and_returns_result() {
        let db = Db::open_in_memory().unwrap();
        let n: i64 = db
            .call(|conn| {
                conn.execute("INSERT INTO repos (id, name) VALUES (1, 'o/r')", [])?;
                conn.query_row("SELECT COUNT(*) FROM repos", [], |r| r.get(0))
                    .map_err(DbError::from)
            })
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn backup_before_migrate_keeps_newest_three() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("watchpost.db");
        let conn = Connection::open(&db_path).unwrap();
        apply_pragmas(&conn).unwrap();

        // Four pre-existing fake backups, oldest to newest by embedded timestamp.
        let fake_names = [
            "watchpost.v1.20260101T000000Z.bak",
            "watchpost.v1.20260102T000000Z.bak",
            "watchpost.v1.20260103T000000Z.bak",
            "watchpost.v1.20260104T000000Z.bak",
        ];
        for name in fake_names {
            std::fs::write(dir.path().join(name), b"fake").unwrap();
        }

        let created = backup_before_migrate(&conn, &db_path, 1).unwrap();
        assert!(created.is_some());

        let mut remaining: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(BACKUP_PREFIX) && n.ends_with(BACKUP_SUFFIX))
            .collect();
        remaining.sort();

        // 4 fake + 1 new = 5, pruned to newest 3 -> the two oldest fakes are gone.
        assert_eq!(remaining.len(), KEEP_BACKUPS);
        assert!(!remaining.contains(&fake_names[0].to_string()));
        assert!(!remaining.contains(&fake_names[1].to_string()));
        assert!(remaining.contains(&fake_names[3].to_string()));
    }

    /// Sorting whole filenames orders `v10` before `v2`, which prunes a
    /// database's newest backups first the moment the schema reaches v10. The
    /// embedded timestamp is what decides age.
    #[test]
    fn pruning_is_chronological_across_schema_versions() {
        let dir = tempfile::tempdir().unwrap();
        let names = [
            "watchpost.v2.20260101T000000Z.bak",  // oldest
            "watchpost.v10.20260102T000000Z.bak", // lexicographically first
            "watchpost.v2.20260103T000000Z.bak",
            "watchpost.v10.20260104T000000Z.bak", // newest
            "watchpost.vX.bak",                   // no timestamp: age unknown
        ];
        for name in names {
            std::fs::write(dir.path().join(name), b"fake").unwrap();
        }

        prune_backups(dir.path()).unwrap();

        let remaining: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();

        assert_eq!(remaining.len(), KEEP_BACKUPS, "{remaining:?}");
        assert!(!remaining.contains(&names[4].to_string()), "{remaining:?}");
        assert!(!remaining.contains(&names[0].to_string()), "{remaining:?}");
        for kept in &names[1..4] {
            assert!(remaining.contains(&kept.to_string()), "{remaining:?}");
        }
    }
}
