//! Markup for the dashboard: one card per tracked repo, or an empty state
//! pointing at the repo picker.
//!
//! The whole page is a single render — there are no swap targets here, so
//! nothing needs its own wrapper id. Each card carries the two hooks the client
//! sparkline binds to: a `canvas.spark` and, as its sibling, a `spark-data`
//! JSON island holding that repo's dense 30-day star values.

use maud::{Markup, html};

use crate::routes::html::{json_script_class, relative_time};
use crate::types::RepoOverview;

/// How many days of stars a card's sparkline shows. The array embedded per
/// card is exactly this long, gaps included.
pub const SPARK_DAYS: u32 = 30;

/// A card and the sparkline values behind it, in `repo_overview` order.
pub type Card = (RepoOverview, Vec<Option<i64>>);

/// The dashboard body, for wrapping in [`super::base`].
pub fn index_body(cards: &[Card]) -> Markup {
    html! {
        h1 { "Repos" }
        @if cards.is_empty() {
            article {
                p { "No repos tracked yet." }
                p class="wp-muted" {
                    "Pick the repos to watch in "
                    a href="/settings" { "settings" }
                    " — stats start collecting on the next sync."
                }
            }
        } @else {
            div class="wp-cards" {
                @for (repo, spark) in cards {
                    (repo_card(repo, spark))
                }
            }
        }
    }
}

/// One repo card. `spark` is the dense star series from
/// [`crate::db::queries::dense_series`] — already carried forward, so the
/// client can plot it directly and treat any remaining `null` as a genuine
/// "not yet observed" gap.
pub fn repo_card(repo: &RepoOverview, spark: &[Option<i64>]) -> Markup {
    html! {
        article class="wp-card" {
            header class="wp-row" {
                strong class="wp-grow" {
                    a href=(format!("/repos/{}", repo.repo_id)) { (repo.name) }
                }
                @if let Some(error) = &repo.last_error {
                    // Pico renders `data-tooltip` on hover/focus; `tabindex`
                    // makes the message reachable without a pointer, and the
                    // label keeps the bare glyph meaningful to a screenreader.
                    span class="wp-danger" data-tooltip=(error) tabindex="0"
                        role="img" aria-label=(format!("Last sync failed: {error}")) { "⚠" }
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
                " · synced " (relative_time(repo.last_synced_at.as_deref()))
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
        let out = repo_card(&repo, &[Some(1), None, Some(2)]).into_string();

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
    fn card_shows_a_dash_for_unobserved_counters() {
        let repo = RepoOverview {
            repo_id: 1,
            name: "octo/x".into(),
            stars: None,
            ..RepoOverview::default()
        };
        let out = repo_card(&repo, &[]).into_string();
        assert!(out.contains("<strong>—</strong>"), "out was {out}");
        assert!(!out.contains("<strong>0</strong>"), "out was {out}");
    }

    #[test]
    fn event_count_is_pluralised() {
        let one = RepoOverview {
            event_count: 1,
            ..RepoOverview::default()
        };
        assert!(repo_card(&one, &[]).into_string().contains("1 event ·"));
        let many = RepoOverview {
            event_count: 2,
            ..RepoOverview::default()
        };
        assert!(repo_card(&many, &[]).into_string().contains("2 events ·"));
    }

    #[test]
    fn empty_state_points_at_settings_and_draws_nothing() {
        let out = index_body(&[]).into_string();
        assert!(out.contains("No repos tracked yet"), "out was {out}");
        assert!(out.contains(r#"<a href="/settings">"#), "out was {out}");
        assert!(!out.contains("canvas"), "out was {out}");
    }
}
