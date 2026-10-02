//! Markup for the dashboard: one card per tracked repo, or an empty state
//! pointing at the repo picker.
//!
//! The whole page is a single render — there are no swap targets here, so
//! nothing needs its own wrapper id. Each card carries the two hooks the client
//! sparkline binds to: a `canvas.spark` and, as its sibling, a `spark-data`
//! JSON island holding that repo's dense 30-day star values.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use maud::{Markup, html};

use crate::routes::html::{
    Notice, empty_state, error_glyph, future_timestamp, json_script_class, notice, page_header,
    plural, signed, slash_breaks, timestamp,
};
use crate::series::growth;
use crate::types::RepoOverview;

/// How many days of stars a card's sparkline shows. The array embedded per
/// card is exactly this long, gaps included.
pub const SPARK_DAYS: u32 = 30;

/// A card and the sparkline values behind it, in `repo_overview` order.
pub type Card = (RepoOverview, Vec<Option<i64>>);

/// The dashboard body, for wrapping in [`super::base`].
///
/// Page header, then one card per tracked repo. What *moved* lives on
/// /analytics: the feed runs to twenty full-width rows, and above the cards it
/// pushed the repos this page is named after off the first screen.
/// `next_sync` is only read by cards still waiting for their first sync.
pub fn index_body(cards: &[Card], next_sync: Option<DateTime<Utc>>, tz: Tz) -> Markup {
    html! {
        (page_header("Repositories", None, None))
        @if cards.is_empty() {
            (nothing_tracked())
        } @else {
            (sync_failures_notice(
                cards.iter().filter(|(repo, _)| repo.last_error.is_some()).count(),
            ))
            div class="wp-cards" {
                @for (repo, spark) in cards {
                    (repo_card(repo, spark, next_sync, tz))
                }
            }
        }
    }
}

/// The first-run state both overview pages share: nothing is tracked, and the
/// one action that changes that.
///
/// The old copy promised "stats start collecting on the next sync", which is
/// false until a repository is ticked: a new user read it as "wait", and
/// waited an hour for nothing. The link lands on the picker itself
/// (`#wp-repos`), which sits below the fold on a phone.
pub fn nothing_tracked() -> Markup {
    empty_state(
        "No repositories tracked yet — watchpost only collects the ones you pick.",
        Some(("/settings#wp-repos", "Pick repositories to track")),
    )
}

/// Whether a card has nothing to show yet: never synced, and no stats row.
///
/// Both, not `last_synced_at` alone: a repo whose first syncs failed part-way
/// can hold rows without ever having synced cleanly, and those rows are worth
/// drawing.
pub fn awaiting_first_sync(repo: &RepoOverview) -> bool {
    repo.last_synced_at.is_none() && repo.date.is_none()
}

/// One calm line when any tracked repository's last sync failed, pointing at
/// the picker, where each failure is listed. Nothing at all when none did.
///
/// On both overview pages, because Analytics is where a maintainer starts the
/// day and one ⚠ in a grid or a table is easy to scroll past; an expired token
/// fails every repository at once while the page still looked healthy. Info,
/// not Error: `role="alert"` would interrupt a screenreader on every page load
/// for a standing condition. The line carries only the count; each glyph's
/// tooltip carries the stored category, never more.
pub fn sync_failures_notice(failed: usize) -> Markup {
    let n = failed as i64;
    html! {
        @if n > 0 {
            (notice(Notice::Info, html! {
                span aria-hidden="true" { "⚠ " }
                (n) " " (plural(n, "repository", "repositories")) " failed "
                (plural(n, "its", "their")) " last sync — "
                a href="/settings#wp-repos" { "see Settings" }
            }))
        }
    }
}

/// One repo card. `spark` is the dense star series from
/// [`crate::db::queries::dense_series`] — already carried forward, so the
/// client can plot it directly and treat any remaining `null` as a genuine
/// "not yet observed" gap.
///
/// A repo nothing has been collected for yet gets one line instead of a blank
/// sparkline over three dashes: when the first numbers arrive, as a countdown
/// to the next scheduled cycle. With no scheduler running there is nothing to
/// count down to, and the line just says it is waiting.
///
/// The title link is the card's only link and, stretched over the card in CSS,
/// its whole click target: the sparkline is the most inviting thing on a card
/// and used to do nothing. The name breaks after the slash.
pub fn repo_card(
    repo: &RepoOverview,
    spark: &[Option<i64>],
    next_sync: Option<DateTime<Utc>>,
    tz: Tz,
) -> Markup {
    html! {
        article class="wp-card" {
            header class="wp-row" {
                h2 class="wp-card-title" {
                    a href=(format!("/repos/{}", repo.repo_id)) { (slash_breaks(&repo.name)) }
                }
                @if let Some(error) = &repo.last_error {
                    (error_glyph(error))
                }
            }
            @if awaiting_first_sync(repo) {
                p class="wp-card-waiting wp-muted" {
                    "Waiting for first sync"
                    @if next_sync.is_some() {
                        " · " (future_timestamp(next_sync, tz))
                    }
                }
                // Events can be logged before the first sync; a zero count is
                // not news, and "synced never" would repeat the line above.
                @if repo.event_count > 0 {
                    footer class="wp-muted wp-small" {
                        (repo.event_count) " " (plural(repo.event_count, "event", "events"))
                    }
                }
            } @else {
                div class="wp-spark" {
                    canvas class="spark" {}
                    (json_script_class("spark-data", &spark))
                }
                (spark_caption(spark))
                ul class="wp-stats" {
                    (stat("Stars", repo.stars))
                    (stat("Forks", repo.forks))
                    (stat("Open issues", repo.issues))
                }
                footer class="wp-muted wp-small" {
                    // A count only when there is one. "0 events" on most cards
                    // was the same nothing repeated down the grid, and an
                    // event count is a COUNT(*), never unobserved, so leaving
                    // the zero out hides no gap.
                    @if repo.event_count > 0 {
                        (repo.event_count) " " (plural(repo.event_count, "event", "events")) " · "
                    }
                    "synced " (timestamp(repo.last_synced_at.as_deref(), tz))
                }
            }
        }
    }
}

/// What the sparkline shows, in words: how far the stars moved over its
/// [`SPARK_DAYS`], from [`growth`] over the same series the line draws. Each
/// card scales its own line, so +2 on 13 stars draws the same cliff as +18 on
/// 103; the caption is what tells them apart. Left out when the window holds no
/// reading, which [`growth`] reports as `None`: no "+0" for "nobody looked".
fn spark_caption(spark: &[Option<i64>]) -> Markup {
    html! {
        @if let Some(n) = growth(spark) {
            p class="wp-card-caption wp-muted wp-small" {
                (signed(n)) " " (plural(n.abs(), "star", "stars")) " · " (SPARK_DAYS) " days"
            }
        }
    }
}

/// One labelled number. An unobserved counter shows an em dash rather than a
/// zero — the dashboard must not claim a repo has no stars when watchpost
/// simply has not looked yet.
fn stat(label: &str, value: Option<i64>) -> Markup {
    html! {
        li {
            span class="wp-muted wp-small" { (label) }
            strong { @match value { Some(n) => (n), None => "—" } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A card that has synced, so it draws its sparkline and stats rather
    /// than the waiting line.
    fn synced(repo: RepoOverview) -> RepoOverview {
        RepoOverview {
            last_synced_at: Some("2026-08-17T09:05:00Z".into()),
            ..repo
        }
    }

    #[test]
    fn card_embeds_the_spark_hooks_side_by_side() {
        let repo = synced(RepoOverview {
            repo_id: 7,
            name: "octo/x".into(),
            stars: Some(3),
            ..RepoOverview::default()
        });
        let out = repo_card(&repo, &[Some(1), None, Some(2)], None, Tz::UTC).into_string();

        // The canvas and its payload must be siblings, canvas first — that is
        // the relationship the client walks.
        assert!(
            out.contains(
                r#"<canvas class="spark"></canvas><script type="application/json" class="spark-data">[1,null,2]</script>"#
            ),
            "out was {out}"
        );
        assert!(out.contains(r#"href="/repos/7""#), "out was {out}");
    }

    #[test]
    fn card_title_is_a_heading_not_a_bold_paragraph() {
        // A grid of cards is a list of sections; each needs a heading for the
        // document outline. No `.wp-grow`: that utility's `flex: 1 1 auto`
        // would win over the title's zero basis and push the glyph onto a
        // line of its own under a long name.
        let repo = RepoOverview {
            repo_id: 7,
            name: "octo/x".into(),
            ..RepoOverview::default()
        };
        let out = repo_card(&repo, &[], None, Tz::UTC).into_string();
        assert!(
            out.contains(r#"<h2 class="wp-card-title"><a href="/repos/7">octo/<wbr>x</a></h2>"#),
            "out was {out}"
        );
        assert!(!out.contains("<strong class="), "out was {out}");
        // One link per card: the stretched title link is the whole card's.
        assert_eq!(out.matches("<a ").count(), 1, "out was {out}");
    }

    #[test]
    fn card_reuses_the_shared_error_glyph() {
        // The glyph's tooltip/label wiring lives in one place; a card that
        // hand-rolls it drifts out of step with the rest of the app.
        let repo = RepoOverview {
            last_error: Some("github 502".into()),
            ..RepoOverview::default()
        };
        let out = repo_card(&repo, &[], None, Tz::UTC).into_string();
        assert!(
            out.contains(&error_glyph("github 502").into_string()),
            "out was {out}"
        );
    }

    #[test]
    fn card_footer_marks_up_the_sync_time() {
        let repo = synced(RepoOverview::default());
        let out = repo_card(&repo, &[], None, Tz::UTC).into_string();
        // Coarse text to read, exact instant still in the markup.
        assert!(
            out.contains(r#"<time datetime="2026-08-17T09:05:00Z""#),
            "out was {out}"
        );
    }

    #[test]
    fn dashboard_leads_with_the_shared_page_header() {
        let out = index_body(&[], None, Tz::UTC).into_string();
        assert!(
            out.starts_with(
                r#"<header class="wp-page-header"><hgroup><h1>Repositories</h1></hgroup></header>"#
            ),
            "out was {out}"
        );
        // No subtitle: a heading that says "Repositories" over a grid of repo
        // cards has nothing left to explain.
        assert!(!out.contains("<p>Tracked"), "out was {out}");
        // And no feed: what moved lives on /analytics.
        assert!(!out.contains("wp-changes"), "out was {out}");
    }

    #[test]
    fn card_shows_a_dash_for_unobserved_counters() {
        let repo = synced(RepoOverview {
            repo_id: 1,
            name: "octo/x".into(),
            stars: None,
            ..RepoOverview::default()
        });
        let out = repo_card(&repo, &[], None, Tz::UTC).into_string();
        assert!(out.contains("<strong>—</strong>"), "out was {out}");
        assert!(!out.contains("<strong>0</strong>"), "out was {out}");
    }

    #[test]
    fn event_count_is_pluralised() {
        let one = synced(RepoOverview {
            event_count: 1,
            ..RepoOverview::default()
        });
        assert!(
            repo_card(&one, &[], None, Tz::UTC)
                .into_string()
                .contains("1 event ·")
        );
        let many = synced(RepoOverview {
            event_count: 2,
            ..RepoOverview::default()
        });
        assert!(
            repo_card(&many, &[], None, Tz::UTC)
                .into_string()
                .contains("2 events ·")
        );
    }

    #[test]
    fn empty_state_points_at_the_picker_and_draws_nothing() {
        let out = index_body(&[], None, Tz::UTC).into_string();
        assert!(
            out.contains(
                "<p>No repositories tracked yet — watchpost only collects the ones you pick.</p>"
            ),
            "out was {out}"
        );
        assert!(
            out.contains(
                r#"<a class="wp-empty-cta" href="/settings#wp-repos">Pick repositories to track</a>"#
            ),
            "out was {out}"
        );
        assert!(!out.contains("canvas"), "out was {out}");
    }

    #[test]
    fn a_never_synced_card_says_when_data_arrives_and_shows_no_zeros() {
        let repo = RepoOverview {
            repo_id: 7,
            name: "octo/x".into(),
            ..RepoOverview::default()
        };
        let next =
            chrono::Utc::now() + chrono::Duration::minutes(42) + chrono::Duration::seconds(30);
        let out = repo_card(&repo, &[None, None], Some(next), Tz::UTC).into_string();
        assert!(
            out.contains(
                r#"<p class="wp-card-waiting wp-muted">Waiting for first sync · <time datetime=""#
            ),
            "out was {out}"
        );
        assert!(out.contains(">in 42m</time></p>"), "out was {out}");
        // Nothing drawn and nothing counted: no blank line, no dashes, no
        // "0 events", no "synced never" repeating the line above.
        for absent in [
            "<canvas",
            "spark-data",
            "<strong>",
            "events",
            "synced never",
        ] {
            assert!(!out.contains(absent), "{absent} in {out}");
        }
    }

    #[test]
    fn a_never_synced_card_without_a_schedule_only_waits() {
        // No scheduler running (a failed boot-time start): no countdown, and
        // "not scheduled" is not dressed up as one.
        let out = repo_card(&RepoOverview::default(), &[], None, Tz::UTC).into_string();
        assert!(
            out.contains(r#"<p class="wp-card-waiting wp-muted">Waiting for first sync</p>"#),
            "out was {out}"
        );
        assert!(!out.contains("not scheduled"), "out was {out}");
    }

    #[test]
    fn a_never_synced_card_still_counts_its_events() {
        let repo = RepoOverview {
            event_count: 2,
            ..RepoOverview::default()
        };
        let out = repo_card(&repo, &[], None, Tz::UTC).into_string();
        assert!(
            out.contains(r#"<footer class="wp-muted wp-small">2 events</footer>"#),
            "out was {out}"
        );
    }

    #[test]
    fn a_card_with_rows_is_not_waiting_even_before_a_clean_sync() {
        // A repo whose first syncs failed part-way can hold rows; they are
        // worth drawing.
        let failed_with_rows = RepoOverview {
            date: Some("2026-08-17".into()),
            ..RepoOverview::default()
        };
        assert!(!awaiting_first_sync(&failed_with_rows));
        assert!(awaiting_first_sync(&RepoOverview::default()));
        assert!(!awaiting_first_sync(&synced(RepoOverview::default())));
    }

    fn failing(repo_id: i64) -> Card {
        let repo = synced(RepoOverview {
            repo_id,
            last_error: Some("github 502".into()),
            ..RepoOverview::default()
        });
        (repo, Vec::new())
    }

    #[test]
    fn failed_syncs_are_one_calm_line_above_the_cards() {
        let one = index_body(&[failing(1)], None, Tz::UTC).into_string();
        assert!(
            one.contains(
                r#"<p class="wp-notice wp-notice-info" role="status"><span aria-hidden="true">⚠ </span>1 repository failed its last sync — <a href="/settings#wp-repos">see Settings</a></p>"#
            ),
            "out was {one}"
        );
        assert!(
            one.find("failed its last sync").unwrap() < one.find("wp-cards").unwrap(),
            "out was {one}"
        );
        let two = index_body(&[failing(1), failing(2)], None, Tz::UTC).into_string();
        assert!(
            two.contains("2 repositories failed their last sync"),
            "out was {two}"
        );
    }

    #[test]
    fn no_failed_sync_draws_no_line() {
        let healthy: Card = (synced(RepoOverview::default()), Vec::new());
        let out = index_body(&[healthy], None, Tz::UTC).into_string();
        assert!(!out.contains("last sync"), "out was {out}");
        assert_eq!(sync_failures_notice(0).into_string(), "");
    }

    #[test]
    fn a_card_without_events_leaves_the_count_out() {
        let repo = synced(RepoOverview::default());
        let out = repo_card(&repo, &[], None, Tz::UTC).into_string();
        assert!(!out.contains("events"), "out was {out}");
        assert!(
            out.contains(r#"<footer class="wp-muted wp-small">synced <time"#),
            "out was {out}"
        );
    }

    #[test]
    fn the_sparkline_is_captioned_with_its_growth() {
        let repo = synced(RepoOverview::default());
        let caption = |spark: &[Option<i64>]| repo_card(&repo, spark, None, Tz::UTC).into_string();

        let grew = caption(&[None, Some(100), Some(118)]);
        assert!(
            grew.contains(
                r#"<p class="wp-card-caption wp-muted wp-small">+18 stars · 30 days</p>"#
            ),
            "out was {grew}"
        );
        // Under the line it describes, above the stats.
        assert!(
            grew.find("spark-data").unwrap() < grew.find("wp-card-caption").unwrap()
                && grew.find("wp-card-caption").unwrap() < grew.find("wp-stats").unwrap(),
            "out was {grew}"
        );
        assert!(caption(&[Some(5), Some(6)]).contains(">+1 star · 30 days<"));
        assert!(caption(&[Some(6), Some(5)]).contains(">\u{2212}1 star · 30 days<"));
        assert!(caption(&[Some(5), Some(5)]).contains(">\u{00b1}0 stars · 30 days<"));
        // Nothing observed in the window: no caption, not "+0".
        assert!(!caption(&[None, None]).contains("wp-card-caption"));
    }
}
