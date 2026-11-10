//! `marathon-desktop`: the Marathon desktop client.

use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "marathon-desktop starting"
    );
    anyhow::bail!("marathon-desktop does not play anything yet (PLAN.md milestone M3)")
}
