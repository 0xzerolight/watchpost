//! Markup for the dashboard: one card per tracked repo, or an empty state
//! pointing at the repo picker.
//!
//! The whole page is a single render — there are no swap targets here, so
//! nothing needs its own wrapper id. Each card carries the two hooks the client
//! sparkline binds to: a `canvas.spark` and, as its sibling, a `spark-data`
//! JSON island holding that repo's dense 30-day star values.

use chrono_tz::Tz;
use maud::{Markup, html};

use crate::routes::html::{
    date_stamp, empty_state, error_glyph, json_script_class, page_header, timestamp,
};
use crate::types::{ChangeMetric, RepoChange, RepoOverview};

/// How many days of stars a card's sparkline shows. The array embedded per
/// card is exactly this long, gaps included.
pub const SPARK_DAYS: u32 = 30;

/// How far back the recent-changes feed looks.
pub const CHANGES_DAYS: u32 = 14;

/// How many days-with-movement the feed lists. A bound, not a page size:
/// there is no "show more", because the repo pages are where the full history
/// already lives.
pub const CHANGES_MAX_ROWS: usize = 20;

/// A card and the sparkline values behind it, in `repo_overview` order.
pub type Card = (RepoOverview, Vec<Option<i64>>);

/// The dashboard body, for wrapping in [`super::base`].
pub fn index_body(cards: &[Card], changes: &[RepoChange], tz: Tz) -> Markup {
    html! {
        (page_header("Repositories", None, None))
        @if cards.is_empty() {
            (empty_state(
                "No repos tracked yet — stats start collecting on the next sync.",
                Some(("/settings", "Pick repos to watch")),
            ))
        } @else {
            // Only once there is something to have changed. On an install with
            // nothing tracked the picker CTA above is the whole message, and a
            // second empty state under it would bury it.
            (changes_section(changes))
            div class="wp-cards" {
                @for (repo, spark) in cards {
                    (repo_card(repo, spark, tz))
                }
            }
        }
    }
}

/// What moved lately, newest day first.
///
/// The cards above show levels, so a reader who has not memorised yesterday's
/// star count cannot tell one apart from three days ago. Each row is one repo
/// on one UTC day, and a day with nothing to report is simply absent — see
/// [`crate::db::queries::recent_changes`] for what counts as a change.
pub fn changes_section(changes: &[RepoChange]) -> Markup {
    html! {
        section class="wp-changes" {
            h2 { "Recent changes" }
            @if changes.is_empty() {
                (empty_state(
                    "Nothing changed in the last 14 days.",
                    None,
                ))
            } @else {
                ul {
                    @for change in changes {
                        li {
                            span class="wp-change-day wp-muted wp-small" { (date_stamp(&change.date)) }
                            a class="wp-change-repo" href=(format!("/repos/{}", change.repo_id)) {
                                (change.name)
                            }
                            span class="wp-change-deltas" {
                                @for (metric, delta) in &change.deltas {
                                    (delta_chip(*metric, *delta))
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One "+3 stars" / "-1 open issue".
///
/// The sign is spelled out rather than left to colour alone, so the direction
/// survives a monochrome screen and a reader who cannot separate the two hues.
/// It is U+2212 MINUS SIGN, not a hyphen: at this size a hyphen next to a
/// digit reads as punctuation.
fn delta_chip(metric: ChangeMetric, delta: i64) -> Markup {
    let (one, many) = metric.labels();
    let class = if delta > 0 {
        "wp-delta wp-delta-up"
    } else {
        "wp-delta wp-delta-down"
    };
    html! {
        span class=(class) {
            (if delta > 0 { "+" } else { "\u{2212}" })
            (delta.abs())
            " "
            (plural(delta.abs(), one, many))
        }
    }
}

/// One repo card. `spark` is the dense star series from
/// [`crate::db::queries::dense_series`] — already carried forward, so the
/// client can plot it directly and treat any remaining `null` as a genuine
/// "not yet observed" gap.
pub fn repo_card(repo: &RepoOverview, spark: &[Option<i64>], tz: Tz) -> Markup {
    html! {
        article class="wp-card" {
            header class="wp-row" {
                h2 class="wp-card-title wp-grow" {
                    a href=(format!("/repos/{}", repo.repo_id)) { (repo.name) }
                }
                @if let Some(error) = &repo.last_error {
                    (error_glyph(error))
                }
            }
            div class="wp-spark" {
                canvas class="spark" {}
                (json_script_class("spark-data", &spark))
            }
            ul class="wp-stats" {
                (stat("Stars", repo.stars))
                (stat("Forks", repo.forks))
                (stat("Open issues", repo.issues))
            }
            footer class="wp-muted wp-small" {
                (repo.event_count) " " (plural(repo.event_count, "event", "events"))
                " · synced " (timestamp(repo.last_synced_at.as_deref(), tz))
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

fn plural(n: i64, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_embeds_the_spark_hooks_side_by_side() {
        let repo = RepoOverview {
            repo_id: 7,
            name: "octo/x".into(),
            stars: Some(3),
            ..RepoOverview::default()
        };
        let out = repo_card(&repo, &[Some(1), None, Some(2)], Tz::UTC).into_string();

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
        // document outline, and `.wp-grow` belongs on whatever the row lays out.
        let repo = RepoOverview {
            repo_id: 7,
            name: "octo/x".into(),
            ..RepoOverview::default()
        };
        let out = repo_card(&repo, &[], Tz::UTC).into_string();
        assert!(
            out.contains(r#"<h2 class="wp-card-title wp-grow"><a href="/repos/7">octo/x</a></h2>"#),
            "out was {out}"
        );
        assert!(!out.contains("<strong class="), "out was {out}");
    }

    #[test]
    fn card_reuses_the_shared_error_glyph() {
        // The glyph's tooltip/label wiring lives in one place; a card that
        // hand-rolls it drifts out of step with the rest of the app.
        let repo = RepoOverview {
            last_error: Some("github 502".into()),
            ..RepoOverview::default()
        };
        let out = repo_card(&repo, &[], Tz::UTC).into_string();
        assert!(
            out.contains(&error_glyph("github 502").into_string()),
            "out was {out}"
        );
    }

    #[test]
    fn card_footer_marks_up_the_sync_time() {
        let repo = RepoOverview {
            last_synced_at: Some("2026-08-17T09:05:00Z".into()),
            ..RepoOverview::default()
        };
        let out = repo_card(&repo, &[], Tz::UTC).into_string();
        // Coarse text to read, exact instant still in the markup.
        assert!(
            out.contains(r#"<time datetime="2026-08-17T09:05:00Z""#),
            "out was {out}"
        );
    }

    #[test]
    fn dashboard_leads_with_the_shared_page_header() {
        let out = index_body(&[], &[], Tz::UTC).into_string();
        assert!(
            out.starts_with(
                r#"<header class="wp-page-header"><hgroup><h1>Repositories</h1></hgroup></header>"#
            ),
            "out was {out}"
        );
        // No subtitle: a heading that says "Repositories" over a grid of repo
        // cards has nothing left to explain.
        assert!(!out.contains("<p>Tracked"), "out was {out}");
    }

    #[test]
    fn card_shows_a_dash_for_unobserved_counters() {
        let repo = RepoOverview {
            repo_id: 1,
            name: "octo/x".into(),
            stars: None,
            ..RepoOverview::default()
        };
        let out = repo_card(&repo, &[], Tz::UTC).into_string();
        assert!(out.contains("<strong>—</strong>"), "out was {out}");
        assert!(!out.contains("<strong>0</strong>"), "out was {out}");
    }

    #[test]
    fn event_count_is_pluralised() {
        let one = RepoOverview {
            event_count: 1,
            ..RepoOverview::default()
        };
        assert!(
            repo_card(&one, &[], Tz::UTC)
                .into_string()
                .contains("1 event ·")
        );
        let many = RepoOverview {
            event_count: 2,
            ..RepoOverview::default()
        };
        assert!(
            repo_card(&many, &[], Tz::UTC)
                .into_string()
                .contains("2 events ·")
        );
    }

    fn change(deltas: Vec<(ChangeMetric, i64)>) -> RepoChange {
        RepoChange {
            repo_id: 7,
            name: "octo/x".into(),
            date: "2026-08-19".into(),
            deltas,
        }
    }

    #[test]
    fn a_delta_spells_out_its_direction_and_pluralises() {
        let out = changes_section(&[change(vec![
            (ChangeMetric::Stars, 3),
            (ChangeMetric::Issues, -1),
        ])])
        .into_string();
        assert!(
            out.contains(r#"<span class="wp-delta wp-delta-up">+3 stars</span>"#),
            "out was {out}"
        );
        // U+2212 MINUS SIGN, not a hyphen, and the singular noun for one.
        assert!(
            out.contains("<span class=\"wp-delta wp-delta-down\">\u{2212}1 open issue</span>"),
            "out was {out}"
        );
        assert!(out.contains(r#"href="/repos/7""#), "out was {out}");
        assert!(
            out.contains(r#"<time datetime="2026-08-19">"#),
            "out was {out}"
        );
    }

    #[test]
    fn a_quiet_fortnight_says_so_instead_of_rendering_an_empty_list() {
        let out = changes_section(&[]).into_string();
        assert!(
            out.contains("<p>Nothing changed in the last 14 days.</p>"),
            "out was {out}"
        );
        assert!(!out.contains("<ul>"), "out was {out}");
    }

    #[test]
    fn the_feed_sits_above_the_cards_and_only_when_there_are_cards() {
        let repo = RepoOverview {
            repo_id: 7,
            name: "octo/x".into(),
            ..RepoOverview::default()
        };
        let with_cards = index_body(&[(repo, vec![])], &[], Tz::UTC).into_string();
        let feed = with_cards.find("wp-changes").expect("feed rendered");
        let cards = with_cards.find("wp-cards").expect("cards rendered");
        assert!(feed < cards, "out was {with_cards}");

        // Nothing tracked: the picker CTA is the whole message, so a second
        // empty state under it would only bury it.
        let untracked = index_body(&[], &[], Tz::UTC).into_string();
        assert!(!untracked.contains("wp-changes"), "out was {untracked}");
    }

    #[test]
    fn empty_state_points_at_settings_and_draws_nothing() {
        let out = index_body(&[], &[], Tz::UTC).into_string();
        assert!(
            out.contains("<p>No repos tracked yet — stats start collecting on the next sync.</p>"),
            "out was {out}"
        );
        assert!(
            out.contains(r#"<a class="wp-empty-cta" href="/settings">Pick repos to watch</a>"#),
            "out was {out}"
        );
        assert!(!out.contains("canvas"), "out was {out}");
    }
}
