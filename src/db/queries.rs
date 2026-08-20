//! Query functions on top of `Db::call`'s `&Connection`.

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::errors::DbError;
use crate::types::{
    AssetSnapshot, ChangeMetric, ContainerPullRow, Event, GhRepo, Metric, NewEvent, PopularDay,
    PopularItem, PopularKind, PopularRow, ReleaseAssetRow, RepoChange, RepoOverview, RepoRow,
    StatRow, StatSnapshot, TrafficDay, TrafficKind,
};

/// Settings key holding the GitHub PAT the setup page saved.
pub const GITHUB_TOKEN_KEY: &str = "github_token";

/// Read one setting. `None` means the key was never written — not an error.
pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>, DbError> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .optional()
    .map_err(DbError::from)
}

/// Write one setting, replacing any previous value for that key.
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )?;
    Ok(())
}

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

/// Record the day's cumulative GHCR pull count for a repo. Monotonic MAX on
/// conflict, like [`upsert_release_assets`]: a scrape racing an earlier one
/// on the same day must never move the counter backwards.
pub fn upsert_container_pulls(
    conn: &Connection,
    repo_id: i64,
    date: &str,
    pull_count: i64,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO container_pulls AS t (repo_id, date, pull_count)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(repo_id, date) DO UPDATE SET
           -- pull_count is NOT NULL; scalar MAX is safe here.
           pull_count = MAX(t.pull_count, excluded.pull_count)",
        params![repo_id, date, pull_count],
    )?;
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
/// Doubling is what makes the two bounds agree: a row at the outer window's
/// edge keeps its predecessor as long as the gap between the two is at most
/// `window_days`. While a key is getting traffic that gap is one cycle.
///
/// Any observation gap wider than `2 × window_days` drops the predecessor and
/// the row restarts from baseline. A collector outage does that; so, routinely,
/// does a long-tail key — GitHub only returns referrers and paths with traffic
/// in the window, so a key that goes quiet for six weeks and comes back has no
/// rows in between. `popular_dead_key_keeps_its_first_count_and_stops_there`
/// models one with a 200-day gap. Baseline-from-zero is the more correct
/// reading in both cases: `count` is GitHub's rolling 14-day total, so the two
/// observations cover disjoint periods and the later count is all new traffic.
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

/// The five `repo_stats` columns a day-over-day difference means something
/// for. Literals, spliced into SQL below — never caller input.
const CHANGE_STAT_COLUMNS: [ChangeMetric; 5] = [
    ChangeMetric::Stars,
    ChangeMetric::Forks,
    ChangeMetric::Watchers,
    ChangeMetric::Issues,
    ChangeMetric::Prs,
];

/// What moved, per tracked repo per UTC day, over the trailing `days` window,
/// newest day first and capped at `limit` rows.
///
/// The dashboard shows levels — 41 stars, 3 forks — so noticing that three
/// stars arrived yesterday otherwise means having memorised the old number.
/// This is the difference the database has always held and nothing surfaced.
///
/// Four rules decide what counts as a change:
///
/// * **The predecessor is the last *observed* value, not the previous
///   calendar day.** Each `WHERE <col> IS NOT NULL` runs before its `LAG`
///   (SQLite evaluates `WHERE` ahead of the window functions), so a repo
///   synced Monday and Thursday reports one change on Thursday rather than a
///   phantom pair. `dense_series` renders the same gap as a flat line for
///   exactly this reason.
/// * **A first observation is not a change.** `prev IS NULL` rows are dropped:
///   the first sync of a 400-star repo is a reading, and reporting it as
///   "+400 stars" would open every fresh install with a fiction.
/// * **Zero deltas are not entries**, so a quiet day produces no row at all
///   rather than a row of noughts.
/// * **The four traffic columns are excluded.** They are per-day rates — the
///   day's value already is the change — which is the snapshot-versus-rate
///   split [`Metric::carries_forward`] draws, seen from the other side.
///
/// The `days` filter sits in the outer `SELECT`, never in the CTE. Filtering
/// before the `LAG` would strip the predecessor of the window's first row and
/// turn a level into a delta — the fake-spike-at-the-window-edge trap
/// [`update_deltas_table`] documents at length. That one can bound its CTE by
/// doubling the window because its rows are a rolling 14-day count; these are
/// running totals with no such horizon, so the CTE reads full history and the
/// outer `LIMIT` is what bounds the work that reaches the page.
///
/// `days == 0` is an empty range, not "all time" — matching [`dense_series`].
pub fn recent_changes(
    conn: &Connection,
    days: u32,
    limit: usize,
) -> Result<Vec<RepoChange>, DbError> {
    use std::collections::HashMap;

    if days == 0 || limit == 0 {
        return Ok(Vec::new());
    }
    let today = chrono::Utc::now().date_naive();
    let start = (today - chrono::Duration::days(i64::from(days) - 1)).to_string();

    // (repo_id, date) -> (name, deltas). Grouped here rather than in SQL: the
    // three sources have different shapes and only agree once they are five
    // columns wide.
    type Grouped = HashMap<(i64, String), (String, Vec<(ChangeMetric, i64)>)>;
    let mut grouped: Grouped = HashMap::new();
    let mut collect = |sql: &str| -> Result<(), DbError> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![start], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            let (repo_id, name, date, tag, delta) = row?;
            // An unknown tag cannot happen — every one is a literal from this
            // module — so a mismatch is a bug here, not bad data, and dropping
            // the row is the containable answer.
            let Some(metric) = ChangeMetric::from_tag(&tag) else {
                continue;
            };
            grouped
                .entry((repo_id, date))
                .or_insert_with(|| (name, Vec::new()))
                .1
                .push((metric, delta));
        }
        Ok(())
    };

    // repo_stats: one UNION ALL branch per column, each with its own LAG.
    let branches = CHANGE_STAT_COLUMNS
        .iter()
        .map(|m| {
            let col = m.tag();
            format!(
                "SELECT repo_id, date, '{col}' AS metric, {col} AS v,
                        LAG({col}) OVER (PARTITION BY repo_id ORDER BY date) AS prev
                 FROM repo_stats WHERE {col} IS NOT NULL"
            )
        })
        .collect::<Vec<_>>()
        .join("\n             UNION ALL\n             ");
    collect(&format!(
        "WITH obs AS (
             {branches}
         )
         SELECT o.repo_id, r.name, o.date, o.metric, o.v - o.prev
         FROM obs o JOIN repos r ON r.id = o.repo_id
         WHERE r.tracked = 1 AND r.hidden = 0
           AND o.prev IS NOT NULL AND o.v <> o.prev AND o.date >= ?1"
    ))?;

    // container_pulls: one cumulative value per repo per day.
    collect(
        "WITH obs AS (
             SELECT repo_id, date, pull_count AS v,
                    LAG(pull_count) OVER (PARTITION BY repo_id ORDER BY date) AS prev
             FROM container_pulls
         )
         SELECT o.repo_id, r.name, o.date, 'pulls', o.v - o.prev
         FROM obs o JOIN repos r ON r.id = o.repo_id
         WHERE r.tracked = 1 AND r.hidden = 0
           AND o.prev IS NOT NULL AND o.v <> o.prev AND o.date >= ?1",
    )?;

    // release_assets: per-(tag, asset) counters, summed. Only the total is a
    // fact about the repo, so rows that did not move stay in the SUM as zero
    // and the `HAVING` drops a day whose assets cancelled each other out.
    collect(
        "WITH obs AS (
             SELECT repo_id, date, download_count AS v,
                    LAG(download_count) OVER (
                        PARTITION BY repo_id, release_tag, asset_name ORDER BY date
                    ) AS prev
             FROM release_assets
         )
         SELECT o.repo_id, r.name, o.date, 'downloads', SUM(o.v - o.prev) AS delta
         FROM obs o JOIN repos r ON r.id = o.repo_id
         WHERE r.tracked = 1 AND r.hidden = 0
           AND o.prev IS NOT NULL AND o.date >= ?1
         GROUP BY o.repo_id, r.name, o.date
         HAVING delta <> 0",
    )?;

    let mut out: Vec<RepoChange> = grouped
        .into_iter()
        .map(|((repo_id, date), (name, mut deltas))| {
            deltas.sort_by_key(|(metric, _)| *metric);
            RepoChange {
                repo_id,
                name,
                date,
                deltas,
            }
        })
        .collect();
    // Newest day first, then by name so a day with several repos is stable.
    out.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.name.cmp(&b.name)));
    out.truncate(limit);
    Ok(out)
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

/// One slot per UTC day across the trailing `days` window ending today,
/// whether or not that day was observed. The single source of truth for chart
/// series shape — every page that plots `repo_stats` goes through here, so a
/// gap means the same thing everywhere.
///
/// Returning observed rows only would be the wrong shape for a chart: a
/// collector that ran on Monday and Thursday would draw those two points as
/// adjacent, silently compressing the week.
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
/// `days == 0` is an empty range, not "all time": there is no unbounded dense
/// window.
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
    //
    // Never SUM(uniques) across days: GitHub's weekly uniques deduplicates
    // visitors across days, so a sum is arithmetically wrong. This reads the
    // raw daily column with no aggregation, and must stay that way.
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

/// Per-day cumulative GHCR pull count, dense over the trailing `days` window
/// ending today — the `pulls_total` chart series.
///
/// One value per repo per day, so this is the snapshot carry-forward of
/// [`dense_series`] against a different table: seed from the newest row
/// strictly before the window, fill unobserved days with the last observed
/// value, `None` only before the first observation ever.
///
/// `days == 0` is an empty range, matching [`dense_series`].
pub fn dense_container_pulls(
    conn: &Connection,
    repo_id: i64,
    days: u32,
) -> Result<Vec<(String, Option<i64>)>, DbError> {
    if days == 0 {
        return Ok(Vec::new());
    }
    let today = chrono::Utc::now().date_naive();
    let start = today - chrono::Duration::days(i64::from(days) - 1);
    let (start_str, end_str) = (start.to_string(), today.to_string());

    // Observed rows inside the window, keyed by date for O(1) lookup while
    // walking the calendar below.
    let mut stmt = conn.prepare(
        "SELECT date, pull_count FROM container_pulls
         WHERE repo_id = ?1 AND date >= ?2 AND date <= ?3",
    )?;
    let observed: std::collections::HashMap<String, i64> = stmt
        .query_map(params![repo_id, start_str, end_str], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<_, _>>()?;

    // The pre-window seed: the newest reading before the window opens.
    let mut carried = conn
        .query_row(
            "SELECT pull_count FROM container_pulls
             WHERE repo_id = ?1 AND date < ?2
             ORDER BY date DESC LIMIT 1",
            params![repo_id, start_str],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;

    let mut out = Vec::with_capacity(days as usize);
    for offset in 0..i64::from(days) {
        let date = (start + chrono::Duration::days(offset)).to_string();
        if let Some(count) = observed.get(&date) {
            carried = Some(*count);
        }
        out.push((date, carried));
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
             UNION ALL
             SELECT MIN(date) FROM container_pulls WHERE repo_id = ?1
         )",
        params![repo_id],
        |r| r.get::<_, Option<String>>(0),
    )?;
    Ok(date)
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

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// How many days of history a repo has, counting both ends — a repo first
/// observed today spans one day, and one never synced spans none.
///
/// The measure both full-history readers share: the repo page floors it so a
/// one-column chart does not look broken, the export does not because a data
/// file has no such problem. Keeping the span itself in one place is what
/// stops the two drifting apart.
///
/// A stored date in the future (clock skew) would give a negative span; the
/// `unwrap_or(0)` catches that along with the never-synced case.
pub fn history_span(conn: &Connection, repo_id: i64) -> Result<u32, DbError> {
    let today = chrono::Utc::now().date_naive();
    let span = first_observed_date(conn, repo_id)?
        .and_then(|date| chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok())
        .map_or(0, |first| (today - first).num_days() + 1);
    Ok(u32::try_from(span).unwrap_or(0))
}

/// `PRAGMA user_version` — which migration the file is at. Stamped into the
/// JSON export so a later reader can tell which shape it is holding.
pub fn schema_version(conn: &Connection) -> Result<i64, DbError> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Every observed `repo_stats` row, oldest first.
///
/// Raw: no carry-forward and no dense calendar, unlike [`dense_series`]. An
/// unobserved column stays `NULL` and an unobserved day has no row at all,
/// which is what the storage actually holds — filling either in is a render
/// decision, and the JSON export is deliberately not a render.
pub fn export_stats(conn: &Connection, repo_id: i64) -> Result<Vec<StatRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT date, stars, forks, watchers, issues, prs,
                views_count, views_uniques, clones_count, clones_uniques
         FROM repo_stats WHERE repo_id = ?1 ORDER BY date",
    )?;
    let rows = stmt
        .query_map(params![repo_id], |r| {
            Ok(StatRow {
                date: r.get(0)?,
                stars: r.get(1)?,
                forks: r.get(2)?,
                watchers: r.get(3)?,
                issues: r.get(4)?,
                prs: r.get(5)?,
                views_count: r.get(6)?,
                views_uniques: r.get(7)?,
                clones_count: r.get(8)?,
                clones_uniques: r.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every observed release-asset reading, oldest first.
pub fn export_release_assets(
    conn: &Connection,
    repo_id: i64,
) -> Result<Vec<ReleaseAssetRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT date, release_tag, asset_name, download_count
         FROM release_assets WHERE repo_id = ?1
         ORDER BY date, release_tag, asset_name",
    )?;
    let rows = stmt
        .query_map(params![repo_id], |r| {
            Ok(ReleaseAssetRow {
                date: r.get(0)?,
                release_tag: r.get(1)?,
                asset_name: r.get(2)?,
                download_count: r.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every observed GHCR pull reading, oldest first.
pub fn export_container_pulls(
    conn: &Connection,
    repo_id: i64,
) -> Result<Vec<ContainerPullRow>, DbError> {
    let mut stmt = conn
        .prepare("SELECT date, pull_count FROM container_pulls WHERE repo_id = ?1 ORDER BY date")?;
    let rows = stmt
        .query_map(params![repo_id], |r| {
            Ok(ContainerPullRow {
                date: r.get(0)?,
                pull_count: r.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every observed referrer or path row, oldest first, deltas included.
///
/// Unlike [`popular_items`] this aggregates nothing: the daily rows are the
/// record, and the all-time rollup that page shows is one reading of them.
pub fn export_popular(
    conn: &Connection,
    repo_id: i64,
    kind: PopularKind,
) -> Result<Vec<PopularRow>, DbError> {
    // Table and key are chosen here from a two-variant enum, never from input.
    let (table, key, title) = match kind {
        PopularKind::Referrers => ("repo_referrers", "referrer", "NULL"),
        PopularKind::Paths => ("repo_popular_paths", "path", "title"),
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT date, {key}, {title}, count, uniques, count_delta, uniques_delta
         FROM {table} WHERE repo_id = ?1 ORDER BY date, {key}"
    ))?;
    let rows = stmt
        .query_map(params![repo_id], |r| {
            Ok(PopularRow {
                date: r.get(0)?,
                name: r.get(1)?,
                title: r.get(2)?,
                count: r.get(3)?,
                uniques: r.get(4)?,
                count_delta: r.get(5)?,
                uniques_delta: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Analytics
// ---------------------------------------------------------------------------

/// How many days of star history the tracked portfolio spans, counting both
/// ends — the analytics chart's "All".
///
/// Deliberately not `SELECT MIN(date) FROM repo_stats`: migration v2 dropped
/// `idx_repo_stats_date` because nothing reads that table `date`-first, and a
/// bare `MIN(date)` would scan every row in it to find one value. The correlated
/// subquery keeps one index seek per repo, which the `(repo_id, date)` primary
/// key serves, driven by the handful of rows `idx_repos_tracked` supplies.
///
/// Stars only, and only tracked visible repos: the chart plots the portfolio's
/// star total, so its "All" is the first day one of *those* repos had a star
/// count. A repo the page does not chart must not stretch its axis.
///
/// Zero when nothing has been observed, and zero rather than a negative span if
/// a stored date is somehow in the future — the same contract [`history_span`]
/// keeps.
pub fn portfolio_history_span(conn: &Connection) -> Result<u32, DbError> {
    let first: Option<String> = conn.query_row(
        "SELECT MIN(first) FROM (
             SELECT (SELECT MIN(date) FROM repo_stats s
                      WHERE s.repo_id = r.id AND s.stars IS NOT NULL) AS first
               FROM repos r
              WHERE r.tracked = 1 AND r.hidden = 0
         )",
        [],
        |r| r.get(0),
    )?;
    let today = chrono::Utc::now().date_naive();
    let span = first
        .and_then(|date| chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok())
        .map_or(0, |first| (today - first).num_days() + 1);
    Ok(u32::try_from(span).unwrap_or(0))
}

/// Release downloads to date: the sum over every `(release_tag, asset_name)` of
/// that pair's newest observed `download_count`.
///
/// Not `SUM(download_count)` over the table — that adds every day's snapshot of
/// the same cumulative counter and reports a number several times the truth. Not
/// one day's rows either: an asset with no row on the newest day was not
/// re-read, and it did not lose its downloads. This is the same
/// latest-row-per-group shape [`dense_downloads_total`] seeds itself with, which
/// is what makes this figure and the last point of that chart the same number by
/// construction rather than by agreement.
///
/// `None` for a repo whose releases have never been observed. `SUM` over no rows
/// is already `NULL`, so the distinction survives the query rather than being
/// reconstructed after it.
pub fn latest_downloads_total(conn: &Connection, repo_id: i64) -> Result<Option<i64>, DbError> {
    Ok(conn.query_row(
        "SELECT SUM(download_count) FROM (
             SELECT download_count,
                    ROW_NUMBER() OVER (PARTITION BY release_tag, asset_name
                                           ORDER BY date DESC) AS rn
               FROM release_assets
              WHERE repo_id = ?1
         ) WHERE rn = 1",
        params![repo_id],
        |r| r.get::<_, Option<i64>>(0),
    )?)
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
        // The unobserved day gets no row at all, never a row reading zero.
        let mut stmt = c
            .prepare(
                "SELECT date FROM repo_stats
                 WHERE repo_id = 1 AND stars IS NOT NULL ORDER BY date",
            )
            .unwrap();
        let dates: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(dates, ["2026-08-01", "2026-08-03"]);
        assert_eq!(get_stars(&c, 1, "2026-08-02"), None);
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
        // baseline. Documents the deliberate cost of bounding the recompute —
        // a gap this wide is what a referrer that went quiet and came back
        // looks like, GitHub having reported no rows for it in between.
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
    fn release_assets_upsert_is_monotonic() {
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
        let mut stmt = c
            .prepare(
                "SELECT date, download_count FROM release_assets
                 WHERE repo_id = 1 AND release_tag = 'v1' AND asset_name = 'app.bin'
                 ORDER BY date",
            )
            .unwrap();
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            [("2026-08-01".to_owned(), 10), ("2026-08-02".to_owned(), 25)]
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
        // Container pulls count too: a docker-only repo has nothing else.
        upsert_container_pulls(&c, 1, "2026-06-15", 8).unwrap();
        assert_eq!(
            first_observed_date(&c, 1).unwrap(),
            Some("2026-06-15".into())
        );
    }

    #[test]
    fn container_pulls_upsert_is_monotonic() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_container_pulls(&c, 1, "2026-08-19", 100).unwrap();
        upsert_container_pulls(&c, 1, "2026-08-19", 90).unwrap();
        let count: i64 = c
            .query_row(
                "SELECT pull_count FROM container_pulls WHERE repo_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 100, "a lower same-day reading must not win");
        upsert_container_pulls(&c, 1, "2026-08-19", 120).unwrap();
        let count: i64 = c
            .query_row(
                "SELECT pull_count FROM container_pulls WHERE repo_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 120);
    }

    #[test]
    fn dense_container_pulls_carries_forward() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_container_pulls(&c, 1, &days_ago(5), 40).unwrap();
        upsert_container_pulls(&c, 1, &days_ago(2), 70).unwrap();
        let rows = dense_container_pulls(&c, 1, 10).unwrap();
        assert_eq!(rows.len(), 10);
        assert_eq!(rows[0].1, None, "before the first observation stays None");
        assert_eq!(rows[4].1, Some(40), "observation day");
        assert_eq!(rows[6].1, Some(40), "a gap carries the last value");
        assert_eq!(rows[9].1, Some(70), "today carries the newest value");
    }

    #[test]
    fn dense_container_pulls_seeds_from_before_the_window() {
        let c = test_conn();
        seed_repo(&c, 1);
        upsert_container_pulls(&c, 1, &days_ago(30), 15).unwrap();
        let rows = dense_container_pulls(&c, 1, 7).unwrap();
        assert!(rows.iter().all(|(_, v)| *v == Some(15)), "{rows:?}");
        assert!(dense_container_pulls(&c, 1, 0).unwrap().is_empty());
    }

    #[test]
    fn dense_container_pulls_ignores_other_repos() {
        let c = test_conn();
        seed_repo(&c, 1);
        seed_repo(&c, 2);
        upsert_container_pulls(&c, 2, &days_ago(1), 99).unwrap();
        let rows = dense_container_pulls(&c, 1, 3).unwrap();
        assert!(rows.iter().all(|(_, v)| v.is_none()), "{rows:?}");
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

    #[test]
    fn a_setting_round_trips_and_overwrites_in_place() {
        let c = test_conn();
        assert_eq!(get_setting(&c, GITHUB_TOKEN_KEY).unwrap(), None);
        set_setting(&c, GITHUB_TOKEN_KEY, "ghp_first").unwrap();
        assert_eq!(
            get_setting(&c, GITHUB_TOKEN_KEY).unwrap().as_deref(),
            Some("ghp_first")
        );
        // Upsert, not a second row: rotating a token must not leave the old one
        // readable in the same table.
        set_setting(&c, GITHUB_TOKEN_KEY, "ghp_second").unwrap();
        assert_eq!(
            get_setting(&c, GITHUB_TOKEN_KEY).unwrap().as_deref(),
            Some("ghp_second")
        );
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    // ---- recent_changes ----------------------------------------------------

    /// `seed_repo` leaves a repo untracked, which the feed filters out. Most
    /// of these tests want a repo the dashboard would actually show.
    fn seed_tracked_repo(conn: &Connection, id: i64) {
        seed_repo(conn, id);
        set_tracked(conn, id, true).unwrap();
    }

    /// The deltas of the one row for `date`, or an empty vec if there is none.
    fn deltas_on(rows: &[RepoChange], date: &str) -> Vec<(ChangeMetric, i64)> {
        rows.iter()
            .find(|r| r.date == date)
            .map(|r| r.deltas.clone())
            .unwrap_or_default()
    }

    #[test]
    fn a_change_reports_the_delta_not_the_level() {
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(2), &snap!(stars: Some(40))).unwrap();
        upsert_stats(&c, 1, &days_ago(1), &snap!(stars: Some(43))).unwrap();

        let rows = recent_changes(&c, 7, 20).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].name, "owner/repo1");
        assert_eq!(rows[0].date, days_ago(1));
        assert_eq!(rows[0].deltas, vec![(ChangeMetric::Stars, 3)]);
    }

    #[test]
    fn a_first_observation_is_not_a_change() {
        // The first sync of a 400-star repo is a reading, not news. Without
        // this the feed opens with a fictional "+400 stars".
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(1), &snap!(stars: Some(400))).unwrap();

        assert!(recent_changes(&c, 7, 20).unwrap().is_empty());
    }

    #[test]
    fn an_unchanged_counter_is_not_a_change() {
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(2), &snap!(stars: Some(40))).unwrap();
        upsert_stats(&c, 1, &days_ago(1), &snap!(stars: Some(40))).unwrap();

        assert!(recent_changes(&c, 7, 20).unwrap().is_empty());
    }

    #[test]
    fn a_gap_compares_against_the_last_observed_day() {
        // Synced 10 days ago and again 2 days ago: one change on the day the
        // second observation landed, not a phantom on every day between.
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(10), &snap!(stars: Some(40))).unwrap();
        upsert_stats(&c, 1, &days_ago(2), &snap!(stars: Some(43))).unwrap();

        let rows = recent_changes(&c, 14, 20).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].date, days_ago(2));
        assert_eq!(rows[0].deltas, vec![(ChangeMetric::Stars, 3)]);
    }

    #[test]
    fn a_fall_is_reported() {
        // Closing issues is the useful half of an issue tracker.
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(2), &snap!(issues: Some(5))).unwrap();
        upsert_stats(&c, 1, &days_ago(1), &snap!(issues: Some(3))).unwrap();

        let rows = recent_changes(&c, 7, 20).unwrap();
        assert_eq!(rows[0].deltas, vec![(ChangeMetric::Issues, -2)]);
    }

    #[test]
    fn the_window_edge_reads_its_predecessor_from_outside_the_window() {
        // 40 stars at -20d (outside a 7-day window), 43 at -3d (inside). The
        // LAG must reach past the window edge or the first in-window row reads
        // as "+43 stars" — the same trap `update_deltas_recent` documents.
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(20), &snap!(stars: Some(40))).unwrap();
        upsert_stats(&c, 1, &days_ago(3), &snap!(stars: Some(43))).unwrap();

        let rows = recent_changes(&c, 7, 20).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].deltas, vec![(ChangeMetric::Stars, 3)]);
    }

    #[test]
    fn rate_metrics_never_appear() {
        // Views and clones are per-day rates: the day's value already is the
        // change, so a difference between two of them describes nothing.
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        upsert_traffic_days(
            &c,
            1,
            TrafficKind::Views,
            &traffic_days(&[(&days_ago(2), 10, 5), (&days_ago(1), 90, 40)]),
        )
        .unwrap();
        upsert_traffic_days(
            &c,
            1,
            TrafficKind::Clones,
            &traffic_days(&[(&days_ago(2), 3, 2), (&days_ago(1), 11, 7)]),
        )
        .unwrap();

        assert!(recent_changes(&c, 7, 20).unwrap().is_empty());
    }

    #[test]
    fn everything_that_moved_on_one_day_is_one_row_in_metric_order() {
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        let before = days_ago(2);
        let after = days_ago(1);
        upsert_stats(
            &c,
            1,
            &before,
            &snap!(stars: Some(40), forks: Some(2), issues: Some(5)),
        )
        .unwrap();
        upsert_stats(
            &c,
            1,
            &after,
            &snap!(stars: Some(43), forks: Some(2), issues: Some(4)),
        )
        .unwrap();
        upsert_container_pulls(&c, 1, &before, 100).unwrap();
        upsert_container_pulls(&c, 1, &after, 142).unwrap();

        let rows = recent_changes(&c, 7, 20).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        // Declaration order of ChangeMetric, and forks (unchanged) absent.
        assert_eq!(
            rows[0].deltas,
            vec![
                (ChangeMetric::Stars, 3),
                (ChangeMetric::Issues, -1),
                (ChangeMetric::ContainerPulls, 42),
            ]
        );
    }

    #[test]
    fn download_deltas_sum_the_assets_that_moved() {
        // Two assets under one tag; only the sum is a fact about the repo.
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        let before = days_ago(2);
        let after = days_ago(1);
        let assets = |linux: i64, mac: i64| {
            vec![
                AssetSnapshot {
                    release_tag: "v1".into(),
                    asset_name: "linux".into(),
                    download_count: linux,
                },
                AssetSnapshot {
                    release_tag: "v1".into(),
                    asset_name: "mac".into(),
                    download_count: mac,
                },
            ]
        };
        upsert_release_assets(&c, 1, &before, &assets(10, 5)).unwrap();
        upsert_release_assets(&c, 1, &after, &assets(40, 17)).unwrap();

        let rows = recent_changes(&c, 7, 20).unwrap();
        assert_eq!(
            deltas_on(&rows, &after),
            vec![(ChangeMetric::Downloads, 42)]
        );
    }

    #[test]
    fn a_new_asset_enters_the_feed_on_its_second_reading() {
        // A release first seen today has no predecessor, so its whole backlog
        // is a reading rather than a day's downloads. It starts contributing
        // once there are two observations to subtract.
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        let first = days_ago(2);
        let second = days_ago(1);
        let asset = |n: i64| {
            vec![AssetSnapshot {
                release_tag: "v1".into(),
                asset_name: "linux".into(),
                download_count: n,
            }]
        };
        upsert_release_assets(&c, 1, &first, &asset(500)).unwrap();
        upsert_release_assets(&c, 1, &second, &asset(507)).unwrap();

        let rows = recent_changes(&c, 7, 20).unwrap();
        assert!(deltas_on(&rows, &first).is_empty(), "{rows:?}");
        assert_eq!(
            deltas_on(&rows, &second),
            vec![(ChangeMetric::Downloads, 7)]
        );
    }

    #[test]
    fn download_moves_that_cancel_out_are_not_a_change() {
        // Two assets, one up 5 and one down 5: nothing happened to the repo's
        // downloads, so the day does not appear.
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        let before = days_ago(2);
        let after = days_ago(1);
        // Seeded directly: `upsert_release_assets` is monotonic and would
        // refuse to record the fall this test needs.
        for (date, linux, mac) in [(&before, 10, 20), (&after, 15, 15)] {
            for (name, n) in [("linux", linux), ("mac", mac)] {
                c.execute(
                    "INSERT INTO release_assets (repo_id, date, release_tag, asset_name, download_count)
                     VALUES (1, ?1, 'v1', ?2, ?3)",
                    params![date, name, n],
                )
                .unwrap();
            }
        }

        assert!(recent_changes(&c, 7, 20).unwrap().is_empty());
    }

    #[test]
    fn untracked_and_hidden_repos_never_appear() {
        let c = test_conn();
        seed_repo(&c, 1); // tracked = 0
        seed_tracked_repo(&c, 2);
        mark_hidden(&c, &[2]).unwrap();
        for id in [1, 2] {
            upsert_stats(&c, id, &days_ago(2), &snap!(stars: Some(40))).unwrap();
            upsert_stats(&c, id, &days_ago(1), &snap!(stars: Some(43))).unwrap();
        }

        assert!(recent_changes(&c, 7, 20).unwrap().is_empty());
    }

    #[test]
    fn rows_are_newest_first_and_bounded_by_the_limit() {
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        // Stars climbing by one a day for five days: four changes.
        for (offset, stars) in (1..=5).rev().zip(40..) {
            upsert_stats(&c, 1, &days_ago(offset), &snap!(stars: Some(stars))).unwrap();
        }

        let all = recent_changes(&c, 7, 20).unwrap();
        assert_eq!(all.len(), 4, "{all:?}");
        let dates: Vec<&str> = all.iter().map(|r| r.date.as_str()).collect();
        let mut sorted = dates.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(dates, sorted, "newest first");

        let capped = recent_changes(&c, 7, 2).unwrap();
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].date, all[0].date, "the limit keeps the newest");
    }

    #[test]
    fn a_zero_day_window_is_empty() {
        // Matches `dense_series`: 0 is an empty range, never an "all time"
        // sentinel.
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(2), &snap!(stars: Some(40))).unwrap();
        upsert_stats(&c, 1, &days_ago(1), &snap!(stars: Some(43))).unwrap();

        assert!(recent_changes(&c, 0, 20).unwrap().is_empty());
    }

    // ---- portfolio_history_span / latest_downloads_total -------------------

    /// One asset under a named release tag, for the cases that care which
    /// group a row belongs to. [`seed_asset`] hardcodes `v1`.
    fn seed_tagged_asset(conn: &Connection, repo_id: i64, date: &str, tag: &str, count: i64) {
        upsert_release_assets(
            conn,
            repo_id,
            date,
            &[AssetSnapshot {
                release_tag: tag.into(),
                asset_name: "app.tar".into(),
                download_count: count,
            }],
        )
        .unwrap();
    }

    #[test]
    fn portfolio_span_covers_the_earliest_tracked_star_row() {
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        seed_tracked_repo(&c, 2);
        upsert_stats(&c, 1, &days_ago(99), &snap!(stars: Some(10))).unwrap();
        upsert_stats(&c, 2, &days_ago(9), &snap!(stars: Some(3))).unwrap();

        // Both ends counted: 99 days back plus today is 100 slots.
        assert_eq!(portfolio_history_span(&c).unwrap(), 100);
    }

    #[test]
    fn portfolio_span_ignores_untracked_and_hidden_repos() {
        let c = test_conn();
        // Untracked: seeded but never `set_tracked`.
        seed_repo(&c, 1);
        upsert_stats(&c, 1, &days_ago(399), &snap!(stars: Some(900))).unwrap();
        // Tracked upstream once, hidden since.
        seed_tracked_repo(&c, 2);
        mark_hidden(&c, &[2]).unwrap();
        upsert_stats(&c, 2, &days_ago(299), &snap!(stars: Some(500))).unwrap();
        seed_tracked_repo(&c, 3);
        upsert_stats(&c, 3, &days_ago(9), &snap!(stars: Some(3))).unwrap();

        // A repo the page does not chart must not stretch its axis.
        assert_eq!(portfolio_history_span(&c).unwrap(), 10);
    }

    #[test]
    fn portfolio_span_is_zero_with_nothing_observed() {
        let c = test_conn();
        assert_eq!(portfolio_history_span(&c).unwrap(), 0);

        seed_tracked_repo(&c, 1);
        assert_eq!(portfolio_history_span(&c).unwrap(), 0);

        // A row exists but its star count does not: not observed is not zero.
        upsert_stats(&c, 1, &days_ago(5), &snap!(forks: Some(2))).unwrap();
        assert_eq!(portfolio_history_span(&c).unwrap(), 0);
    }

    #[test]
    fn latest_downloads_total_sums_the_newest_row_per_asset() {
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        for (day, one, two) in [(3, 10, 100), (2, 12, 100), (1, 15, 140)] {
            seed_asset(&c, 1, &days_ago(day), "a.tar", one);
            seed_asset(&c, 1, &days_ago(day), "b.tar", two);
        }

        // 15 + 140, not the 377 a bare SUM over six cumulative snapshots gives.
        assert_eq!(latest_downloads_total(&c, 1).unwrap(), Some(155));
    }

    #[test]
    fn latest_downloads_total_carries_an_asset_that_stopped_being_read() {
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        seed_tagged_asset(&c, 1, &days_ago(9), "v1", 500);
        seed_tagged_asset(&c, 1, &days_ago(1), "v2", 4);

        // The tag is part of the group, so an old release with no row on the
        // newest day was not re-read — it did not lose its downloads.
        assert_eq!(latest_downloads_total(&c, 1).unwrap(), Some(504));
    }

    #[test]
    fn latest_downloads_total_is_none_without_releases() {
        let c = test_conn();
        seed_tracked_repo(&c, 1);
        // An empty cell, not a confident zero.
        assert_eq!(latest_downloads_total(&c, 1).unwrap(), None);
    }
}
