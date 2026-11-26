//! `dgc daemon {run|start|stop|status}`.

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Subcommand;
use dgc_client::Client;
use dgc_daemon::config::DaemonConfig;
use dgc_daemon::lock::{InstanceLock, LockError};
use dgc_daemon::paths::DaemonPaths;
use dgc_daemon::run_daemon;
use dgc_protocol::methods::{DAEMON_SHUTDOWN, DAEMON_STATUS, DaemonStatus};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tracing_subscriber::filter::LevelFilter;

use super::{CliResult, connect_or_spawn, resolve_paths};
use crate::output;

/// How long `stop` waits for the daemon to release the socket and the lock.
const STOP_WAIT: Duration = Duration::from_secs(5);
const STOP_POLL: Duration = Duration::from_millis(25);

#[derive(Subcommand)]
pub enum DaemonCommand {
    /// Run the daemon in the foreground, or start it in the background.
    Run {
        /// Start the daemon in a new session, logging to <state>/daemon.log,
        /// and return once it answers.
        #[arg(long)]
        detach: bool,
    },
    /// Start the daemon in the background unless it is already running.
    Start,
    /// Ask the daemon to shut down.
    Stop,
    /// Show daemon status, starting the daemon if needed.
    Status,
}

pub async fn run(command: DaemonCommand, json: bool) -> CliResult<ExitCode> {
    let paths = resolve_paths()?;
    match command {
        DaemonCommand::Run { detach: false } => run_foreground(&paths).await,
        DaemonCommand::Run { detach: true } => {
            connect_or_spawn(&paths).await?;
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::Start => start(&paths).await,
        DaemonCommand::Stop => stop(&paths).await,
        DaemonCommand::Status => status(&paths, json).await,
    }
}

fn log_level() -> LevelFilter {
    match std::env::var("DGC_LOG").as_deref() {
        Ok("error") => LevelFilter::ERROR,
        Ok("warn") => LevelFilter::WARN,
        Ok("debug") => LevelFilter::DEBUG,
        Ok("trace") => LevelFilter::TRACE,
        _ => LevelFilter::INFO,
    }
}

async fn run_foreground(paths: &DaemonPaths) -> CliResult<ExitCode> {
    tracing_subscriber::fmt()
        .with_max_level(log_level())
        .with_writer(std::io::stderr)
        .init();

    let config = DaemonConfig::load(&paths.config_file)?;
    let (shutdown, _keepalive) = watch::channel(false);

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    let trigger = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
        trigger.send_replace(true);
    });

    // run_daemon logs through tracing; a failure is also printed by `main`.
    run_daemon(paths.clone(), config, shutdown).await?;
    Ok(ExitCode::SUCCESS)
}

async fn start(paths: &DaemonPaths) -> CliResult<ExitCode> {
    match Client::connect(paths).await {
        Ok(client) => {
            println!("already running (pid {})", client.daemon_pid());
            return Ok(ExitCode::SUCCESS);
        }
        Err(err) if err.is_not_running() => {}
        Err(err) => return Err(err.into()),
    }
    let client = connect_or_spawn(paths).await?;
    println!("started (pid {})", client.daemon_pid());
    Ok(ExitCode::SUCCESS)
}

async fn status(paths: &DaemonPaths, json: bool) -> CliResult<ExitCode> {
    let mut client = connect_or_spawn(paths).await?;
    let status: DaemonStatus = client.call(DAEMON_STATUS, &serde_json::json!({})).await?;
    if json {
        println!("{}", output::daemon_status_json(&status)?);
    } else {
        println!("{}", output::daemon_status_text(&status));
    }
    Ok(ExitCode::SUCCESS)
}

async fn stop(paths: &DaemonPaths) -> CliResult<ExitCode> {
    let mut client = match Client::connect(paths).await {
        Ok(client) => client,
        Err(err) if err.is_not_running() => {
            println!("daemon is not running");
            return Ok(ExitCode::SUCCESS);
        }
        Err(err) => return Err(err.into()),
    };
    let _: serde_json::Value = client.call(DAEMON_SHUTDOWN, &serde_json::json!({})).await?;
    drop(client);

    // The daemon removes the socket first and releases the instance lock last.
    // Waiting for both means a following autostart cannot lose the lock race
    // to the daemon that is still exiting.
    let deadline = Instant::now() + STOP_WAIT;
    while !(socket_gone(&paths.socket) && lock_free(&paths.state_dir)) {
        if Instant::now() >= deadline {
            return Err("daemon did not stop within 5 s".into());
        }
        tokio::time::sleep(STOP_POLL).await;
    }
    println!("daemon stopped");
    Ok(ExitCode::SUCCESS)
}

fn socket_gone(socket: &Path) -> bool {
    std::fs::symlink_metadata(socket).is_err()
}

/// True when no daemon holds the instance lock. Taking the lock is only a
/// probe; it is released again immediately.
fn lock_free(state_dir: &Path) -> bool {
    match InstanceLock::acquire(state_dir) {
        Ok(_probe) => true,
        Err(LockError::AlreadyRunning) => false,
        // No state directory or unreadable lock file: nothing is holding it.
        Err(LockError::Io(_)) => true,
    }
}
