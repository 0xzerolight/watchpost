//! The collection schedule: how often a cycle runs, and where that answer
//! comes from.
//!
//! Two shapes, because there are two ways to say it. `WATCHPOST_CRON` and the
//! built-in default are cron expressions — the default deliberately fires at
//! five past the hour rather than on the hour. The settings page writes an
//! interval instead, because "every 10 minutes" is the question an operator
//! actually has and a six-field cron expression is not the answer they should
//! have to write.
//!
//! Parsing lives at the top of this file and scheduler I/O at the bottom, so
//! everything that decides *what* the schedule is can be tested without a
//! runtime.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};

use crate::collector::try_run_cycle;
use crate::config::DEFAULT_CRON;
use crate::db::queries;
use crate::state::AppState;

/// The shortest interval the settings page accepts.
///
/// Below this a cycle can still be running when the next one is due. Nothing
/// breaks — [`crate::collector::try_run_cycle`] drops the overlapping tick —
/// but the schedule stops describing what happens, and a setting that lies is
/// worse than one that refuses.
pub const MIN_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// The longest interval the settings page accepts.
///
/// GitHub keeps traffic data for fourteen days, so a gap wider than that drops
/// days permanently — watchpost exists to outlive that window, and a setting
/// that quietly defeats it is not a setting worth offering.
pub const MAX_INTERVAL: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// Why an interval was rejected. The `Display` text is what the settings page
/// shows under the field, so each message names the fix rather than the rule.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntervalError {
    #[error("Enter an interval, for example 30m or 1h 30m.")]
    Empty,
    #[error("\"{0}\" is not a duration. Use a number and one of m, h, d, w — for example 30m.")]
    BadTerm(String),
    #[error("The shortest interval is 5m.")]
    TooShort,
    #[error("The longest interval is 14d — GitHub keeps traffic data for 14 days.")]
    TooLong,
}

/// Parse `10m`, `6h`, `1h 30m`.
///
/// Terms add rather than override, so `1h 30m` and `90m` are the same ninety
/// minutes and `1d 1d` is two days. Units are lowercase only: `1M` and `1m`
/// differ by 720x, and a parser that accepts both makes that typo invisible.
pub fn parse_interval(raw: &str) -> Result<Duration, IntervalError> {
    let mut secs: u64 = 0;
    let mut seen = false;
    for term in raw.split_whitespace() {
        seen = true;
        secs = secs
            .checked_add(term_seconds(term)?)
            .ok_or(IntervalError::TooLong)?;
    }
    if !seen {
        return Err(IntervalError::Empty);
    }
    let interval = Duration::from_secs(secs);
    if interval < MIN_INTERVAL {
        return Err(IntervalError::TooShort);
    }
    if interval > MAX_INTERVAL {
        return Err(IntervalError::TooLong);
    }
    Ok(interval)
}

/// One `<number><unit>` term, in seconds.
fn term_seconds(term: &str) -> Result<u64, IntervalError> {
    let bad = || IntervalError::BadTerm(term.to_owned());
    let unit = match term.chars().next_back() {
        Some('m') => 60u64,
        Some('h') => 60 * 60,
        Some('d') => 24 * 60 * 60,
        Some('w') => 7 * 24 * 60 * 60,
        _ => return Err(bad()),
    };
    // The unit matched above is ASCII, so this is always a char boundary.
    let digits = &term[..term.len() - 1];
    let count: u64 = digits.parse().map_err(|_| bad())?;
    // An overflow here is over the ceiling by any reading; saying so beats
    // wrapping into a legal-looking value.
    count.checked_mul(unit).ok_or(IntervalError::TooLong)
}

/// How often a cycle runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    /// A six-field cron expression — the environment's, or the default.
    Cron(String),
    /// A fixed interval, measured from the moment the job was installed.
    Every(Duration),
}

impl std::fmt::Display for Schedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Schedule::Cron(expr) => write!(f, "cron {expr}"),
            Schedule::Every(every) => write!(f, "every {}", humanize(*every)),
        }
    }
}

/// A duration as the largest whole unit that divides it exactly.
///
/// Only exact divisions collapse: ninety minutes stays `90m` rather than
/// becoming a lossy `1.5h`. Every unit is a whole number of minutes, so the
/// final arm never truncates.
fn humanize(interval: Duration) -> String {
    let secs = interval.as_secs();
    for (unit, size) in [("w", 604_800u64), ("d", 86_400), ("h", 3_600)] {
        if secs.is_multiple_of(size) {
            return format!("{}{unit}", secs / size);
        }
    }
    format!("{}m", secs / 60)
}

/// Which of the three places the schedule came from.
///
/// The distinction is what the settings page needs, for the same reason
/// [`crate::config::TokenSource`] exists: an environment-supplied schedule
/// cannot be replaced from a browser, because the next boot would read the
/// environment again and overwrite whatever was saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleSource {
    Env,
    Database,
    Default,
}

/// The schedule an install resolves to right now.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub schedule: Schedule,
    pub source: ScheduleSource,
    /// The stored interval exactly as it was typed, for the field to show
    /// back. `None` when nothing is stored, or when the environment owns the
    /// schedule and the stored value is therefore not in force.
    pub stored: Option<String>,
}

/// A cron expression the scheduler will actually accept.
///
/// A bad `WATCHPOST_CRON` must not leave the service collecting nothing at
/// all, so an unparseable expression is logged and replaced by the default.
/// Probe-parsed by building a throwaway job, because that is the only parser
/// guaranteed to agree with the one the scheduler uses.
pub fn valid_cron(input: &str) -> String {
    match Job::new_async(input, |_id, _sched| Box::pin(async {})) {
        Ok(_) => input.to_string(),
        Err(e) => {
            tracing::warn!(
                schedule = input,
                error = %e,
                default = DEFAULT_CRON,
                "invalid cron schedule; falling back to the default"
            );
            DEFAULT_CRON.to_string()
        }
    }
}

/// Pick the schedule to run on.
///
/// The environment wins, exactly as it does for the token
/// ([`crate::config::resolve_token`]): it is the deployment's own statement of
/// intent, it survives a lost database, and a compose file that sets it would
/// otherwise silently disagree with a value saved from a browser.
///
/// A stored interval that no longer parses is a warning and the default, not a
/// failure — a hand-edited database must not be able to stop collection.
pub fn resolve_schedule(env: Option<&str>, stored: Option<&str>) -> (Schedule, ScheduleSource) {
    if let Some(raw) = env {
        return (Schedule::Cron(valid_cron(raw)), ScheduleSource::Env);
    }
    if let Some(raw) = stored {
        match parse_interval(raw) {
            Ok(every) => return (Schedule::Every(every), ScheduleSource::Database),
            Err(e) => tracing::warn!(
                interval = raw,
                error = %e,
                "stored sync interval is invalid; falling back to the default"
            ),
        }
    }
    (
        Schedule::Cron(DEFAULT_CRON.to_string()),
        ScheduleSource::Default,
    )
}

/// The schedule a given environment and stored interval resolve to.
///
/// Split out of [`current`] so a caller holding a value it has already read can
/// build the same answer without a second trip to the blocking pool. The
/// settings page is that caller: it reads every key it needs in one
/// [`crate::db::Db::call`].
pub fn resolved(env: Option<&str>, stored: Option<String>) -> Resolved {
    let (schedule, source) = resolve_schedule(env, stored.as_deref());
    Resolved {
        schedule,
        source,
        // Dropped unless it is the interval actually in force. A value the
        // environment is overriding is still in the database and comes back if
        // the environment stops setting one, but reporting it here would put
        // it in a field describing a schedule that is not running.
        stored: stored.filter(|_| source == ScheduleSource::Database),
    }
}

/// The schedule this install resolves to right now, read fresh from the
/// database rather than cached: the settings page can change it between two
/// requests, and a cached answer would render a panel that disagrees with the
/// scheduler.
pub async fn current(state: &AppState) -> Resolved {
    // A settings read that fails must not stop the page rendering or the
    // scheduler starting — the same treatment the stored token gets at boot.
    let stored = state
        .db
        .call(|c| queries::get_setting(c, queries::SYNC_INTERVAL_KEY))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "could not read the stored sync interval");
            None
        });
    resolved(state.cfg.cron_schedule.as_deref(), stored)
}

/// The collection job, built for whichever shape of schedule this is.
fn collection_job(state: Arc<AppState>, schedule: &Schedule) -> Result<Job, JobSchedulerError> {
    let run = move |_id, _sched| {
        let state = Arc::clone(&state);
        Box::pin(async move {
            try_run_cycle(state).await;
        }) as Pin<Box<dyn Future<Output = ()> + Send>>
    };
    match schedule {
        Schedule::Cron(expr) => Job::new_async(expr.as_str(), run),
        Schedule::Every(every) => Job::new_repeated_async(*every, run),
    }
}

/// Build the scheduler, register the collection job, and hand the handle to
/// `state` so the settings page can replace the job without a restart.
pub async fn start(state: Arc<AppState>) -> Result<(), JobSchedulerError> {
    let resolved = current(&state).await;
    let scheduler = JobScheduler::new().await?;
    let job = scheduler
        .add(collection_job(Arc::clone(&state), &resolved.schedule)?)
        .await?;
    scheduler.start().await?;
    tracing::info!(
        schedule = %resolved.schedule,
        source = ?resolved.source,
        "collection scheduled"
    );
    state.install_scheduler(scheduler, job).await;
    Ok(())
}

/// Swap the running job for one on `schedule`.
///
/// `Ok(false)` means there was no scheduler to swap: the boot-time start
/// failed and the process is serving without cron. The caller has already
/// stored the setting, so it takes effect on the next restart — that is a
/// notice, not an error.
///
/// The add comes before the remove on purpose. For the moment between them two
/// jobs are registered, which costs nothing — [`try_run_cycle`] drops an
/// overlapping tick — while the other order would leave an install collecting
/// on nothing at all if the add failed.
pub async fn reschedule(
    state: &Arc<AppState>,
    schedule: &Schedule,
) -> Result<bool, JobSchedulerError> {
    // The lock is held across both awaits so two saves cannot interleave into
    // two surviving jobs.
    let mut slot = state.scheduler.lock().await;
    let Some(running) = slot.as_mut() else {
        return Ok(false);
    };
    let job = running
        .scheduler
        .add(collection_job(Arc::clone(state), schedule)?)
        .await?;
    running.scheduler.remove(&running.job).await?;
    running.job = job;
    tracing::info!(schedule = %schedule, "collection rescheduled");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unit_parses_to_its_own_length() {
        assert_eq!(parse_interval("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_interval("6h").unwrap(), Duration::from_secs(6 * 3600));
        assert_eq!(parse_interval("1d").unwrap(), Duration::from_secs(86_400));
        assert_eq!(parse_interval("1w").unwrap(), Duration::from_secs(604_800));
    }

    /// The whole point of the compound form: terms add, they do not override.
    #[test]
    fn compound_terms_add_up() {
        let ninety = Duration::from_secs(90 * 60);
        assert_eq!(parse_interval("1h 30m").unwrap(), ninety);
        assert_eq!(parse_interval("90m").unwrap(), ninety);
        assert_eq!(parse_interval("  1h\t30m  ").unwrap(), ninety);
        // Repeating a unit sums rather than replaces, which is the same rule.
        assert_eq!(
            parse_interval("1d 1d").unwrap(),
            Duration::from_secs(2 * 86_400)
        );
    }

    #[test]
    fn a_blank_field_is_its_own_error() {
        assert_eq!(parse_interval(""), Err(IntervalError::Empty));
        assert_eq!(parse_interval("   "), Err(IntervalError::Empty));
    }

    #[test]
    fn a_term_needs_a_number_and_a_known_unit() {
        for raw in [
            "10",
            "m",
            "h",
            "ten m",
            "10 minutes",
            "-5m",
            "1.5h",
            "10s",
            "1y",
        ] {
            assert!(
                matches!(parse_interval(raw), Err(IntervalError::BadTerm(_))),
                "{raw:?} must be rejected"
            );
        }
        // One bad term poisons the whole field rather than being skipped.
        assert!(matches!(
            parse_interval("1h banana"),
            Err(IntervalError::BadTerm(_))
        ));
    }

    /// `1M` is the minute/month typo. Accepting uppercase would make it a 720x
    /// mistake the field could never warn about, so the unit letters are
    /// lowercase and nothing else.
    #[test]
    fn units_are_lowercase_only() {
        for raw in ["1M", "1H", "1D", "1W"] {
            assert!(
                matches!(parse_interval(raw), Err(IntervalError::BadTerm(_))),
                "{raw:?} must be rejected"
            );
        }
    }

    #[test]
    fn bounds_are_rejections_not_clamps() {
        assert_eq!(parse_interval("4m"), Err(IntervalError::TooShort));
        assert_eq!(parse_interval("0m"), Err(IntervalError::TooShort));
        assert_eq!(parse_interval("15d"), Err(IntervalError::TooLong));
        assert_eq!(parse_interval("2w 1d"), Err(IntervalError::TooLong));
        // The edges themselves are legal.
        assert_eq!(parse_interval("5m").unwrap(), MIN_INTERVAL);
        assert_eq!(parse_interval("14d").unwrap(), MAX_INTERVAL);
        assert_eq!(parse_interval("2w").unwrap(), MAX_INTERVAL);
    }

    /// A number big enough to overflow the seconds arithmetic is over the
    /// ceiling by any reading, so it must answer that rather than wrap.
    #[test]
    fn an_overflowing_term_reads_as_too_long() {
        // Fits in u64 on its own; overflows once multiplied into seconds.
        assert_eq!(
            parse_interval("18446744073709551615m"),
            Err(IntervalError::TooLong)
        );
        // Fits and does not overflow, but is far past the ceiling.
        assert_eq!(parse_interval("999999999999w"), Err(IntervalError::TooLong));
        // Too many digits to be a number at all, which is a malformed term
        // rather than a large one.
        assert!(matches!(
            parse_interval("99999999999999999999w"),
            Err(IntervalError::BadTerm(_))
        ));
    }

    #[test]
    fn the_error_messages_name_the_fix() {
        assert!(IntervalError::TooShort.to_string().contains("5m"));
        assert!(IntervalError::TooLong.to_string().contains("14d"));
        assert!(
            IntervalError::BadTerm("1y".into())
                .to_string()
                .contains("1y")
        );
    }

    #[test]
    fn valid_cron_passes_a_good_expression_through() {
        assert_eq!(valid_cron("0 */5 * * * *"), "0 */5 * * * *");
    }

    #[test]
    fn valid_cron_falls_back_on_a_bad_expression() {
        assert_eq!(valid_cron("garbage"), DEFAULT_CRON);
        assert_eq!(valid_cron(""), DEFAULT_CRON);
        // Five fields: seconds are required, so this is not a valid schedule.
        assert_eq!(valid_cron("5 * * * *"), DEFAULT_CRON);
    }

    /// The environment is the deployment's own statement of intent — the same
    /// reason it wins for the token.
    #[test]
    fn the_environment_wins_over_a_stored_interval() {
        let (schedule, source) = resolve_schedule(Some("0 */10 * * * *"), Some("6h"));
        assert_eq!(schedule, Schedule::Cron("0 */10 * * * *".into()));
        assert_eq!(source, ScheduleSource::Env);
    }

    #[test]
    fn a_stored_interval_is_used_when_the_environment_is_silent() {
        let (schedule, source) = resolve_schedule(None, Some("6h"));
        assert_eq!(schedule, Schedule::Every(Duration::from_secs(6 * 3600)));
        assert_eq!(source, ScheduleSource::Database);
    }

    /// An install that has set nothing keeps the cron default, not a one-hour
    /// repeating job: the default fires at :05 past, and a repeating job would
    /// fire at whatever minute the process happened to boot.
    #[test]
    fn setting_nothing_keeps_the_cron_default() {
        let (schedule, source) = resolve_schedule(None, None);
        assert_eq!(schedule, Schedule::Cron(DEFAULT_CRON.into()));
        assert_eq!(source, ScheduleSource::Default);
    }

    /// A stored value that no longer parses must not stop collection — the
    /// same non-fatal treatment a bad WATCHPOST_CRON gets.
    #[test]
    fn an_unparseable_stored_interval_falls_back_to_the_default() {
        let (schedule, source) = resolve_schedule(None, Some("banana"));
        assert_eq!(schedule, Schedule::Cron(DEFAULT_CRON.into()));
        assert_eq!(source, ScheduleSource::Default);
    }

    /// A bad expression in the environment still reports Env: the panel has to
    /// say the environment owns the setting even when its value was replaced.
    #[test]
    fn a_bad_environment_expression_still_reports_env() {
        let (schedule, source) = resolve_schedule(Some("garbage"), None);
        assert_eq!(schedule, Schedule::Cron(DEFAULT_CRON.into()));
        assert_eq!(source, ScheduleSource::Env);
    }

    /// The settings page builds its panel from `resolved` rather than
    /// `current`, so the two have to drop the stored value in the same case.
    #[test]
    fn an_overridden_interval_is_not_reported_as_the_one_in_force() {
        let r = resolved(Some("0 5 * * * *"), Some("30m".to_owned()));
        assert_eq!(r.source, ScheduleSource::Env);
        assert_eq!(r.stored, None);

        let r = resolved(None, Some("30m".to_owned()));
        assert_eq!(r.source, ScheduleSource::Database);
        assert_eq!(r.stored.as_deref(), Some("30m"));

        // Unparseable: the schedule in force is the default, so there is no
        // stored value to show back either.
        let r = resolved(None, Some("banana".to_owned()));
        assert_eq!(r.source, ScheduleSource::Default);
        assert_eq!(r.stored, None);
    }

    #[test]
    fn display_names_the_schedule_in_the_units_it_was_written_in() {
        assert_eq!(
            Schedule::Cron("0 5 * * * *".into()).to_string(),
            "cron 0 5 * * * *"
        );
        assert_eq!(
            Schedule::Every(Duration::from_secs(600)).to_string(),
            "every 10m"
        );
        assert_eq!(
            Schedule::Every(Duration::from_secs(6 * 3600)).to_string(),
            "every 6h"
        );
        assert_eq!(
            Schedule::Every(Duration::from_secs(86_400)).to_string(),
            "every 1d"
        );
        assert_eq!(
            Schedule::Every(Duration::from_secs(604_800)).to_string(),
            "every 1w"
        );
        // 90 minutes is not a whole number of hours, so it stays minutes
        // rather than becoming a lossy "1.5h".
        assert_eq!(
            Schedule::Every(Duration::from_secs(90 * 60)).to_string(),
            "every 90m"
        );
    }

    // ---- the scheduler half -----------------------------------------------
    //
    // These build `Schedule::Every` directly rather than through
    // `parse_interval`, so the 5m floor does not force a slow test: the bounds
    // live in the parser, not in the enum.

    use crate::config::{Config, TokenSource};
    use crate::db::Db;
    use crate::state::AppState;
    use std::sync::Arc;

    fn test_state() -> Arc<AppState> {
        let base: url::Url = "http://127.0.0.1:1/".parse().unwrap();
        let cfg = Config {
            github_token: None,
            cron_schedule: None,
            db_path: std::path::PathBuf::from(":memory:"),
            host: "127.0.0.1".into(),
            port: 8080,
            log_level: "info".into(),
            github_api_base: base.clone(),
            github_page_base: base,
            timezone: chrono_tz::Tz::UTC,
        };
        Arc::new(AppState::new(
            Db::open_in_memory().unwrap(),
            cfg,
            None,
            None,
            TokenSource::Unset,
        ))
    }

    #[tokio::test]
    async fn current_reads_the_stored_interval() {
        let state = test_state();
        assert_eq!(current(&state).await.source, ScheduleSource::Default);

        state
            .db
            .call(|c| {
                crate::db::queries::set_setting(c, crate::db::queries::SYNC_INTERVAL_KEY, "6h")
            })
            .await
            .unwrap();

        let resolved = current(&state).await;
        assert_eq!(resolved.source, ScheduleSource::Database);
        assert_eq!(
            resolved.schedule,
            Schedule::Every(Duration::from_secs(6 * 3600))
        );
        // The raw string comes back too, because the field shows what was typed.
        assert_eq!(resolved.stored.as_deref(), Some("6h"));
    }

    /// A value the environment overrides is still in the database, but it is
    /// not the schedule in force — reporting it would put it in a field
    /// describing something that is not running.
    #[tokio::test]
    async fn an_overridden_interval_is_not_reported_as_in_force() {
        let mut state = test_state();
        Arc::get_mut(&mut state).unwrap().cfg.cron_schedule = Some("0 */10 * * * *".into());
        state
            .db
            .call(|c| {
                crate::db::queries::set_setting(c, crate::db::queries::SYNC_INTERVAL_KEY, "6h")
            })
            .await
            .unwrap();

        let resolved = current(&state).await;
        assert_eq!(resolved.source, ScheduleSource::Env);
        assert_eq!(resolved.stored, None);
    }

    #[tokio::test]
    async fn starting_registers_a_job_and_a_next_tick() {
        let state = test_state();
        start(Arc::clone(&state)).await.unwrap();

        let next = state
            .next_sync()
            .await
            .expect("a started job has a next tick");
        assert!(
            next > chrono::Utc::now(),
            "the next tick must be in the future"
        );
    }

    /// The whole point of holding the scheduler: a saved interval takes effect
    /// without a restart, and the job it replaces stops.
    #[tokio::test]
    async fn rescheduling_swaps_the_job_and_moves_the_next_tick() {
        let state = test_state();
        start(Arc::clone(&state)).await.unwrap();
        let first = state.scheduler.lock().await.as_ref().unwrap().job;

        assert!(
            reschedule(&state, &Schedule::Every(Duration::from_secs(3 * 3600)))
                .await
                .unwrap()
        );

        let slot = state.scheduler.lock().await;
        let slot = slot.as_ref().unwrap();
        assert_ne!(
            slot.job, first,
            "the job must be replaced, not left in place"
        );

        // Deliberately not asserting the old job's metadata is already gone:
        // `remove` is channel-driven, so that read is a race. What guarantees
        // one job is structural — the add and the remove happen under one
        // lock — and the manual verification watches the log for a doubled
        // cycle.
        let mut scheduler = slot.scheduler.clone();
        let next = scheduler
            .next_tick_for_job(slot.job)
            .await
            .unwrap()
            .unwrap();
        let due_in = next.signed_duration_since(chrono::Utc::now());
        assert!(
            due_in > chrono::Duration::minutes(170) && due_in < chrono::Duration::minutes(190),
            "the next tick should be about three hours out, was {due_in}"
        );
    }

    /// A scheduler that never started is a degraded service, not an error the
    /// settings page should report as a failure to save.
    #[tokio::test]
    async fn rescheduling_without_a_scheduler_reports_that_and_does_not_fail() {
        let state = test_state();
        assert!(
            !reschedule(&state, &Schedule::Every(Duration::from_secs(600)))
                .await
                .unwrap()
        );
        assert_eq!(state.next_sync().await, None);
    }
}
