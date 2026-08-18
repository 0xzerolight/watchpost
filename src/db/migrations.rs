use rusqlite::Connection;

use crate::errors::DbError;

/// A migration takes the current transaction and mutates schema/data.
/// Kept as a plain `fn` pointer (not a closure) so `MIGRATIONS` can be a
/// `const` array — migrations never need captured state.
pub type Migration = fn(&rusqlite::Transaction) -> Result<(), DbError>;

pub const MIGRATIONS: &[Migration] = &[migrate_v1];

/// Run all pending migrations against `conn`, bringing `PRAGMA user_version`
/// up to `MIGRATIONS.len()`.
pub fn migrate(conn: &mut Connection) -> Result<(), DbError> {
    run_migrations(conn, MIGRATIONS)
}

pub(crate) fn run_migrations(
    conn: &mut Connection,
    migrations: &[Migration],
) -> Result<(), DbError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    guard_schema_version(version, migrations.len())?;
    for (idx, m) in migrations.iter().enumerate() {
        let target = idx as i64 + 1;
        if version < target {
            tracing::info!("migrating db to v{target}");
            let tx = conn.transaction()?;
            m(&tx).map_err(|e| DbError::Migration {
                version: target,
                source: Box::new(e),
            })?;
            tx.pragma_update(None, "user_version", target)?;
            tx.commit()?; // DDL + version bump atomic
        }
    }
    Ok(())
}

/// Refuse a database a newer build wrote.
///
/// Migrations only run forward, so an older binary meeting a newer schema
/// would find nothing to do and then serve queries against a shape it does not
/// know — a silent half-working install, and one that keeps writing to a file
/// the newer build still considers current. Refusing to open is the only
/// honest answer, and the message names the fix.
///
/// Checked by `Db::open` before it touches the file and here, which is what
/// the in-memory and test paths go through.
pub(crate) fn guard_schema_version(found: i64, supported: usize) -> Result<(), DbError> {
    if found > supported as i64 {
        return Err(DbError::SchemaTooNew {
            found,
            supported: supported as i64,
        });
    }
    Ok(())
}

fn migrate_v1(tx: &rusqlite::Transaction) -> Result<(), DbError> {
    tx.execute_batch(
        "CREATE TABLE repos (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  description TEXT, homepage TEXT,
  archived INTEGER NOT NULL DEFAULT 0, fork INTEGER NOT NULL DEFAULT 0,
  tracked INTEGER NOT NULL DEFAULT 0,
  hidden INTEGER NOT NULL DEFAULT 0,
  stars_synced INTEGER NOT NULL DEFAULT 0,
  last_synced_at TEXT, last_error TEXT,
  error_streak INTEGER NOT NULL DEFAULT 0, backoff_until TEXT);

CREATE TABLE repo_stats (
  repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
  date TEXT NOT NULL,
  stars INTEGER, forks INTEGER, watchers INTEGER, issues INTEGER, prs INTEGER,
  views_count INTEGER, views_uniques INTEGER, clones_count INTEGER, clones_uniques INTEGER,
  -- NULL = not observed, 0 = observed zero. Missing days render as gaps (rate metrics);
  -- cumulative columns (stars, and release_assets.download_count) carry forward at render (rule 3).
  PRIMARY KEY (repo_id, date));

CREATE TABLE repo_referrers (
  repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
  date TEXT NOT NULL, referrer TEXT NOT NULL,
  count INTEGER NOT NULL DEFAULT 0, uniques INTEGER NOT NULL DEFAULT 0,
  count_delta INTEGER NOT NULL DEFAULT 0, uniques_delta INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (repo_id, date, referrer));

CREATE TABLE repo_popular_paths (
  repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
  date TEXT NOT NULL, path TEXT NOT NULL, title TEXT,
  count INTEGER NOT NULL DEFAULT 0, uniques INTEGER NOT NULL DEFAULT 0,
  count_delta INTEGER NOT NULL DEFAULT 0, uniques_delta INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (repo_id, date, path));

CREATE TABLE release_assets (
  repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
  date TEXT NOT NULL, release_tag TEXT NOT NULL, asset_name TEXT NOT NULL,
  download_count INTEGER NOT NULL,
  PRIMARY KEY (repo_id, date, release_tag, asset_name));

CREATE TABLE events (
  id INTEGER PRIMARY KEY,
  repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
  date TEXT NOT NULL,
  title TEXT NOT NULL,
  notes TEXT NOT NULL DEFAULT '',
  url TEXT,
  kind TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);

CREATE INDEX idx_events_repo_date    ON events(repo_id, date);
CREATE INDEX idx_events_kind         ON events(kind) WHERE kind IS NOT NULL;
CREATE INDEX idx_repo_stats_date     ON repo_stats(date);
CREATE INDEX idx_referrers_repo_date ON repo_referrers(repo_id, date);
CREATE INDEX idx_paths_repo_date     ON repo_popular_paths(repo_id, date);
CREATE INDEX idx_assets_series       ON release_assets(repo_id, release_tag, asset_name, date);
CREATE INDEX idx_repos_tracked       ON repos(tracked, hidden);",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fresh_db_migrates_to_current() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        migrate(&mut c).unwrap();
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        // spot-check schema exists
        c.execute("INSERT INTO repos (id, name) VALUES (1, 'o/r')", [])
            .unwrap();
    }
    #[test]
    fn migrate_twice_is_noop() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        migrate(&mut c).unwrap();
        migrate(&mut c).unwrap(); // must not error (CREATE TABLE would collide if re-run)
    }
    #[test]
    fn a_schema_from_a_newer_build_is_refused() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        c.pragma_update(None, "user_version", 99).unwrap();

        let err = migrate(&mut c).unwrap_err();
        assert!(
            matches!(err, DbError::SchemaTooNew { found: 99, supported }
                if supported == MIGRATIONS.len() as i64),
            "{err}"
        );
        assert!(err.to_string().contains("upgrade watchpost"), "{err}");
    }

    #[test]
    fn failed_migration_rolls_back() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        let bad: super::Migration = |_tx| Err(crate::errors::DbError::Backup("boom".into()));
        let r = run_migrations(&mut c, &[bad]);
        assert!(r.is_err());
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 0); // version unchanged — nothing partial
    }
}
