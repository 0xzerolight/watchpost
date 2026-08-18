//! Query functions on top of `Db::call`'s `&Connection`. Everything here is
//! unused outside tests until later tasks wire in the collector and http
//! handlers.
#![allow(dead_code)]

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::errors::DbError;
use crate::types::{
    AssetSeriesRow, AssetSnapshot, Event, GhRepo, Metric, NewEvent, PopularDay, PopularItem,
    PopularKind, RepoOverview, RepoRow, StatSnapshot, TrafficDay, TrafficKind,
};

/// Builds the NULL-safe last-write-wins `SET` fragment for one nullable
/// column (substrate rule 1). NULL incoming means "not observed this run",
/// never "observed as zero", so it keeps the existing value; any non-NULL
/// value replaces what is stored.
///
/// This is the rule for writers that send a **full snapshot** of a counter
/// GitHub can report lower than before — [`upsert_stats`]. Stars, forks,
/// watchers, issues and PRs all fall in normal use (unstars, closed issues,
/// merged PRs), and a MAX rule would pin the day's row to its intraday peak
/// and silently drop every decrease. Compare [`null_safe_max_clause`].
fn null_safe_last_clause(col: &str) -> String {
    format!("{col} = COALESCE(excluded.{col}, t.{col})")
}

/// Builds the NULL-safe monotonic MAX `SET` fragment for one nullable
/// counter column (substrate rule 1). Scalar `MAX()` returns NULL if *any*
/// argument is NULL, which would clobber a previously observed value when a
/// partial sync brings in NULL for this column, hence the CASE guard.
///
/// This is the rule for writers whose value may be a **partial view of the
/// same quantity** another writer reports in full, so the larger observation
/// is the more complete one:
///
/// - [`insert_star_history`] replays stargazer pages into a running total,
///   truncated wherever the per-cycle page budget ran out. Last-write-wins
///   here would overwrite the true count from [`upsert_stats`] with a
///   fraction of it.
/// - [`upsert_traffic_days`] rewrites GitHub's rolling 14-day window every
///   cycle, and the current day is still accumulating when it is read.
fn null_safe_max_clause(col: &str) -> String {
    format!(
        "{col} = CASE WHEN excluded.{col} IS NULL THEN t.{col} \
         ELSE MAX(COALESCE(t.{col}, 0), excluded.{col}) END"
    )
}

// ---------------------------------------------------------------------------
// Upserts
// ---------------------------------------------------------------------------

/// Insert or refresh one repo's metadata. Rediscovery also clears `hidden`,
/// which keeps the invariant "hidden ⇔ absent from the last successful
/// discovery": a repo hidden by a truncated upstream listing comes back on the
/// next listing that includes it, instead of being invisible forever with no
/// recovery path but a sqlite shell.
pub fn upsert_repo(conn: &Connection, repo: &GhRepo) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO repos (id, name, description, homepage, archived, fork)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name,
           description = excluded.description,
           homepage = excluded.homepage,
           archived = excluded.archived,
           fork = excluded.fork,
           hidden = 0",
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

/// Record one day's counter snapshot. Each column keeps the **last**
/// observation of the day rather than the day's maximum — see
/// [`null_safe_last_clause`] for why these counters must be allowed to fall.
pub fn upsert_stats(
    conn: &Connection,
    repo_id: i64,
    date: &str,
    s: &StatSnapshot,
) -> Result<(), DbError> {
    let cols = ["stars", "forks", "watchers", "issues", "prs"];
    let set_clause = cols
        .iter()
        .map(|c| null_safe_last_clause(c))
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

/// Backfill `stars` from replayed stargazer pages. Deliberately keeps the
/// monotonic MAX rule that [`upsert_stats`] no longer uses: these totals are
/// truncated wherever the page budget ran out, so the larger of the two
/// observations is the trustworthy one — see [`null_safe_max_clause`].
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
/// substrate rule 3). The outer `UPDATE` rewrites the trailing
/// `window_days`; the LAG CTE spans **twice** that, so the oldest row being
/// updated still sees the predecessor it is a diff against. A row with no
/// visible predecessor gets `delta = count` (baseline-from-zero), via the
/// `COALESCE`.
///
/// Both bounds are load-bearing. Scoping the CTE to `window_days` too would
/// give the first in-window row `LAG = NULL`, so its delta would become the
/// *full* rolling count — a fake spike at the window edge every cycle. Not
/// scoping it at all recomputes `LAG` over every row these tables have ever
/// held, hourly, for a result the outer `WHERE` then throws away: cost that
/// grows with history rather than with the window.
///
/// Doubling is what makes the two bounds agree. A row at the outer window's
/// edge keeps its predecessor as long as the gap between the two is at most
/// `window_days`, which hourly collection always satisfies. Only an
/// observation gap wider than `2 × window_days` — a collector down for six
/// weeks at the default 21 — drops a predecessor, and the row restarts from
/// baseline. That is arguably the more correct reading anyway: `count` is
/// GitHub's rolling 14-day total, so a diff across a gap that long measures
/// nothing.
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
             WHERE date >= date('now', ?1)
             WINDOW w AS (PARTITION BY repo_id, {key} ORDER BY date)
         ) AS lag_tbl
         WHERE t.repo_id = lag_tbl.repo_id AND t.date = lag_tbl.date AND t.{key} = lag_tbl.k
           AND t.date >= date('now', ?2)"
    );
    let lag_window = format!("-{} day", window_days.saturating_mul(2));
    let window = format!("-{window_days} day");
    conn.execute(&sql, params![lag_window, window])?;
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

/// Every repo discovery has ever seen and not hidden, tracked or not — what
/// the settings picker offers. `tracked_repos` is the collector's view; this
/// one is the user's.
pub fn known_repos(conn: &Connection) -> Result<Vec<RepoRow>, DbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {REPO_COLS} FROM repos WHERE hidden = 0 ORDER BY name"
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

/// A sync that wrote something but not everything: the repo answered, so it
/// counts as synced and is taken out of backoff, while `last_error` keeps the
/// endpoints that failed.
///
/// `error_streak` is deliberately left alone. It is the exponent behind
/// [`record_sync_err`]'s backoff, and data landing means the repo is healthy
/// enough to try again next cycle — counting partials would walk the streak up
/// until the first total failure backed off for a day instead of 30 minutes.
pub fn record_sync_partial(
    conn: &Connection,
    repo_id: i64,
    at: &str,
    err: &str,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE repos SET last_synced_at = ?2, last_error = ?3, backoff_until = NULL
         WHERE id = ?1",
        params![repo_id, at, err],
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

fn map_overview_row(r: &rusqlite::Row) -> rusqlite::Result<RepoOverview> {
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
}

/// The overview projection, either for every visible repo or for one.
///
/// Both callers share this one statement so the column order can never drift
/// away from [`map_overview_row`]'s positional reads. `one_repo` splices the
/// `?1` filter into **both** CTEs, not just the outer `WHERE`: a window
/// function is an optimization barrier, so an outer-only filter still makes
/// SQLite number every repo's stats rows and count every repo's events before
/// discarding all but one. The literal fragments are built here, never from
/// caller input.
fn overview_sql(one_repo: bool) -> String {
    let (scope, outer, order) = if one_repo {
        ("WHERE repo_id = ?1", "AND r.id = ?1", "")
    } else {
        ("", "", "ORDER BY r.name")
    };
    format!(
        "WITH latest AS (
             SELECT repo_id, date, stars, forks, watchers, issues, prs,
                    ROW_NUMBER() OVER (PARTITION BY repo_id ORDER BY date DESC) AS rn
             FROM repo_stats {scope}
         ),
         ev AS (
             SELECT repo_id, COUNT(*) AS event_count FROM events {scope} GROUP BY repo_id
         )
         SELECT r.id, r.name, r.description, r.homepage, r.archived, r.fork,
                r.last_synced_at, r.last_error, r.error_streak,
                l.date, l.stars, l.forks, l.watchers, l.issues, l.prs,
                COALESCE(ev.event_count, 0) AS event_count
         FROM repos r
         LEFT JOIN latest l ON l.repo_id = r.id AND l.rn = 1
         LEFT JOIN ev ON ev.repo_id = r.id
         WHERE r.tracked = 1 AND r.hidden = 0 {outer}
         {order}"
    )
}

/// Dashboard projection: one row per tracked, visible repo, joined to its
/// latest `repo_stats` row and total event count. Uses a
/// `ROW_NUMBER() OVER (PARTITION BY repo_id ORDER BY date DESC)` CTE to pick
/// the latest row per repo — deliberately not SQLite's
/// bare-column-with-`MAX()` extension (that only special-cases a single
/// aggregate column per query and silently picks arbitrary values for the
/// rest when more than one non-aggregated column is selected).
pub fn repo_overview(conn: &Connection) -> Result<Vec<RepoOverview>, DbError> {
    let mut stmt = conn.prepare(&overview_sql(false))?;
    let rows = stmt
        .query_map([], map_overview_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The same projection for a single repo, for the page that renders one.
///
/// `None` means the repo has no page — untracked, hidden upstream, or never
/// discovered at all — which is exactly [`repo_overview`]'s predicate, so a
/// repo the dashboard does not link to 404s.
pub fn repo_overview_one(conn: &Connection, repo_id: i64) -> Result<Option<RepoOverview>, DbError> {
    let row = conn
        .query_row(&overview_sql(true), params![repo_id], map_overview_row)
        .optional()?;
    Ok(row)
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

/// One slot per UTC day across the trailing `days` window ending today,
/// whether or not that day was observed. The single source of truth for chart
/// series shape — every page that plots `repo_stats` goes through here, so a
/// gap means the same thing everywhere.
///
/// [`series`] returns observed rows only, which is the right shape for a table
/// but the wrong one for a chart: a collector that ran on Monday and Thursday
/// would draw those two points as adjacent, silently compressing the week.
///
/// How an unobserved day is filled depends on the metric
/// ([`Metric::carries_forward`]):
///
/// * **Snapshot metrics** (stars, forks, watchers, issues, PRs) carry forward
///   the last value observed at or before that day. "No observation" is not
///   "value changed", so a gap between two syncs is a flat line, not a hole
///   and never a drop to zero. The carry is *seeded* from the latest observed
///   row strictly before the window, so a window that opens mid-history starts
///   at the right level rather than with a run of nulls. `None` therefore
///   appears only for days preceding the first observation ever.
/// * **Rate metrics** (the four traffic columns) stay `None`. A day with no
///   traffic row is unknown activity; repeating the previous day's count would
///   fabricate visits.
///
/// `days == 0` is an empty range, not "all time" — unlike [`series`], there is
/// no unbounded dense window.
pub fn dense_series(
    conn: &Connection,
    repo_id: i64,
    metric: Metric,
    days: u32,
) -> Result<Vec<(String, Option<i64>)>, DbError> {
    if days == 0 {
        return Ok(Vec::new());
    }
    let col = metric.column();
    let today = chrono::Utc::now().date_naive();
    let start = today - chrono::Duration::days(i64::from(days) - 1);
    let (start_str, end_str) = (start.to_string(), today.to_string());

    // Observed rows inside the window, keyed by date for O(1) lookup while
    // walking the calendar below. No ORDER BY: the calendar walk supplies the
    // ordering, this is only a lookup table.
    let mut stmt = conn.prepare(&format!(
        "SELECT date, {col} FROM repo_stats
         WHERE repo_id = ?1 AND {col} IS NOT NULL AND date >= ?2 AND date <= ?3"
    ))?;
    let observed: std::collections::HashMap<String, i64> = stmt
        .query_map(params![repo_id, start_str, end_str], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<_, _>>()?;

    // The pre-window seed. Only snapshot metrics need it — for a rate metric
    // an earlier day says nothing about a later one.
    let mut carried = if metric.carries_forward() {
        conn.query_row(
            &format!(
                "SELECT {col} FROM repo_stats
                 WHERE repo_id = ?1 AND {col} IS NOT NULL AND date < ?2
                 ORDER BY date DESC LIMIT 1"
            ),
            params![repo_id, start_str],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    } else {
        None
    };

    let carries = metric.carries_forward();
    let mut out = Vec::with_capacity(days as usize);
    for offset in 0..i64::from(days) {
        let date = (start + chrono::Duration::days(offset)).to_string();
        let observed_today = observed.get(&date).copied();
        if observed_today.is_some() {
            carried = observed_today;
        }
        out.push((date, if carries { carried } else { observed_today }));
    }
    Ok(out)
}

/// One release asset's identity across days: `(release_tag, asset_name)`.
type AssetKey = (String, String);

/// Per-day total release downloads, dense over the trailing `days` window
/// ending today — the `downloads_total` chart series.
///
/// `download_count` is a cumulative per-asset counter, so summing the rows that
/// happen to exist on a day is meaningless: an asset with no row that day has
/// not lost its downloads, it simply was not re-read. Every
/// `(release_tag, asset_name)` pair is therefore carried forward independently,
/// seeded from its newest row strictly before the window — the same rule
/// [`dense_series`] applies to snapshot metrics — and the day's value is the sum
/// over every pair observed at or before it.
///
/// A day where no asset has ever been observed is `None`, not zero: a repo with
/// no releases yet must not plot a flat zero line. A pair first seen mid-window
/// starts contributing on that day, which is a genuine step in the total rather
/// than an artefact.
///
/// `days == 0` is an empty range, matching [`dense_series`].
pub fn dense_downloads_total(
    conn: &Connection,
    repo_id: i64,
    days: u32,
) -> Result<Vec<(String, Option<i64>)>, DbError> {
    use std::collections::HashMap;

    if days == 0 {
        return Ok(Vec::new());
    }
    let today = chrono::Utc::now().date_naive();
    let start = today - chrono::Duration::days(i64::from(days) - 1);
    let (start_str, end_str) = (start.to_string(), today.to_string());

    // The pre-window seed: each pair's newest reading before the window opens.
    // `ROW_NUMBER()` per pair rather than `MAX(date)` + join — the same
    // latest-row-per-group shape `repo_overview` uses, and for the same reason
    // (SQLite's bare-column extension only special-cases one aggregate).
    let mut carried: HashMap<AssetKey, i64> = conn
        .prepare(
            "SELECT release_tag, asset_name, download_count FROM (
                 SELECT release_tag, asset_name, download_count,
                        ROW_NUMBER() OVER (PARTITION BY release_tag, asset_name
                                           ORDER BY date DESC) AS rn
                 FROM release_assets
                 WHERE repo_id = ?1 AND date < ?2
             ) WHERE rn = 1",
        )?
        .query_map(params![repo_id, start_str], |r| {
            Ok((
                (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<_, _>>()?;

    // In-window rows bucketed by date, so the calendar walk below is one pass
    // with O(1) lookups rather than a query per day.
    let mut stmt = conn.prepare(
        "SELECT date, release_tag, asset_name, download_count FROM release_assets
         WHERE repo_id = ?1 AND date >= ?2 AND date <= ?3",
    )?;
    let mut observed: HashMap<String, Vec<(AssetKey, i64)>> = HashMap::new();
    let rows = stmt.query_map(params![repo_id, start_str, end_str], |r| {
        Ok((
            r.get::<_, String>(0)?,
            (r.get::<_, String>(1)?, r.get::<_, String>(2)?),
            r.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (date, key, count) = row?;
        observed.entry(date).or_default().push((key, count));
    }

    let mut out = Vec::with_capacity(days as usize);
    for offset in 0..i64::from(days) {
        let date = (start + chrono::Duration::days(offset)).to_string();
        if let Some(rows) = observed.get(&date) {
            for (key, count) in rows {
                carried.insert(key.clone(), *count);
            }
        }
        let total = (!carried.is_empty()).then(|| carried.values().sum());
        out.push((date, total));
    }
    Ok(out)
}

/// The earliest day watchpost has any chartable observation for, or `None` for
/// a repo that has never been synced. Backs the "All" period, which spans from
/// here to today.
pub fn first_observed_date(conn: &Connection, repo_id: i64) -> Result<Option<String>, DbError> {
    let date = conn.query_row(
        "SELECT MIN(d) FROM (
             SELECT MIN(date) AS d FROM repo_stats WHERE repo_id = ?1
             UNION ALL
             SELECT MIN(date) FROM release_assets WHERE repo_id = ?1
         )",
        params![repo_id],
        |r| r.get::<_, Option<String>>(0),
    )?;
    Ok(date)
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

/// How many rows [`popular_items`] returns at most. All-time aggregation
/// never forgets a key, so without a cap the tables would list every
/// referrer/path ever seen and only grow.
const POPULAR_LIMIT: u32 = 20;

/// Top referrers/paths over the trailing `days` (0 = all time), capped at
/// [`POPULAR_LIMIT`] rows. Pinned aggregation:
///
/// * `count = SUM(MAX(count_delta, 0))` — accumulated observed increases,
///   never a plain `SUM(count_delta)`. Deltas are baseline + diffs of a
///   rolling 14-day count, so a plain sum telescopes to the *last* snapshot:
///   a referrer that went quiet months ago would sit pinned at its stale
///   14-day count forever and outrank live ones. Clamping negatives makes
///   the number monotone while traffic is observed — an estimator of
///   cumulative traffic that undercounts before install and during downtime,
///   but never inflates.
/// * `uniques = MAX(uniques)` — peak daily snapshot, never a sum (do NOT
///   copy ghstats' `get_popular_items`, which does `SUM(uniques_delta)` —
///   exactly the summed-uniques mistake substrate rule 2 forbids).
pub fn popular_items(
    conn: &Connection,
    repo_id: i64,
    kind: PopularKind,
    days: u32,
) -> Result<Vec<PopularItem>, DbError> {
    // Referrers have no title column, so that table selects a literal NULL —
    // the two kinds keep one row type and one mapping function.
    let (table, key, title) = match kind {
        PopularKind::Referrers => ("repo_referrers", "referrer", "NULL"),
        PopularKind::Paths => ("repo_popular_paths", "path", "MAX(title)"),
    };
    let window_clause = if days == 0 {
        String::new()
    } else {
        " AND date >= date('now', ?2)".to_string()
    };
    let sql = format!(
        "SELECT {key} AS name,
                {title} AS title,
                SUM(MAX(count_delta, 0)) AS count,
                MAX(uniques) AS uniques
         FROM {table}
         WHERE repo_id = ?1{window_clause}
         GROUP BY {key}
         ORDER BY count DESC
         LIMIT {POPULAR_LIMIT}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let map_row = |r: &rusqlite::Row| -> rusqlite::Result<PopularItem> {
        Ok(PopularItem {
            name: r.get(0)?,
            title: r.get(1)?,
            count: r.get(2)?,
            uniques: r.get(3)?,
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

/// One event, scoped to the repo that owns it. `None` covers both "no such
/// event" and "that event belongs to another repo", which is what a handler
/// holding two path segments needs before it touches anything.
pub fn event_by_id(conn: &Connection, repo_id: i64, id: i64) -> Result<Option<Event>, DbError> {
    let mut stmt = conn.prepare("SELECT * FROM events WHERE id = ?1 AND repo_id = ?2")?;
    let row = stmt
        .query_map(params![id, repo_id], map_event_row)?
        .next()
        .transpose()?;
    Ok(row)
}

/// Whether `repo_id` names a repo watchpost has a page for.
///
/// Same predicate as [`repo_overview`], so an event route 404s exactly where
/// the repo page does — an untracked or upstream-hidden repo has neither.
pub fn repo_is_visible(conn: &Connection, repo_id: i64) -> Result<bool, DbError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM repos WHERE id = ?1 AND tracked = 1 AND hidden = 0",
        params![repo_id],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// Not scoped by repo: a caller holding an id that came from a URL must prove
/// ownership first with [`event_by_id`].
pub fn update_event(conn: &Connection, id: i64, e: &NewEvent) -> Result<(), DbError> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE events SET date = ?2, title = ?3, notes = ?4, url = ?5, kind = ?6, updated_at = ?7
         WHERE id = ?1",
        params![id, e.date, e.title, e.notes, e.url, e.kind, now],
    )?;
    Ok(())
}

/// Not scoped by repo — see [`update_event`].
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
    fn upsert_stats_records_last_observation() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(10))).unwrap();
        // Unstars, closed issues and merged PRs all make these counters fall.
        // A lower snapshot must win, or the row freezes at the intraday peak
        // and the drop is never recorded.
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(7))).unwrap();
        assert_eq!(get_stars(&c, 1, "2026-08-01"), Some(7));
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
        // Substrate rule 1 NULL-safety proof: NULL means "not observed this
        // run", never "observed as nothing", so it must not reach the column.
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
    fn delta_lag_reaches_back_twice_the_window() {
        // -30d is outside the 21-day update window but inside the LAG CTE's
        // 42-day reach, so the -1d row is still a diff. This is the gap the
        // doubled CTE window exists to cover.
        let c = test_conn();
        seed_repo(&c, 1);
        let older = days_ago(30);
        let recent = days_ago(1);
        upsert_referrers(
            &c,
            1,
            &older,
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
            &recent,
            &[PopularDay {
                name: "google".into(),
                title: None,
                count: 130,
                uniques: 12,
            }],
        )
        .unwrap();
        update_deltas_recent(&c, 21).unwrap();
        let delta: i64 = c
            .query_row(
                "SELECT count_delta FROM repo_referrers WHERE date = ?1",
                [&recent],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(delta, 30); // 130 - 100
    }

    #[test]
    fn delta_beyond_twice_the_window_restarts_from_zero() {
        // The predecessor at -50d is outside the CTE's 42-day reach, so the
        // -1d row has no visible LAG and takes its full count as a fresh
        // baseline. Documents the deliberate cost of bounding the recompute:
        // reachable only through an observation gap of more than twice the
        // window, which hourly collection never produces.
        let c = test_conn();
        seed_repo(&c, 1);
        let ancient = days_ago(50);
        let recent = days_ago(1);
        upsert_referrers(
            &c,
            1,
            &ancient,
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
            &recent,
            &[PopularDay {
                name: "google".into(),
                title: None,
                count: 130,
                uniques: 12,
            }],
        )
        .unwrap();
        update_deltas_recent(&c, 21).unwrap();
        let (count_delta, uniques_delta): (i64, i64) = c
            .query_row(
                "SELECT count_delta, uniques_delta FROM repo_referrers WHERE date = ?1",
                [&recent],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(count_delta, 130); // baseline-from-zero, not 130 - 100
        assert_eq!(uniques_delta, 12);
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
        assert_eq!(items[0].count, 8); // SUM(MAX(count_delta, 0)) = 5 + 3
    }

    #[test]
    fn popular_count_accumulates_increases_not_last_snapshot() {
        // Rolling 14-day counts 10, 500, 120, 5 over four days. Deltas as
        // update_deltas_recent computes them: 10 (baseline), 490, -380, -115.
        // A plain SUM telescopes to 5 — the *last* snapshot; the clamped sum
        // is 10 + 490 = 500, the traffic actually observed arriving.
        let c = test_conn();
        seed_repo(&c, 1);
        for (n, count) in [(4, 10), (3, 500), (2, 120), (1, 5)] {
            upsert_referrers(
                &c,
                1,
                &days_ago(n),
                &[PopularDay {
                    name: "hn".into(),
                    title: None,
                    count,
                    uniques: 1,
                }],
            )
            .unwrap();
        }
        update_deltas_recent(&c, 21).unwrap();
        let items = popular_items(&c, 1, PopularKind::Referrers, 0).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].count, 500);
    }

    #[test]
    fn popular_dead_key_keeps_its_first_count_and_stops_there() {
        // A referrer seen once, 200 days ago: its single delta is its full
        // rolling count (baseline-from-zero). All-time it stays listed at
        // exactly that value, so a live key that accumulates past it outranks
        // it — a dead key is never artificially dominant.
        let c = test_conn();
        seed_repo(&c, 1);
        c.execute(
            "INSERT INTO repo_referrers (repo_id, date, referrer, count, uniques, count_delta, uniques_delta)
             VALUES (1, ?1, 'dead.example', 40, 4, 40, 4)",
            [&days_ago(200)],
        )
        .unwrap();
        for n in [2, 1] {
            c.execute(
                "INSERT INTO repo_referrers (repo_id, date, referrer, count, uniques, count_delta, uniques_delta)
                 VALUES (1, ?1, 'live.example', 30, 3, 30, 3)",
                [&days_ago(n)],
            )
            .unwrap();
        }
        let items = popular_items(&c, 1, PopularKind::Referrers, 0).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "live.example");
        assert_eq!(items[0].count, 60); // 30 + 30 accumulated
        assert_eq!(items[1].name, "dead.example");
        assert_eq!(items[1].count, 40); // its only delta, forever
    }

    #[test]
    fn popular_items_caps_the_row_count() {
        // 25 referrers seeded with counts 1..=25 -> 20 rows back, and they
        // are the 20 busiest (counts 6..=25), not an arbitrary twenty.
        let c = test_conn();
        seed_repo(&c, 1);
        let date = days_ago(1);
        for i in 1..=25 {
            c.execute(
                "INSERT INTO repo_referrers (repo_id, date, referrer, count, uniques, count_delta, uniques_delta)
                 VALUES (1, ?1, ?2, ?3, 1, ?3, 1)",
                params![date, format!("ref{i}.example"), i],
            )
            .unwrap();
        }
        let items = popular_items(&c, 1, PopularKind::Referrers, 0).unwrap();
        assert_eq!(items.len(), 20);
        assert_eq!(items[0].count, 25);
        assert!(items.iter().all(|item| item.count >= 6));
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
    fn upsert_repo_unhides_a_rediscovered_repo() {
        let c = test_conn();
        let repo = GhRepo {
            id: 1,
            full_name: "owner/repo".into(),
            description: None,
            homepage: None,
            archived: false,
            fork: false,
            stargazers_count: 0,
            forks_count: 0,
            subscribers_count: None,
            open_issues_count: 0,
        };
        upsert_repo(&c, &repo).unwrap();
        set_tracked(&c, 1, true).unwrap();
        mark_hidden(&c, &[1]).unwrap();
        assert!(known_repos(&c).unwrap().is_empty());

        // A later listing that includes the repo again must restore it.
        upsert_repo(&c, &repo).unwrap();
        let known = known_repos(&c).unwrap();
        assert_eq!(known.len(), 1);
        assert!(!known[0].hidden);
        // Un-hiding never touches the user's own `tracked` flag.
        assert!(known[0].tracked);
        assert_eq!(tracked_repos(&c).unwrap().len(), 1);
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
    fn star_backfill_never_clobbers_snapshot_total() {
        // Why the two writers cannot share one rule: backfill totals are
        // truncated by the per-cycle page budget, so a blanket last-write-wins
        // would overwrite the true count with a partial one.
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(10))).unwrap();
        insert_star_history(&c, 1, &[("2026-08-01".into(), 3)]).unwrap();
        assert_eq!(get_stars(&c, 1, "2026-08-01"), Some(10));
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
    fn record_sync_partial_clears_backoff_and_keeps_the_streak() {
        let c = test_conn();
        seed_repo(&c, 1);
        set_tracked(&c, 1, true).unwrap();
        record_sync_err(&c, 1, "boom", Some("2026-08-02T00:00:00Z")).unwrap();
        record_sync_err(&c, 1, "boom again", Some("2026-08-03T00:00:00Z")).unwrap();

        record_sync_partial(&c, 1, "2026-08-04T00:00:00Z", "partial: releases: down").unwrap();

        let repo = &tracked_repos(&c).unwrap()[0];
        assert_eq!(repo.last_synced_at.as_deref(), Some("2026-08-04T00:00:00Z"));
        assert_eq!(repo.last_error.as_deref(), Some("partial: releases: down"));
        assert_eq!(repo.backoff_until, None, "partial data must not back off");
        assert_eq!(repo.error_streak, 2, "a partial must not move the streak");
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
    fn repo_overview_one_equals_the_all_repos_row() {
        // The single-repo query filters inside the CTEs rather than after
        // them; this pins it to the projection the dashboard already renders,
        // repo by repo, so the two can never drift apart.
        let c = test_conn();
        for id in 1..=3 {
            seed_repo(&c, id);
            set_tracked(&c, id, true).unwrap();
        }
        // Deliberately uneven: repo 1 has history and events, repo 2 has one
        // stats row and no events, repo 3 has never been synced at all.
        upsert_stats(&c, 1, "2026-08-01", &snap!(stars: Some(10), forks: Some(1))).unwrap();
        upsert_stats(
            &c,
            1,
            "2026-08-02",
            &snap!(stars: Some(12), forks: Some(2), watchers: Some(3), issues: Some(4), prs: Some(5)),
        )
        .unwrap();
        upsert_stats(&c, 2, "2026-08-03", &snap!(stars: Some(7))).unwrap();
        for title in ["a", "b"] {
            insert_event(
                &c,
                &NewEvent {
                    repo_id: 1,
                    date: "2026-08-01".into(),
                    title: title.into(),
                    notes: "".into(),
                    url: None,
                    kind: None,
                },
            )
            .unwrap();
        }

        let all = repo_overview(&c).unwrap();
        assert_eq!(all.len(), 3);
        for row in &all {
            assert_eq!(
                repo_overview_one(&c, row.repo_id).unwrap().as_ref(),
                Some(row)
            );
        }
        // Guard against the comparison passing on three identical blank rows.
        assert_eq!(all[0].event_count, 2);
        assert_eq!(all[0].date, Some("2026-08-02".into()));
        assert_eq!(all[1].stars, Some(7));
        assert_eq!(all[2].date, None);
    }

    #[test]
    fn repo_overview_one_is_none_for_a_repo_with_no_page() {
        // 404 semantics: the same predicate the all-repos query applies, so a
        // repo the dashboard does not link to has no page either.
        let c = test_conn();
        seed_repo(&c, 1); // untracked
        seed_repo(&c, 2);
        set_tracked(&c, 2, true).unwrap();
        mark_hidden(&c, &[2]).unwrap(); // tracked, but gone upstream
        seed_repo(&c, 3);
        set_tracked(&c, 3, true).unwrap();

        assert_eq!(repo_overview_one(&c, 1).unwrap(), None);
        assert_eq!(repo_overview_one(&c, 2).unwrap(), None);
        assert_eq!(repo_overview_one(&c, 999).unwrap(), None);
        assert!(repo_overview_one(&c, 3).unwrap().is_some());
        let all = repo_overview(&c).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].repo_id, 3);
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

    // ---- dense_series ------------------------------------------------------

    fn values(rows: &[(String, Option<i64>)]) -> Vec<Option<i64>> {
        rows.iter().map(|(_, v)| *v).collect()
    }

    #[test]
    fn dense_series_spans_the_whole_window_ending_today() {
        let c = test_conn();
        seed_repo(&c, 1);
        let rows = dense_series(&c, 1, Metric::Stars, 7).unwrap();
        assert_eq!(rows.len(), 7);
        assert_eq!(rows[0].0, days_ago(6));
        assert_eq!(rows[6].0, days_ago(0));
        // Strictly increasing, one slot per calendar day, no duplicates.
        let dates: Vec<&str> = rows.iter().map(|(d, _)| d.as_str()).collect();
        let mut sorted = dates.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(dates, sorted);
    }

    #[test]
    fn dense_series_carries_snapshot_metric_over_gaps() {
        // Observed on two days only; the unobserved days between them are the
        // last known value, never a hole and never zero.
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(6), &snap!(stars: Some(10))).unwrap();
        upsert_stats(&c, 1, &days_ago(2), &snap!(stars: Some(14))).unwrap();
        let rows = dense_series(&c, 1, Metric::Stars, 7).unwrap();
        assert_eq!(
            values(&rows),
            vec![
                Some(10),
                Some(10),
                Some(10),
                Some(10),
                Some(14),
                Some(14),
                Some(14)
            ]
        );
        assert!(!values(&rows).iter().any(|v| v.is_none()));
    }

    #[test]
    fn dense_series_keeps_rate_metric_gaps_as_none() {
        // A day with no traffic row means "not observed", not "zero views" —
        // the four traffic metrics must render as gaps, never carry forward.
        let c = test_conn();
        seed_repo(&c, 1);
        let (d4, d1) = (days_ago(4), days_ago(1));
        upsert_traffic_days(
            &c,
            1,
            TrafficKind::Views,
            &traffic_days(&[(&d4, 5, 3), (&d1, 8, 4)]),
        )
        .unwrap();
        let rows = dense_series(&c, 1, Metric::ViewsCount, 5).unwrap();
        assert_eq!(values(&rows), vec![Some(5), None, None, Some(8), None]);
    }

    #[test]
    fn dense_series_seeds_from_the_latest_row_before_the_window() {
        // The only observation predates the window entirely: every slot still
        // carries it, so a mid-window start is not rendered as "no data".
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(40), &snap!(stars: Some(7))).unwrap();
        upsert_stats(&c, 1, &days_ago(60), &snap!(stars: Some(3))).unwrap();
        let rows = dense_series(&c, 1, Metric::Stars, 5).unwrap();
        // Seeded from the *latest* pre-window row (7), not the earliest (3).
        assert_eq!(values(&rows), vec![Some(7); 5]);
    }

    #[test]
    fn dense_series_seed_is_superseded_by_an_in_window_row() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(40), &snap!(stars: Some(7))).unwrap();
        upsert_stats(&c, 1, &days_ago(2), &snap!(stars: Some(9))).unwrap();
        let rows = dense_series(&c, 1, Metric::Stars, 5).unwrap();
        assert_eq!(
            values(&rows),
            vec![Some(7), Some(7), Some(9), Some(9), Some(9)]
        );
    }

    #[test]
    fn dense_series_is_none_before_the_first_observation_ever() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(2), &snap!(stars: Some(9))).unwrap();
        let rows = dense_series(&c, 1, Metric::Stars, 5).unwrap();
        assert_eq!(values(&rows), vec![None, None, Some(9), Some(9), Some(9)]);
    }

    #[test]
    fn dense_series_with_no_rows_is_all_none_at_full_length() {
        let c = test_conn();
        seed_repo(&c, 1);
        let rows = dense_series(&c, 1, Metric::Stars, 30).unwrap();
        assert_eq!(rows.len(), 30);
        assert!(rows.iter().all(|(_, v)| v.is_none()));
    }

    #[test]
    fn dense_series_ignores_other_repos_rows() {
        let c = test_conn();
        seed_repo(&c, 1);
        seed_repo(&c, 2);
        upsert_stats(&c, 2, &days_ago(3), &snap!(stars: Some(99))).unwrap();
        let rows = dense_series(&c, 1, Metric::Stars, 5).unwrap();
        assert!(rows.iter().all(|(_, v)| v.is_none()), "{rows:?}");
    }

    // ---- dense_downloads_total / first_observed_date -----------------------

    fn seed_asset(conn: &Connection, repo_id: i64, date: &str, asset: &str, count: i64) {
        upsert_release_assets(
            conn,
            repo_id,
            date,
            &[AssetSnapshot {
                release_tag: "v1".into(),
                asset_name: asset.into(),
                download_count: count,
            }],
        )
        .unwrap();
    }

    #[test]
    fn downloads_total_carries_each_asset_and_sums_per_day() {
        let c = test_conn();
        seed_repo(&c, 1);
        seed_asset(&c, 1, &days_ago(3), "a.bin", 10);
        seed_asset(&c, 1, &days_ago(1), "b.bin", 5);
        seed_asset(&c, 1, &days_ago(1), "a.bin", 12);
        let rows = dense_downloads_total(&c, 1, 5).unwrap();
        // -4d: nothing observed anywhere yet; -2d: a.bin's 10 still stands even
        // with no row that day; -1d onwards: both assets.
        assert_eq!(
            values(&rows),
            vec![None, Some(10), Some(10), Some(17), Some(17)]
        );
    }

    #[test]
    fn downloads_total_ignores_other_repos_and_zero_windows() {
        let c = test_conn();
        seed_repo(&c, 1);
        seed_repo(&c, 2);
        seed_asset(&c, 2, &days_ago(1), "a.bin", 99);
        let rows = dense_downloads_total(&c, 1, 3).unwrap();
        assert!(rows.iter().all(|(_, v)| v.is_none()), "{rows:?}");
        assert!(dense_downloads_total(&c, 1, 0).unwrap().is_empty());
    }

    #[test]
    fn first_observed_date_spans_stats_and_assets() {
        let c = test_conn();
        seed_repo(&c, 1);
        assert_eq!(first_observed_date(&c, 1).unwrap(), None);
        upsert_stats(&c, 1, "2026-08-05", &snap!(stars: Some(1))).unwrap();
        assert_eq!(
            first_observed_date(&c, 1).unwrap(),
            Some("2026-08-05".into())
        );
        // An older release row moves the start back: "all" must reach the
        // earliest observation of any kind, not just of stats.
        seed_asset(&c, 1, "2026-07-01", "a.bin", 3);
        assert_eq!(
            first_observed_date(&c, 1).unwrap(),
            Some("2026-07-01".into())
        );
    }

    #[test]
    fn dense_series_of_zero_days_is_empty() {
        // No "all time" dense window exists — unlike `series`, 0 is not a
        // sentinel here, it is simply an empty range.
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(1), &snap!(stars: Some(5))).unwrap();
        assert!(dense_series(&c, 1, Metric::Stars, 0).unwrap().is_empty());
    }
}
