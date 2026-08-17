//! Query functions on top of `Db::call`'s `&Connection`. Everything here is
//! unused outside tests until later tasks wire in the collector and http
//! handlers.
#![allow(dead_code)]

use rusqlite::{Connection, params, params_from_iter};

use crate::errors::DbError;
use crate::types::{
    AssetSeriesRow, AssetSnapshot, Event, GhRepo, Metric, NewEvent, PopularDay, PopularItem,
    PopularKind, RepoOverview, RepoRow, StatSnapshot, TrafficDay, TrafficKind,
};

/// Builds the NULL-safe monotonic MAX `SET` fragment for one nullable
/// counter column (substrate rule 1). Scalar `MAX()` returns NULL if *any*
/// argument is NULL, which would clobber a previously observed value when a
/// partial sync brings in NULL for this column. NULL incoming means "not
/// observed this run" — keep the existing value; otherwise take the larger
/// of the two (counters only grow).
fn null_safe_max_clause(col: &str) -> String {
    format!(
        "{col} = CASE WHEN excluded.{col} IS NULL THEN t.{col} \
         ELSE MAX(COALESCE(t.{col}, 0), excluded.{col}) END"
    )
}

// ---------------------------------------------------------------------------
// Upserts
// ---------------------------------------------------------------------------

pub fn upsert_repo(conn: &Connection, repo: &GhRepo) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO repos (id, name, description, homepage, archived, fork)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name,
           description = excluded.description,
           homepage = excluded.homepage,
           archived = excluded.archived,
           fork = excluded.fork",
        params![
            repo.id,
            repo.full_name,
            repo.description,
            repo.homepage,
            repo.archived,
            repo.fork
        ],
    )?;
    Ok(())
}

pub fn upsert_stats(
    conn: &Connection,
    repo_id: i64,
    date: &str,
    s: &StatSnapshot,
) -> Result<(), DbError> {
    let cols = ["stars", "forks", "watchers", "issues", "prs"];
    let set_clause = cols
        .iter()
        .map(|c| null_safe_max_clause(c))
        .collect::<Vec<_>>()
        .join(",\n  ");
    let sql = format!(
        "INSERT INTO repo_stats AS t (repo_id, date, stars, forks, watchers, issues, prs)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(repo_id, date) DO UPDATE SET\n  {set_clause}"
    );
    conn.execute(
        &sql,
        params![repo_id, date, s.stars, s.forks, s.watchers, s.issues, s.prs],
    )?;
    Ok(())
}

pub fn upsert_traffic_days(
    conn: &Connection,
    repo_id: i64,
    kind: TrafficKind,
    days: &[TrafficDay],
) -> Result<(), DbError> {
    let (count_col, uniques_col) = match kind {
        TrafficKind::Views => ("views_count", "views_uniques"),
        TrafficKind::Clones => ("clones_count", "clones_uniques"),
    };
    let sql = format!(
        "INSERT INTO repo_stats AS t (repo_id, date, {count_col}, {uniques_col})
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(repo_id, date) DO UPDATE SET
  {},
  {}",
        null_safe_max_clause(count_col),
        null_safe_max_clause(uniques_col)
    );
    let mut stmt = conn.prepare(&sql)?;
    for day in days {
        // Traffic timestamps truncated at this layer: GitHub sends
        // `2026-08-01T00:00:00Z`, schema dates are plain `YYYY-MM-DD`.
        let date = day.timestamp.split('T').next().unwrap_or(&day.timestamp);
        stmt.execute(params![repo_id, date, day.count, day.uniques])?;
    }
    Ok(())
}

pub fn upsert_referrers(
    conn: &Connection,
    repo_id: i64,
    date: &str,
    rows: &[PopularDay],
) -> Result<(), DbError> {
    let mut stmt = conn.prepare(
        "INSERT INTO repo_referrers AS t (repo_id, date, referrer, count, uniques)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(repo_id, date, referrer) DO UPDATE SET
           count = MAX(t.count, excluded.count),
           uniques = MAX(t.uniques, excluded.uniques)",
    )?;
    for row in rows {
        stmt.execute(params![repo_id, date, row.name, row.count, row.uniques])?;
    }
    Ok(())
}

pub fn upsert_paths(
    conn: &Connection,
    repo_id: i64,
    date: &str,
    rows: &[PopularDay],
) -> Result<(), DbError> {
    let mut stmt = conn.prepare(
        "INSERT INTO repo_popular_paths AS t (repo_id, date, path, title, count, uniques)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(repo_id, date, path) DO UPDATE SET
           title = COALESCE(excluded.title, t.title),
           count = MAX(t.count, excluded.count),
           uniques = MAX(t.uniques, excluded.uniques)",
    )?;
    for row in rows {
        stmt.execute(params![
            repo_id,
            date,
            row.name,
            row.title,
            row.count,
            row.uniques
        ])?;
    }
    Ok(())
}

pub fn upsert_release_assets(
    conn: &Connection,
    repo_id: i64,
    date: &str,
    rows: &[AssetSnapshot],
) -> Result<(), DbError> {
    let mut stmt = conn.prepare(
        "INSERT INTO release_assets AS t (repo_id, date, release_tag, asset_name, download_count)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(repo_id, date, release_tag, asset_name) DO UPDATE SET
           -- download_count is NOT NULL; scalar MAX is safe here, unlike the
           -- nullable columns rule 1 guards against.
           download_count = MAX(t.download_count, excluded.download_count)",
    )?;
    for row in rows {
        stmt.execute(params![
            repo_id,
            date,
            row.release_tag,
            row.asset_name,
            row.download_count
        ])?;
    }
    Ok(())
}

pub fn insert_star_history(
    conn: &Connection,
    repo_id: i64,
    days: &[(String, i64)],
) -> Result<(), DbError> {
    let sql = format!(
        "INSERT INTO repo_stats AS t (repo_id, date, stars)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(repo_id, date) DO UPDATE SET
  {}",
        null_safe_max_clause("stars")
    );
    let mut stmt = conn.prepare(&sql)?;
    for (date, stars) in days {
        stmt.execute(params![repo_id, date, stars])?;
    }
    Ok(())
}

/// Recompute `count_delta`/`uniques_delta` for referrers and popular paths
/// over the trailing `window_days`.
pub fn update_deltas_recent(conn: &Connection, window_days: u32) -> Result<(), DbError> {
    update_deltas_table(conn, "repo_referrers", "referrer", window_days)?;
    update_deltas_table(conn, "repo_popular_paths", "path", window_days)?;
    Ok(())
}

/// LAG-based delta computation (ghstats' `UPDATE ... FROM` pattern —
/// substrate rule 3). The LAG CTE computes over ALL rows per
/// `(repo_id, key)` partition; only the outer `UPDATE`'s `WHERE` is
/// window-scoped. Window-scoping the CTE instead would give the first
/// in-window row `LAG = NULL`, so its delta would become the *full* rolling
/// count — a fake spike at the window edge every cycle. A row with no
/// predecessor at all (true first observation) gets `delta = count`
/// (baseline-from-zero), via the `COALESCE`.
fn update_deltas_table(
    conn: &Connection,
    table: &str,
    key: &str,
    window_days: u32,
) -> Result<(), DbError> {
    let sql = format!(
        "UPDATE {table} AS t
         SET count_delta = COALESCE(lag_tbl.delta_count, lag_tbl.count),
             uniques_delta = COALESCE(lag_tbl.delta_uniques, lag_tbl.uniques)
         FROM (
             SELECT repo_id, date, {key} AS k, count, uniques,
                    count - LAG(count) OVER w AS delta_count,
                    uniques - LAG(uniques) OVER w AS delta_uniques
             FROM {table}
             WINDOW w AS (PARTITION BY repo_id, {key} ORDER BY date)
         ) AS lag_tbl
         WHERE t.repo_id = lag_tbl.repo_id AND t.date = lag_tbl.date AND t.{key} = lag_tbl.k
           AND t.date >= date('now', ?1)"
    );
    let window = format!("-{window_days} day");
    conn.execute(&sql, params![window])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Repo lifecycle
// ---------------------------------------------------------------------------

fn map_repo_row(row: &rusqlite::Row) -> rusqlite::Result<RepoRow> {
    Ok(RepoRow {
        id: row.get("id")?,
        name: row.get("name")?,
        description: row.get("description")?,
        homepage: row.get("homepage")?,
        archived: row.get("archived")?,
        fork: row.get("fork")?,
        tracked: row.get("tracked")?,
        hidden: row.get("hidden")?,
        stars_synced: row.get("stars_synced")?,
        last_synced_at: row.get("last_synced_at")?,
        last_error: row.get("last_error")?,
        error_streak: row.get("error_streak")?,
        backoff_until: row.get("backoff_until")?,
    })
}

const REPO_COLS: &str = "id, name, description, homepage, archived, fork, tracked, hidden,
     stars_synced, last_synced_at, last_error, error_streak, backoff_until";

pub fn tracked_repos(conn: &Connection) -> Result<Vec<RepoRow>, DbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {REPO_COLS} FROM repos WHERE tracked = 1 AND hidden = 0 ORDER BY name"
    ))?;
    let rows = stmt
        .query_map([], map_repo_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn set_tracked(conn: &Connection, repo_id: i64, tracked: bool) -> Result<(), DbError> {
    conn.execute(
        "UPDATE repos SET tracked = ?2 WHERE id = ?1",
        params![repo_id, tracked],
    )?;
    Ok(())
}

/// Hide repos no longer present upstream (e.g. renamed/deleted/transferred).
/// Parameterized — never string-joins ids into the `IN (...)` list.
pub fn mark_hidden(conn: &Connection, missing_ids: &[i64]) -> Result<(), DbError> {
    if missing_ids.is_empty() {
        return Ok(());
    }
    let placeholders = missing_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("UPDATE repos SET hidden = 1 WHERE id IN ({placeholders})");
    conn.execute(&sql, params_from_iter(missing_ids.iter()))?;
    Ok(())
}

pub fn record_sync_ok(conn: &Connection, repo_id: i64, at: &str) -> Result<(), DbError> {
    conn.execute(
        "UPDATE repos SET last_synced_at = ?2, last_error = NULL, error_streak = 0, backoff_until = NULL
         WHERE id = ?1",
        params![repo_id, at],
    )?;
    Ok(())
}

pub fn record_sync_err(
    conn: &Connection,
    repo_id: i64,
    err: &str,
    backoff_until: Option<&str>,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE repos SET last_error = ?2, error_streak = error_streak + 1, backoff_until = ?3
         WHERE id = ?1",
        params![repo_id, err, backoff_until],
    )?;
    Ok(())
}

pub fn mark_stars_synced(conn: &Connection, repo_id: i64) -> Result<(), DbError> {
    conn.execute(
        "UPDATE repos SET stars_synced = 1 WHERE id = ?1",
        params![repo_id],
    )?;
    Ok(())
}

pub fn repos_needing_star_backfill(conn: &Connection) -> Result<Vec<RepoRow>, DbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {REPO_COLS} FROM repos
         WHERE tracked = 1 AND hidden = 0 AND stars_synced = 0 ORDER BY name"
    ))?;
    let rows = stmt
        .query_map([], map_repo_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Reads: overview, series, asset series, popular items
// ---------------------------------------------------------------------------

/// Dashboard projection: one row per tracked, visible repo, joined to its
/// latest `repo_stats` row and total event count. Uses a
/// `ROW_NUMBER() OVER (PARTITION BY repo_id ORDER BY date DESC)` CTE to pick
/// the latest row per repo — deliberately not SQLite's
/// bare-column-with-`MAX()` extension (that only special-cases a single
/// aggregate column per query and silently picks arbitrary values for the
/// rest when more than one non-aggregated column is selected).
pub fn repo_overview(conn: &Connection) -> Result<Vec<RepoOverview>, DbError> {
    let mut stmt = conn.prepare(
        "WITH latest AS (
             SELECT repo_id, date, stars, forks, watchers, issues, prs,
                    ROW_NUMBER() OVER (PARTITION BY repo_id ORDER BY date DESC) AS rn
             FROM repo_stats
         ),
         ev AS (
             SELECT repo_id, COUNT(*) AS event_count FROM events GROUP BY repo_id
         )
         SELECT r.id, r.name, r.description, r.homepage, r.archived, r.fork,
                r.last_synced_at, r.last_error, r.error_streak,
                l.date, l.stars, l.forks, l.watchers, l.issues, l.prs,
                COALESCE(ev.event_count, 0) AS event_count
         FROM repos r
         LEFT JOIN latest l ON l.repo_id = r.id AND l.rn = 1
         LEFT JOIN ev ON ev.repo_id = r.id
         WHERE r.tracked = 1 AND r.hidden = 0
         ORDER BY r.name",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(RepoOverview {
                repo_id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                homepage: r.get(3)?,
                archived: r.get(4)?,
                fork: r.get(5)?,
                last_synced_at: r.get(6)?,
                last_error: r.get(7)?,
                error_streak: r.get(8)?,
                date: r.get(9)?,
                stars: r.get(10)?,
                forks: r.get(11)?,
                watchers: r.get(12)?,
                issues: r.get(13)?,
                prs: r.get(14)?,
                event_count: r.get(15)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Raw daily values for one metric. Returns only OBSERVED rows (the column
/// is `NOT NULL`-filtered) — dense-range materialization for chart gaps
/// happens in later tasks' handlers, not here. `days == 0` means all time.
pub fn series(
    conn: &Connection,
    repo_id: i64,
    metric: Metric,
    days: u32,
) -> Result<Vec<(String, Option<i64>)>, DbError> {
    let col = metric.column();
    // Never SUM(uniques) across days: GitHub's weekly uniques deduplicates
    // visitors across days; a sum is arithmetically wrong. `series()` reads
    // the raw daily column with no aggregation — this comment guards
    // against a future rewrite that tries to aggregate uniques metrics here.
    let mut stmt = if days == 0 {
        conn.prepare(&format!(
            "SELECT date, {col} FROM repo_stats
             WHERE repo_id = ?1 AND {col} IS NOT NULL ORDER BY date"
        ))?
    } else {
        conn.prepare(&format!(
            "SELECT date, {col} FROM repo_stats
             WHERE repo_id = ?1 AND {col} IS NOT NULL AND date >= date('now', ?2)
             ORDER BY date"
        ))?
    };
    let map_row = |r: &rusqlite::Row| -> rusqlite::Result<(String, Option<i64>)> {
        Ok((r.get(0)?, r.get(1)?))
    };
    let rows = if days == 0 {
        stmt.query_map(params![repo_id], map_row)?
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let window = format!("-{days} day");
        stmt.query_map(params![repo_id, window], map_row)?
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

pub fn asset_series(
    conn: &Connection,
    repo_id: i64,
    release_tag: &str,
    asset_name: &str,
) -> Result<Vec<AssetSeriesRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT date, download_count FROM release_assets
         WHERE repo_id = ?1 AND release_tag = ?2 AND asset_name = ?3 ORDER BY date",
    )?;
    let rows = stmt
        .query_map(params![repo_id, release_tag, asset_name], |r| {
            Ok(AssetSeriesRow {
                date: r.get(0)?,
                download_count: r.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Top referrers/paths over the trailing `days` (0 = all time). Pinned
/// aggregation (do NOT copy ghstats' `get_popular_items`, which does
/// `SUM(uniques_delta)` — exactly the summed-uniques mistake substrate rule
/// 2 forbids): `count = SUM(count_delta)`, `uniques = MAX(uniques)` — peak
/// daily snapshot, never a sum.
pub fn popular_items(
    conn: &Connection,
    repo_id: i64,
    kind: PopularKind,
    days: u32,
) -> Result<Vec<PopularItem>, DbError> {
    let (table, key) = match kind {
        PopularKind::Referrers => ("repo_referrers", "referrer"),
        PopularKind::Paths => ("repo_popular_paths", "path"),
    };
    let window_clause = if days == 0 {
        String::new()
    } else {
        " AND date >= date('now', ?2)".to_string()
    };
    let sql = format!(
        "SELECT {key} AS name,
                SUM(count_delta) AS count,
                MAX(uniques) AS uniques
         FROM {table}
         WHERE repo_id = ?1{window_clause}
         GROUP BY {key}
         ORDER BY count DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let map_row = |r: &rusqlite::Row| -> rusqlite::Result<PopularItem> {
        Ok(PopularItem {
            name: r.get(0)?,
            count: r.get(1)?,
            uniques: r.get(2)?,
        })
    };
    let rows = if days == 0 {
        stmt.query_map(params![repo_id], map_row)?
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let window = format!("-{days} day");
        stmt.query_map(params![repo_id, window], map_row)?
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Events CRUD
// ---------------------------------------------------------------------------

fn map_event_row(row: &rusqlite::Row) -> rusqlite::Result<Event> {
    Ok(Event {
        id: row.get("id")?,
        repo_id: row.get("repo_id")?,
        date: row.get("date")?,
        title: row.get("title")?,
        notes: row.get("notes")?,
        url: row.get("url")?,
        kind: row.get("kind")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

pub fn insert_event(conn: &Connection, e: &NewEvent) -> Result<i64, DbError> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO events (repo_id, date, title, notes, url, kind, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![e.repo_id, e.date, e.title, e.notes, e.url, e.kind, now],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_event(conn: &Connection, id: i64, e: &NewEvent) -> Result<(), DbError> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE events SET date = ?2, title = ?3, notes = ?4, url = ?5, kind = ?6, updated_at = ?7
         WHERE id = ?1",
        params![id, e.date, e.title, e.notes, e.url, e.kind, now],
    )?;
    Ok(())
}

pub fn delete_event(conn: &Connection, id: i64) -> Result<(), DbError> {
    conn.execute("DELETE FROM events WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn events_for_repo(
    conn: &Connection,
    repo_id: i64,
    kind: Option<&str>,
) -> Result<Vec<Event>, DbError> {
    let rows = match kind {
        Some(k) => {
            let mut stmt = conn.prepare(
                "SELECT * FROM events WHERE repo_id = ?1 AND kind = ?2 ORDER BY date DESC, id DESC",
            )?;
            stmt.query_map(params![repo_id, k], map_event_row)?
                .collect::<Result<Vec<_>, _>>()?
        }
        None => {
            let mut stmt = conn
                .prepare("SELECT * FROM events WHERE repo_id = ?1 ORDER BY date DESC, id DESC")?;
            stmt.query_map(params![repo_id], map_event_row)?
                .collect::<Result<Vec<_>, _>>()?
        }
    };
    Ok(rows)
}

pub fn event_kinds(conn: &Connection, repo_id: i64) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT kind FROM events WHERE repo_id = ?1 AND kind IS NOT NULL ORDER BY kind",
    )?;
    let rows = stmt
        .query_map(params![repo_id], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OptionalExtension;

    // ---- substrate-proof test helpers -------------------------------------

    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        super::super::apply_pragmas(&conn).unwrap();
        super::super::migrations::migrate(&mut conn).unwrap();
        conn
    }

    fn seed_repo(conn: &Connection, id: i64) {
        conn.execute(
            "INSERT INTO repos (id, name) VALUES (?1, ?2)",
            params![id, format!("owner/repo{id}")],
        )
        .unwrap();
    }

    macro_rules! snap {
        ($($field:ident: $val:expr),* $(,)?) => {{
            #[allow(unused_mut)]
            let mut s = StatSnapshot::default();
            $(s.$field = $val;)*
            s
        }};
    }

    fn traffic_days(rows: &[(&str, i64, i64)]) -> Vec<TrafficDay> {
        rows.iter()
            .map(|(d, c, u)| TrafficDay {
                timestamp: format!("{d}T00:00:00Z"),
                count: *c,
                uniques: *u,
            })
            .collect()
    }

    fn count_rows(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn get_stars(conn: &Connection, repo_id: i64, date: &str) -> Option<i64> {
        conn.query_row(
            "SELECT stars FROM repo_stats WHERE repo_id = ?1 AND date = ?2",
            params![repo_id, date],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
        .flatten()
    }

    fn get_prs(conn: &Connection, repo_id: i64, date: &str) -> Option<i64> {
        conn.query_row(
            "SELECT prs FROM repo_stats WHERE repo_id = ?1 AND date = ?2",
            params![repo_id, date],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
        .flatten()
    }

    fn get_views(conn: &Connection, repo_id: i64, date: &str) -> (Option<i64>, Option<i64>) {
        conn.query_row(
            "SELECT views_count, views_uniques FROM repo_stats WHERE repo_id = ?1 AND date = ?2",
            params![repo_id, date],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    fn days_ago(n: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::days(n))
            .format("%Y-%m-%d")
            .to_string()
    }

    // ---- Step 1: substrate proofs ------------------------------------------

    #[test]
    fn upsert_is_monotonic_max() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(10))).unwrap();
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(7))).unwrap(); // lower — must NOT win
        assert_eq!(get_stars(&c, 1, "2026-08-01"), Some(10));
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(12))).unwrap();
        assert_eq!(get_stars(&c, 1, "2026-08-01"), Some(12));
    }

    #[test]
    fn upsert_twice_identical_rowcount() {
        let c = test_conn();
        seed_repo(&c, 1);
        let days = traffic_days(&[("2026-08-01", 5, 3), ("2026-08-02", 8, 4)]);
        upsert_traffic_days(&c, 1, TrafficKind::Views, &days).unwrap();
        upsert_traffic_days(&c, 1, TrafficKind::Views, &days).unwrap();
        assert_eq!(count_rows(&c, "repo_stats"), 2);
    }

    #[test]
    fn missing_day_is_none_not_zero() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(10))).unwrap();
        upsert_stats(&c, 1, "2026-08-03", &snap!(stars: Some(11))).unwrap();
        let s = series(&c, 1, Metric::Stars, 0 /* all */).unwrap();
        // series returns only observed rows; dense-range materialization
        // happens in a later task's handler.
        assert!(!s.iter().any(|(d, _)| d == "2026-08-02"));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn null_column_upgraded_by_later_value() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(10))).unwrap(); // views NULL
        upsert_traffic_days(
            &c,
            1,
            TrafficKind::Views,
            &traffic_days(&[("2026-08-01", 5, 3)]),
        )
        .unwrap();
        assert_eq!(get_views(&c, 1, "2026-08-01"), (Some(5), Some(3)));
        assert_eq!(get_stars(&c, 1, "2026-08-01"), Some(10)); // disjoint columns untouched
    }

    #[test]
    fn null_incoming_never_clobbers_observed() {
        // Substrate rule 1 NULL-safety proof (scalar MAX(x, NULL) = NULL
        // would destroy data).
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(10), prs: Some(2))).unwrap();
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(11), prs: None)).unwrap(); // prs fetch failed this run
        assert_eq!(get_stars(&c, 1, "2026-08-01"), Some(11));
        assert_eq!(get_prs(&c, 1, "2026-08-01"), Some(2)); // NULL did NOT clobber
    }

    #[test]
    fn fk_cascade_on_repo_delete() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(10))).unwrap();
        insert_event(
            &c,
            &NewEvent {
                repo_id: 1,
                date: "2026-08-01".into(),
                title: "t".into(),
                notes: "".into(),
                url: None,
                kind: None,
            },
        )
        .unwrap();
        assert_eq!(count_rows(&c, "repo_stats"), 1);
        assert_eq!(count_rows(&c, "events"), 1);
        c.execute("DELETE FROM repos WHERE id = 1", []).unwrap();
        assert_eq!(count_rows(&c, "repo_stats"), 0);
        assert_eq!(count_rows(&c, "events"), 0);
    }

    #[test]
    fn event_crud_roundtrip() {
        let c = test_conn();
        seed_repo(&c, 1);
        let id = insert_event(
            &c,
            &NewEvent {
                repo_id: 1,
                date: "2026-08-01".into(),
                title: "launch".into(),
                notes: "n".into(),
                url: None,
                kind: Some("release".into()),
            },
        )
        .unwrap();
        update_event(
            &c,
            id,
            &NewEvent {
                repo_id: 1,
                date: "2026-08-02".into(),
                title: "launch v2".into(),
                notes: "n2".into(),
                url: None,
                kind: Some("release".into()),
            },
        )
        .unwrap();
        let events = events_for_repo(&c, 1, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "launch v2");
        assert_eq!(events[0].date, "2026-08-02");
        delete_event(&c, id).unwrap();
        assert!(events_for_repo(&c, 1, None).unwrap().is_empty());
    }

    #[test]
    fn deltas_scoped_to_window() {
        // referrer rows at -30d and -5d; update_deltas_recent(21) touches
        // only -5d.
        let c = test_conn();
        seed_repo(&c, 1);
        let old = days_ago(30);
        let recent = days_ago(5);
        upsert_referrers(
            &c,
            1,
            &old,
            &[PopularDay {
                name: "google".into(),
                title: None,
                count: 50,
                uniques: 10,
            }],
        )
        .unwrap();
        upsert_referrers(
            &c,
            1,
            &recent,
            &[PopularDay {
                name: "google".into(),
                title: None,
                count: 80,
                uniques: 20,
            }],
        )
        .unwrap();
        update_deltas_recent(&c, 21).unwrap();
        let old_delta: i64 = c
            .query_row(
                "SELECT count_delta FROM repo_referrers WHERE date = ?1",
                [&old],
                |r| r.get(0),
            )
            .unwrap();
        let recent_delta: i64 = c
            .query_row(
                "SELECT count_delta FROM repo_referrers WHERE date = ?1",
                [&recent],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_delta, 0); // untouched — outside window
        assert_eq!(recent_delta, 30); // 80 - 50
    }

    #[test]
    fn delta_window_edge_uses_out_of_window_lag() {
        // referrer count 100 at -22d (outside window), 103 at -20d (inside).
        // update_deltas_recent(21) -> -20d delta == 3, NOT 103 (LAG must see
        // beyond the window).
        let c = test_conn();
        seed_repo(&c, 1);
        let outside = days_ago(22);
        let inside = days_ago(20);
        upsert_referrers(
            &c,
            1,
            &outside,
            &[PopularDay {
                name: "google".into(),
                title: None,
                count: 100,
                uniques: 10,
            }],
        )
        .unwrap();
        upsert_referrers(
            &c,
            1,
            &inside,
            &[PopularDay {
                name: "google".into(),
                title: None,
                count: 103,
                uniques: 12,
            }],
        )
        .unwrap();
        update_deltas_recent(&c, 21).unwrap();
        let delta: i64 = c
            .query_row(
                "SELECT count_delta FROM repo_referrers WHERE date = ?1",
                [&inside],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(delta, 3);
    }

    #[test]
    fn popular_uniques_is_peak_not_sum() {
        // referrer uniques 5 on day1, 3 on day2 -> popular_items reports 5
        // (MAX), never 8 (sum) or delta-sum. Rows are seeded directly with
        // count_delta/uniques already set, so this targets popular_items'
        // aggregation SQL independent of update_deltas_recent.
        let c = test_conn();
        seed_repo(&c, 1);
        let d1 = days_ago(2);
        let d2 = days_ago(1);
        c.execute(
            "INSERT INTO repo_referrers (repo_id, date, referrer, count, uniques, count_delta, uniques_delta)
             VALUES (1, ?1, 'google', 5, 5, 5, 5)",
            [&d1],
        )
        .unwrap();
        c.execute(
            "INSERT INTO repo_referrers (repo_id, date, referrer, count, uniques, count_delta, uniques_delta)
             VALUES (1, ?1, 'google', 8, 3, 3, 3)",
            [&d2],
        )
        .unwrap();
        let items = popular_items(&c, 1, PopularKind::Referrers, 0).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].uniques, 5); // MAX, never 8 (sum) nor delta-sum
        assert_eq!(items[0].count, 8); // SUM(count_delta) = 5 + 3
    }

    // ---- coverage for the rest of the public surface -----------------------

    #[test]
    fn upsert_repo_inserts_and_updates_metadata() {
        let c = test_conn();
        let repo = GhRepo {
            id: 1,
            full_name: "owner/repo".into(),
            description: Some("d".into()),
            homepage: None,
            archived: false,
            fork: false,
            stargazers_count: 5,
            forks_count: 1,
            subscribers_count: Some(2),
            open_issues_count: 0,
        };
        upsert_repo(&c, &repo).unwrap();
        let repo2 = GhRepo {
            description: Some("updated".into()),
            archived: true,
            ..repo
        };
        upsert_repo(&c, &repo2).unwrap();
        let (name, desc, archived): (String, Option<String>, bool) = c
            .query_row(
                "SELECT name, description, archived FROM repos WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(name, "owner/repo");
        assert_eq!(desc, Some("updated".into()));
        assert!(archived);
        assert_eq!(count_rows(&c, "repos"), 1);
    }

    #[test]
    fn upsert_paths_monotonic_and_title_kept_on_null() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_paths(
            &c,
            1,
            "2026-08-01",
            &[PopularDay {
                name: "/docs".into(),
                title: Some("Docs".into()),
                count: 10,
                uniques: 5,
            }],
        )
        .unwrap();
        upsert_paths(
            &c,
            1,
            "2026-08-01",
            &[PopularDay {
                name: "/docs".into(),
                title: None,
                count: 3,
                uniques: 8,
            }],
        )
        .unwrap();
        let (title, count, uniques): (Option<String>, i64, i64) = c
            .query_row(
                "SELECT title, count, uniques FROM repo_popular_paths WHERE repo_id = 1 AND path = '/docs'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, Some("Docs".into())); // NULL title didn't clobber
        assert_eq!(count, 10); // MAX(10, 3)
        assert_eq!(uniques, 8); // MAX(5, 8)
    }

    #[test]
    fn release_assets_upsert_and_series_are_monotonic() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_release_assets(
            &c,
            1,
            "2026-08-01",
            &[AssetSnapshot {
                release_tag: "v1".into(),
                asset_name: "app.bin".into(),
                download_count: 10,
            }],
        )
        .unwrap();
        upsert_release_assets(
            &c,
            1,
            "2026-08-02",
            &[AssetSnapshot {
                release_tag: "v1".into(),
                asset_name: "app.bin".into(),
                download_count: 25,
            }],
        )
        .unwrap();
        // re-upsert same day with a lower count — must not regress
        upsert_release_assets(
            &c,
            1,
            "2026-08-02",
            &[AssetSnapshot {
                release_tag: "v1".into(),
                asset_name: "app.bin".into(),
                download_count: 20,
            }],
        )
        .unwrap();
        let s = asset_series(&c, 1, "v1", "app.bin").unwrap();
        assert_eq!(
            s,
            vec![
                AssetSeriesRow {
                    date: "2026-08-01".into(),
                    download_count: 10
                },
                AssetSeriesRow {
                    date: "2026-08-02".into(),
                    download_count: 25
                },
            ]
        );
    }

    #[test]
    fn insert_star_history_is_monotonic() {
        let c = test_conn();
        seed_repo(&c, 1);
        insert_star_history(&c, 1, &[("2026-08-01".into(), 100)]).unwrap();
        insert_star_history(&c, 1, &[("2026-08-01".into(), 90)]).unwrap(); // lower, must not win
        assert_eq!(get_stars(&c, 1, "2026-08-01"), Some(100));
    }

    #[test]
    fn tracked_repos_and_set_tracked_and_mark_hidden() {
        let c = test_conn();
        seed_repo(&c, 1);
        seed_repo(&c, 2);
        assert!(tracked_repos(&c).unwrap().is_empty());
        set_tracked(&c, 1, true).unwrap();
        set_tracked(&c, 2, true).unwrap();
        assert_eq!(tracked_repos(&c).unwrap().len(), 2);
        mark_hidden(&c, &[2]).unwrap();
        let tracked = tracked_repos(&c).unwrap();
        assert_eq!(tracked.len(), 1);
        assert_eq!(tracked[0].id, 1);
        // empty slice must be a no-op, not a malformed `IN ()`
        mark_hidden(&c, &[]).unwrap();
    }

    #[test]
    fn record_sync_ok_and_err_and_star_backfill() {
        let c = test_conn();
        seed_repo(&c, 1);
        set_tracked(&c, 1, true).unwrap();
        record_sync_err(&c, 1, "boom", Some("2026-08-02T00:00:00Z")).unwrap();
        record_sync_err(&c, 1, "boom again", Some("2026-08-03T00:00:00Z")).unwrap();
        let repos = repos_needing_star_backfill(&c).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].error_streak, 2);
        assert_eq!(repos[0].last_error.as_deref(), Some("boom again"));

        record_sync_ok(&c, 1, "2026-08-04T00:00:00Z").unwrap();
        let repos = tracked_repos(&c).unwrap();
        assert_eq!(repos[0].error_streak, 0);
        assert_eq!(repos[0].last_error, None);
        assert_eq!(repos[0].backoff_until, None);

        mark_stars_synced(&c, 1).unwrap();
        assert!(repos_needing_star_backfill(&c).unwrap().is_empty());
    }

    #[test]
    fn repo_overview_latest_row_and_event_count() {
        let c = test_conn();
        seed_repo(&c, 1);
        set_tracked(&c, 1, true).unwrap();
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(10))).unwrap();
        upsert_stats(&c, 1, "2026-08-02", &snap!(stars: Some(12))).unwrap();
        insert_event(
            &c,
            &NewEvent {
                repo_id: 1,
                date: "2026-08-01".into(),
                title: "t".into(),
                notes: "".into(),
                url: None,
                kind: None,
            },
        )
        .unwrap();
        let overview = repo_overview(&c).unwrap();
        assert_eq!(overview.len(), 1);
        assert_eq!(overview[0].date, Some("2026-08-02".into()));
        assert_eq!(overview[0].stars, Some(12));
        assert_eq!(overview[0].event_count, 1);
    }

    #[test]
    fn event_kinds_lists_distinct_non_null_kinds() {
        let c = test_conn();
        seed_repo(&c, 1);
        for kind in [Some("release"), Some("post"), Some("release"), None] {
            insert_event(
                &c,
                &NewEvent {
                    repo_id: 1,
                    date: "2026-08-01".into(),
                    title: "t".into(),
                    notes: "".into(),
                    url: None,
                    kind: kind.map(String::from),
                },
            )
            .unwrap();
        }
        let mut kinds = event_kinds(&c, 1).unwrap();
        kinds.sort();
        assert_eq!(kinds, vec!["post".to_string(), "release".to_string()]);
    }

    #[test]
    fn events_for_repo_filters_by_kind() {
        let c = test_conn();
        seed_repo(&c, 1);
        insert_event(
            &c,
            &NewEvent {
                repo_id: 1,
                date: "2026-08-01".into(),
                title: "a".into(),
                notes: "".into(),
                url: None,
                kind: Some("release".into()),
            },
        )
        .unwrap();
        insert_event(
            &c,
            &NewEvent {
                repo_id: 1,
                date: "2026-08-02".into(),
                title: "b".into(),
                notes: "".into(),
                url: None,
                kind: Some("post".into()),
            },
        )
        .unwrap();
        let releases = events_for_repo(&c, 1, Some("release")).unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].title, "a");
        assert_eq!(events_for_repo(&c, 1, None).unwrap().len(), 2);
    }

    #[test]
    fn series_days_window_excludes_older_rows() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(30), &snap!(stars: Some(1))).unwrap();
        upsert_stats(&c, 1, &days_ago(1), &snap!(stars: Some(2))).unwrap();
        let recent = series(&c, 1, Metric::Stars, 7).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].1, Some(2));
    }
}
