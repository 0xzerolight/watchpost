use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::get;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use watchpost::collector::try_run_cycle;
use watchpost::config::{Config, DEFAULT_CRON};
use watchpost::db::Db;
use watchpost::gh_client::GhClient;
use watchpost::ratelimit::RateGate;
use watchpost::state::{AppState, SyncStatus};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let log_level = std::env::var("WATCHPOST_LOG").unwrap_or_else(|_| "info".to_string());
    let env_filter = tracing_subscriber::EnvFilter::new(log_level);
    Registry::default()
        .with(env_filter)
        .with(tracing_logfmt::layer())
        .init();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(config = %config.redacted_summary(), "starting watchpost");

    let db = match Db::open(&config.db_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("db error: {e}");
            std::process::exit(1);
        }
    };

    let gh = match GhClient::new(&config.github_token, config.github_api_base.clone()) {
        Ok(gh) => gh,
        Err(e) => {
            eprintln!("github client error: {e}");
            std::process::exit(1);
        }
    };

    let state = Arc::new(AppState {
        db,
        gh,
        cfg: config,
        gate: RateGate::new(),
        sync: Mutex::new(SyncStatus::Idle),
        sync_guard: Arc::new(tokio::sync::Mutex::new(())),
    });

    // Collect once at boot rather than waiting up to an hour for the first
    // tick. Spawned, so a slow or failing cycle never delays serving — and
    // `run_cycle` records its own failures, so there is nothing to handle here.
    {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            try_run_cycle(state).await;
        });
    }

    // A scheduler that won't start is a degraded service, not a dead one: the
    // dashboard still serves what is already collected and the manual trigger
    // still works, so log it and carry on rather than retrying or exiting.
    let scheduler = match start_scheduler(Arc::clone(&state)).await {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::error!(error = %e, "scheduler failed to start; serving without cron");
            None
        }
    };

    let app = Router::new().route("/health", get(|| async { "OK" }));

    let addr = format!("{}:{}", state.cfg.host, state.cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));
    tracing::info!(%addr, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(scheduler))
        .await
        .expect("server error");
}

/// Build the cron scheduler and register the collection job.
async fn start_scheduler(
    state: Arc<AppState>,
) -> Result<JobScheduler, tokio_cron_scheduler::JobSchedulerError> {
    let schedule = resolve_schedule(&state.cfg.cron_schedule);
    let scheduler = JobScheduler::new().await?;
    let job = Job::new_async(&schedule, move |_id, _sched| {
        let state = Arc::clone(&state);
        Box::pin(async move {
            try_run_cycle(state).await;
        })
    })?;
    scheduler.add(job).await?;
    scheduler.start().await?;
    tracing::info!(%schedule, "cron scheduled");
    Ok(scheduler)
}

/// A bad `WATCHPOST_CRON` must not leave the service collecting nothing at
/// all, so an unparseable expression is logged and replaced by the default.
fn resolve_schedule(input: &str) -> String {
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

/// Resolves on SIGINT or SIGTERM (the latter is how a container is stopped),
/// then stops the scheduler so no tick starts while the server is draining.
async fn shutdown_signal(scheduler: Option<JobScheduler>) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install ctrl_c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutdown signal received");

    if let Some(mut scheduler) = scheduler
        && let Err(e) = scheduler.shutdown().await
    {
        tracing::warn!(error = %e, "scheduler shutdown failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_schedule_passes_through() {
        assert_eq!(resolve_schedule("0 */5 * * * *"), "0 */5 * * * *");
    }

    #[test]
    fn invalid_schedule_falls_back_to_default() {
        assert_eq!(resolve_schedule("garbage"), DEFAULT_CRON);
        assert_eq!(resolve_schedule(""), DEFAULT_CRON);
        // Five fields: seconds are required, so this is not a valid schedule.
        assert_eq!(resolve_schedule("5 * * * *"), DEFAULT_CRON);
    }
}
