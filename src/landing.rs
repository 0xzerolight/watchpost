//! The start page: which page the bare root opens on.
//!
//! The option list, the parser that validates against it and the default all
//! live here, for the reason [`crate::routes::html::period`] gives about the
//! chart period — "an option can never be offered that the parser then rejects"
//! is worth being a property of one file rather than an agreement between two
//! that can quietly drift apart.
//!
//! The stored value is a slug, not a path. A path in the settings table could
//! be hand-edited to `/`, which the redirector at the root would follow into an
//! infinite loop; a closed enum makes that unrepresentable. There is no
//! environment variable and so no `Source` enum: a start page is a preference
//! of whoever reads the pages, not a statement the deployment makes, and an
//! override nobody sets would only buy the "set by the environment, change it
//! there" panel state that [`crate::schedule`] needs and this does not.

use crate::db::queries;
use crate::state::AppState;

/// The pages the root may open on: every nav destination, in nav order.
///
/// Analytics sits between the two because it reads about the repos and
/// settings administers them. Repo detail pages are absent — they need an id,
/// and an id stored here would outlive the repo it names.
pub const LANDING_PAGES: [LandingPage; 3] = [
    LandingPage::Repos,
    LandingPage::Analytics,
    LandingPage::Settings,
];

/// Where an install opens before anyone has chosen: the dashboard, as it
/// always did.
pub const DEFAULT_LANDING: LandingPage = LandingPage::Repos;

/// A page the root can open on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandingPage {
    Repos,
    Analytics,
    Settings,
}

impl LandingPage {
    /// What goes in the database, and in the `<option value>`. Deliberately
    /// not the path: see the module doc.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Repos => "repos",
            Self::Analytics => "analytics",
            Self::Settings => "settings",
        }
    }

    /// Where the root redirects to. Never `/`, which the unit tests pin.
    pub fn path(self) -> &'static str {
        match self {
            Self::Repos => "/repos",
            Self::Analytics => "/analytics",
            Self::Settings => "/settings",
        }
    }

    /// The nav label, which is also the `<option>` text — one name for the
    /// page wherever the page is named.
    pub fn label(self) -> &'static str {
        match self {
            Self::Repos => "Repositories",
            Self::Analytics => "Analytics",
            Self::Settings => "Settings",
        }
    }

    /// The allowlist. Validated against the same array the selector renders,
    /// so the two can never disagree.
    pub fn from_slug(raw: &str) -> Option<Self> {
        LANDING_PAGES.into_iter().find(|page| page.slug() == raw)
    }
}

/// Turn whatever the settings table holds into a page.
///
/// An unrecognised slug is a warning and the default, not a failure: a
/// hand-edited database must not be able to strand the front page.
pub fn resolve(stored: Option<&str>) -> LandingPage {
    let Some(raw) = stored else {
        return DEFAULT_LANDING;
    };
    LandingPage::from_slug(raw.trim()).unwrap_or_else(|| {
        tracing::warn!(
            page = raw,
            default = DEFAULT_LANDING.slug(),
            "stored start page is not a known page; falling back to the default"
        );
        DEFAULT_LANDING
    })
}

/// The start page this install opens on right now, read fresh from the
/// database rather than cached: the settings page can change it between two
/// requests.
///
/// A failed read is the default and an `error!`, not a 500 — the same
/// treatment [`crate::schedule::current`] gives the stored interval. The front
/// page must not stop working because a settings read failed.
pub async fn current(state: &AppState) -> LandingPage {
    let stored = state
        .db
        .call(|c| queries::get_setting(c, queries::LANDING_PAGE_KEY))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "could not read the stored start page");
            None
        });
    resolve(stored.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_option_is_one_the_parser_accepts() {
        // The property the whole file exists for: the selector's options are
        // LANDING_PAGES, and so is the parser's allowlist.
        for page in LANDING_PAGES {
            assert_eq!(LandingPage::from_slug(page.slug()), Some(page));
            assert_eq!(resolve(Some(page.slug())), page);
        }
    }

    #[test]
    fn nothing_stored_is_the_repositories_dashboard() {
        assert_eq!(resolve(None), LandingPage::Repos);
        assert_eq!(DEFAULT_LANDING, LandingPage::Repos);
    }

    #[test]
    fn an_unknown_slug_falls_back_to_the_repositories_dashboard() {
        // A hand-edited database must not be able to strand the front page.
        assert_eq!(resolve(Some("")), LandingPage::Repos);
        assert_eq!(resolve(Some("dashboard")), LandingPage::Repos);
        // The path is not the stored form, so it is not accepted as one.
        assert_eq!(resolve(Some("/analytics")), LandingPage::Repos);
        assert_eq!(resolve(Some("  analytics  ")), LandingPage::Analytics);
    }

    #[test]
    fn no_landing_page_resolves_to_the_root_path() {
        // The loop guard. The root redirects to `path()`; a `path()` of "/"
        // would be an infinite redirect, so this is a property of the type
        // rather than a check the handler has to remember to make.
        for page in LANDING_PAGES {
            assert_ne!(page.path(), "/");
            assert!(page.path().starts_with('/'));
            assert!(!page.slug().contains('/'));
        }
    }

    #[test]
    fn slugs_and_paths_are_unique() {
        for (i, a) in LANDING_PAGES.iter().enumerate() {
            for b in &LANDING_PAGES[i + 1..] {
                assert_ne!(a.slug(), b.slug());
                assert_ne!(a.path(), b.path());
            }
        }
    }
}
