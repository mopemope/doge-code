use anyhow::Result;
use tracing::{error, info};
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

    // Set up panic hook to log panics to the same tracing subscriber
    std::panic::set_hook(Box::new(|panic_info| {
        let panic_msg = format!("PANIC: {}", panic_info);
        error!("{}", panic_msg);

        // Also print to stderr to ensure it's always visible
        eprintln!("{}", panic_msg);
    }));

    info!("logging initialized");
    Ok(())
}
