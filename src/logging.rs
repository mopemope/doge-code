use anyhow::Result;
use tracing::info;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

pub fn init_logging() -> Result<()> {
    let log_file = std::sync::Arc::new(std::fs::File::create("./debug.log")?);
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .finish();

    tracing::subscriber::set_global_default(subscriber)?;
    info!("logging initialized");
    Ok(())
}
