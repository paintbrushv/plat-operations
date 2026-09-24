use anyhow::Result;
use boxscore::{cli, config::AppConfig};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        // Logs go to stderr so stdout stays clean JSON for automation.
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    let config = AppConfig::from_env();
    cli::run(config).await
}
