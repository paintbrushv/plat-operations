use std::{env, path::PathBuf};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub bind_addr: String,
    pub report_dir: PathBuf,
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self {
            database_url: env::var("DATABASE_URL").unwrap_or_else(|_| {
                format!(
                    "sqlite://{}?mode=rwc",
                    crate_root().join("boxscore.db").display()
                )
            }),
            bind_addr: env_or_legacy("BOXSCORE_BIND_ADDR", "NOI_ATLAS_BIND_ADDR")
                .unwrap_or_else(|| "127.0.0.1:3818".to_string()),
            report_dir: env_or_legacy("BOXSCORE_REPORT_DIR", "NOI_ATLAS_REPORT_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| crate_root().join("reports/generated")),
        }
    }
}

/// Anchor default paths to the crate checkout instead of the process working
/// directory. Running `boxscore` from the repo root used to silently create a
/// second, empty database there.
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Prefer the Boxscore env var but honor the pre-rename NOI Atlas name so
/// existing shells, scripts, and launchd jobs keep working.
fn env_or_legacy(primary: &str, legacy: &str) -> Option<String> {
    env::var(primary).or_else(|_| env::var(legacy)).ok()
}
