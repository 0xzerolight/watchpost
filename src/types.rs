//! Row/DTO types shared by `db::queries`. `GhRepo` mirrors the subset of the
//! GitHub repo API response the db layer needs; it already derives
//! `Deserialize` so Task 5's http client needs no changes to this struct.

// Most of these types/fields are only consumed by db::queries, which is
// itself unused outside tests until later tasks wire in handlers.
#![allow(dead_code)]

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
/// truncates it to the date.
#[derive(Debug, Clone)]
pub struct TrafficDay {
    pub timestamp: String,
    pub count: i64,
    pub uniques: i64,
}

/// Which `repo_stats` column a `series()` call reads.
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

#[derive(Debug, Clone)]
pub struct AssetSnapshot {
    pub release_tag: String,
    pub asset_name: String,
    pub download_count: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssetSeriesRow {
    pub date: String,
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

/// Subset of GitHub's repo API response the db layer needs. Task 5 reuses
/// this struct for its http client (already `Deserialize`); the fields
/// below that this layer doesn't write anywhere yet (stargazers_count,
/// forks_count, subscribers_count, open_issues_count) exist for that reuse.
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
