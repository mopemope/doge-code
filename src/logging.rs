use anyhow::Result;
use tracing::info;

pub fn init_logging(level: &str) -> Result<()> {
    let _ = level; // TODO: wire level if needed; keep behavior identical for now
    let log_file = std::sync::Arc::new(std::fs::File::create("./debug.log")?);
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .init();
    info!("logging initialized");
    Ok(())
}
