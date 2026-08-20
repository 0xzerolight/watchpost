//! Row/DTO types shared by `db::queries`. `GhRepo` mirrors the subset of the
//! GitHub repo API response the db layer needs, and derives `Deserialize` so
//! the http client can decode straight into it.

use serde::Deserialize;

/// One day's (or point-in-time's) observed counter snapshot. All fields are
/// `Option`: `None` = not observed this sync, distinct from `Some(0)` =
/// observed zero.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatSnapshot {
    pub stars: Option<i64>,
    pub forks: Option<i64>,
    pub watchers: Option<i64>,
    pub issues: Option<i64>,
    pub prs: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficKind {
    Views,
    Clones,
}

/// One day of GitHub traffic (views or clones). `timestamp` is the raw
/// GitHub API value (`2026-08-01T00:00:00Z`); `upsert_traffic_days`
/// truncates it to the date. Derives `Deserialize` for the http client's
/// `TrafficSeries`, whose JSON payload nests these directly.
#[derive(Debug, Clone, Deserialize)]
pub struct TrafficDay {
    pub timestamp: String,
    pub count: i64,
    pub uniques: i64,
}

/// Which `repo_stats` column a `dense_series()` call reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Stars,
    Forks,
    Watchers,
    Issues,
    Prs,
    ViewsCount,
    ViewsUniques,
    ClonesCount,
    ClonesUniques,
}

impl Metric {
    pub(crate) fn column(self) -> &'static str {
        match self {
            Metric::Stars => "stars",
            Metric::Forks => "forks",
            Metric::Watchers => "watchers",
            Metric::Issues => "issues",
            Metric::Prs => "prs",
            Metric::ViewsCount => "views_count",
            Metric::ViewsUniques => "views_uniques",
            Metric::ClonesCount => "clones_count",
            Metric::ClonesUniques => "clones_uniques",
        }
    }

    /// Whether an unobserved day should inherit the last observed value
    /// (see [`crate::db::queries::dense_series`]).
    ///
    /// The split is snapshot vs. rate, not cumulative vs. not. Stars, forks,
    /// watchers, issues and PRs are *level* readings: the number exists on
    /// every day whether or not watchpost looked, so a day with no row means
    /// "not measured", and the last measurement is the best answer. The four
    /// traffic columns are per-day *rates*: a missing day is unknown activity,
    /// and repeating yesterday's view count would invent traffic that may
    /// never have happened.
    pub(crate) fn carries_forward(self) -> bool {
        match self {
            Metric::Stars | Metric::Forks | Metric::Watchers | Metric::Issues | Metric::Prs => true,
            Metric::ViewsCount
            | Metric::ViewsUniques
            | Metric::ClonesCount
            | Metric::ClonesUniques => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewEvent {
    pub repo_id: i64,
    pub date: String,
    pub title: String,
    pub notes: String,
    pub url: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub id: i64,
    pub repo_id: i64,
    pub date: String,
    pub title: String,
    pub notes: String,
    pub url: Option<String>,
    pub kind: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepoRow {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub archived: bool,
    pub fork: bool,
    pub tracked: bool,
    pub hidden: bool,
    pub stars_synced: bool,
    pub last_synced_at: Option<String>,
    pub last_error: Option<String>,
    pub error_streak: i64,
    pub backoff_until: Option<String>,
}

/// Latest-row-per-repo dashboard projection (see `repo_overview`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RepoOverview {
    pub repo_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub archived: bool,
    pub fork: bool,
    pub last_synced_at: Option<String>,
    pub last_error: Option<String>,
    pub error_streak: i64,
    pub date: Option<String>,
    pub stars: Option<i64>,
    pub forks: Option<i64>,
    pub watchers: Option<i64>,
    pub issues: Option<i64>,
    pub prs: Option<i64>,
    pub event_count: i64,
}

/// A counter that moved, in the dashboard's recent-changes feed.
///
/// Only metrics whose day-over-day *difference* carries information appear
/// here. The four traffic columns deliberately do not: they are per-day rates,
/// so the day's own value already is the change and a difference between two
/// of them describes nothing that happened. That is the same snapshot-versus-
/// rate split [`Metric::carries_forward`] draws, read from the other side.
///
/// Declaration order is render order — [`recent_changes`] sorts each row's
/// deltas by it, so a repo that gained stars and lost an issue always reads
/// the same way round.
///
/// [`recent_changes`]: crate::db::queries::recent_changes
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeMetric {
    Stars,
    Forks,
    Watchers,
    Issues,
    Prs,
    Downloads,
    ContainerPulls,
}

impl ChangeMetric {
    /// The tag [`recent_changes`]'s SQL labels this metric's rows with. Also
    /// the `repo_stats` column name for the five that have one, which is why
    /// the query can build its `UNION ALL` branches from this list.
    ///
    /// [`recent_changes`]: crate::db::queries::recent_changes
    pub(crate) fn tag(self) -> &'static str {
        match self {
            ChangeMetric::Stars => "stars",
            ChangeMetric::Forks => "forks",
            ChangeMetric::Watchers => "watchers",
            ChangeMetric::Issues => "issues",
            ChangeMetric::Prs => "prs",
            ChangeMetric::Downloads => "downloads",
            ChangeMetric::ContainerPulls => "pulls",
        }
    }

    pub(crate) fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "stars" => ChangeMetric::Stars,
            "forks" => ChangeMetric::Forks,
            "watchers" => ChangeMetric::Watchers,
            "issues" => ChangeMetric::Issues,
            "prs" => ChangeMetric::Prs,
            "downloads" => ChangeMetric::Downloads,
            "pulls" => ChangeMetric::ContainerPulls,
            _ => return None,
        })
    }

    /// Singular and plural nouns for the feed line, in the words the rest of
    /// the UI already uses ("Open issues" on a dashboard card).
    pub fn labels(self) -> (&'static str, &'static str) {
        match self {
            ChangeMetric::Stars => ("star", "stars"),
            ChangeMetric::Forks => ("fork", "forks"),
            ChangeMetric::Watchers => ("watcher", "watchers"),
            ChangeMetric::Issues => ("open issue", "open issues"),
            ChangeMetric::Prs => ("open PR", "open PRs"),
            ChangeMetric::Downloads => ("download", "downloads"),
            ChangeMetric::ContainerPulls => ("container pull", "container pulls"),
        }
    }
}

/// Everything that moved for one repo on one UTC day. `deltas` is never empty
/// — a day with nothing to report produces no `RepoChange` at all.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoChange {
    pub repo_id: i64,
    pub name: String,
    pub date: String,
    pub deltas: Vec<(ChangeMetric, i64)>,
}

#[derive(Debug, Clone)]
pub struct AssetSnapshot {
    pub release_tag: String,
    pub asset_name: String,
    pub download_count: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopularKind {
    Referrers,
    Paths,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PopularItem {
    pub name: String,
    /// Page title, paths only — referrers have no title column, so it is always
    /// `None` for [`PopularKind::Referrers`].
    pub title: Option<String>,
    pub count: i64,
    pub uniques: i64,
}

/// One row of daily referrer/path traffic as reported by GitHub, ready for
/// `upsert_referrers`/`upsert_paths`. `title` is only meaningful for paths.
#[derive(Debug, Clone)]
pub struct PopularDay {
    pub name: String,
    pub title: Option<String>,
    pub count: i64,
    pub uniques: i64,
}

/// Subset of GitHub's repo API response, shared by the http client (which
/// decodes into it) and `upsert_repo` (which writes the identity fields). The
/// counter fields — stargazers_count, forks_count, subscribers_count,
/// open_issues_count — belong to neither: the collector reads them off into a
/// `StatSnapshot` for `upsert_stats`.
#[derive(Debug, Clone, Deserialize)]
pub struct GhRepo {
    pub id: i64,
    pub full_name: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub archived: bool,
    pub fork: bool,
    pub stargazers_count: i64,
    pub forks_count: i64,
    pub subscribers_count: Option<i64>,
    pub open_issues_count: i64,
}
