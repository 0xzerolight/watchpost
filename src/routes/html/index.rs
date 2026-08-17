//! Markup for the dashboard: one card per tracked repo, or an empty state
//! pointing at the repo picker.
//!
//! The whole page is a single render — there are no swap targets here, so
//! nothing needs its own wrapper id. Each card carries the two hooks the client
//! sparkline binds to: a `canvas.spark` and, as its sibling, a `spark-data`
//! JSON island holding that repo's dense 30-day star values.

use maud::{Markup, html};

use crate::routes::html::json_script_class;
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

/// A coarse "3h ago" for a stored RFC 3339 timestamp.
///
/// Deliberately lossy: on a dashboard the useful question is whether a repo
/// synced recently, and an exact timestamp forces the reader to do the
/// subtraction. Anything that does not parse falls back to the stored string,
/// so a malformed value is visible rather than silently rendered as "never".
/// A timestamp in the future (clock skew) reads as "just now" rather than a
/// negative age.
fn relative_time(at: Option<&str>) -> String {
    let Some(at) = at else {
        return "never".to_owned();
    };
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(at) else {
        return at.to_owned();
    };
    let elapsed = chrono::Utc::now().signed_duration_since(then.with_timezone(&chrono::Utc));
    let (minutes, hours, days) = (
        elapsed.num_minutes(),
        elapsed.num_hours(),
        elapsed.num_days(),
    );
    if minutes < 1 {
        "just now".to_owned()
    } else if hours < 1 {
        format!("{minutes}m ago")
    } else if days < 1 {
        format!("{hours}h ago")
    } else {
        format!("{days}d ago")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn ago(d: Duration) -> String {
        (Utc::now() - d).to_rfc3339()
    }

    #[test]
    fn relative_time_buckets_by_magnitude() {
        assert_eq!(relative_time(None), "never");
        assert_eq!(relative_time(Some(&ago(Duration::seconds(5)))), "just now");
        assert_eq!(relative_time(Some(&ago(Duration::minutes(7)))), "7m ago");
        assert_eq!(relative_time(Some(&ago(Duration::minutes(59)))), "59m ago");
        assert_eq!(relative_time(Some(&ago(Duration::hours(3)))), "3h ago");
        assert_eq!(relative_time(Some(&ago(Duration::hours(23)))), "23h ago");
        assert_eq!(relative_time(Some(&ago(Duration::days(4)))), "4d ago");
    }

    #[test]
    fn relative_time_survives_bad_input() {
        // A future timestamp is clock skew, not a negative age.
        assert_eq!(relative_time(Some(&ago(-Duration::hours(2)))), "just now");
        // Unparseable values are shown as stored rather than swallowed.
        assert_eq!(relative_time(Some("not a date")), "not a date");
    }

    #[test]
    fn relative_time_reads_non_utc_offsets() {
        // Stored values are UTC today, but an offset timestamp must still be
        // compared as an instant, not as wall-clock digits.
        let then = (Utc::now() - Duration::hours(2))
            .with_timezone(&chrono::FixedOffset::east_opt(5 * 3600).unwrap())
            .to_rfc3339();
        assert_eq!(relative_time(Some(&then)), "2h ago");
    }

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
