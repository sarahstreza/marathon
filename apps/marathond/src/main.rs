//! `marathond`: the Marathon server daemon.

use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "marathond starting");
    anyhow::bail!("marathond does not serve anything yet (PLAN.md milestone M2)")
}
