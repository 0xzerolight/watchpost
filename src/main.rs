use axum::Router;
use axum::routing::get;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use watchpost::config::Config;
use watchpost::db::Db;

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

    // Opens (or creates + migrates) the sqlite db. Not queried yet — wired in
    // by later tasks via Db::call; held here so it stays open for the
    // process lifetime and startup fails fast on a bad db path.
    let _db = match Db::open(&config.db_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("db error: {e}");
            std::process::exit(1);
        }
    };

    let app = Router::new().route("/health", get(|| async { "OK" }));

    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));
    tracing::info!(%addr, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install ctrl_c handler");
    tracing::info!("shutdown signal received");
}
