//! Commands for cron launches and heartbeat prompts.

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{ArgGroup, Args, Subcommand};
use dgc_client::{Client, ClientError};
use dgc_protocol::ids::{AgentId, ScheduleId, WorkspaceId};
use dgc_protocol::methods::{
    AGENT_GET, AgentIdParams, SCHEDULE_CREATE, SCHEDULE_GET, SCHEDULE_LIST, SCHEDULE_PAUSE,
    SCHEDULE_REMOVE, SCHEDULE_RESUME, SCHEDULE_RUN, ScheduleCreateParams, ScheduleIdParams,
    ScheduleKind, ScheduleListParams, ScheduleListResult, ScheduleTargetParams,
};
use dgc_protocol::model::{Agent, Schedule, ScheduleRun, ScheduleTarget};
use dgc_protocol::rpc::NOT_FOUND;
use serde_json::Value;

use super::{CliResult, agent::Provider, connect};
use crate::output;

#[derive(Args)]
pub struct CreateOptions {
    /// Five-field cron expression or a supported macro.
    #[arg(long, value_name = "EXPR")]
    cron: String,
    /// Name displayed in the schedule list.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    /// End after this many successful launches or sends.
    #[arg(long, value_name = "N")]
    max_runs: Option<u32>,
    /// End after a duration such as 30m, 6h, or 2d.
    #[arg(long, value_name = "DUR", value_parser = parse_expiry)]
    expires_in: Option<i64>,
    /// Prompt to send on each run.
    prompt: String,
}

#[derive(Subcommand)]
pub enum ScheduleCommand {
    /// Register a schedule that starts new agents.
    #[command(group(ArgGroup::new("launch").required(true).args(["provider", "profile"])))]
    Create {
        /// Workspace where each new agent runs.
        #[arg(long, value_name = "WS")]
        workspace: WorkspaceId,
        /// Provider to launch.
        #[arg(long, value_enum, value_name = "P")]
        provider: Option<Provider>,
        /// Named launch profile from the daemon's config.
        #[arg(long, value_name = "N")]
        profile: Option<String>,
        /// Provider model name.
        #[arg(long, value_name = "M")]
        model: Option<String>,
        /// Provider permission mode.
        #[arg(long, value_name = "MODE")]
        mode: Option<String>,
        #[command(flatten)]
        options: CreateOptions,
    },
    /// List schedules that start new agents.
    Ls,
    /// Show a schedule's settings and recent runs.
    Show { id: ScheduleId },
    /// Pause automatic runs.
    Pause { id: ScheduleId },
    /// Resume automatic runs.
    Resume { id: ScheduleId },
    /// Trigger a run now, including while paused.
    Run { id: ScheduleId },
    /// Remove a schedule.
    Rm { id: ScheduleId },
}

#[derive(Subcommand)]
pub enum HeartbeatCommand {
    /// Register a schedule that sends a prompt to an idle agent.
    Create {
        /// Existing agent that receives the prompt.
        agent: AgentId,
        #[command(flatten)]
        options: CreateOptions,
    },
    /// List heartbeats.
    Ls,
    /// Show a heartbeat's settings and recent runs.
    Show { id: ScheduleId },
    /// Remove a heartbeat.
    Rm { id: ScheduleId },
}

pub async fn run(command: ScheduleCommand, json: bool) -> CliResult<ExitCode> {
    let mut client = connect().await?;
    match command {
        ScheduleCommand::Create {
            workspace,
            provider,
            profile,
            model,
            mode,
            options,
        } => {
            let target = ScheduleTargetParams::NewAgent {
                workspace_id: workspace,
                provider: provider.map(Into::into),
                profile,
                model,
                mode,
            };
            create(&mut client, target, options, json).await?;
        }
        ScheduleCommand::Ls => list(&mut client, ScheduleKind::NewAgent, json).await?,
        ScheduleCommand::Show { id } => show(&mut client, id, json).await?,
        ScheduleCommand::Pause { id } => set_state(&mut client, SCHEDULE_PAUSE, id, json).await?,
        ScheduleCommand::Resume { id } => set_state(&mut client, SCHEDULE_RESUME, id, json).await?,
        ScheduleCommand::Run { id } => {
            let result: ScheduleRun = client.call(SCHEDULE_RUN, &ScheduleIdParams { id }).await?;
            if json {
                println!(
                    "{}",
                    output::escape_json_line(&serde_json::to_string(&result)?)
                );
            } else {
                println!("{}", output::schedule_run_line(&result));
            }
        }
        ScheduleCommand::Rm { id } => remove(&mut client, id, json).await?,
    }
    Ok(ExitCode::SUCCESS)
}

pub async fn heartbeat(command: HeartbeatCommand, json: bool) -> CliResult<ExitCode> {
    let mut client = connect().await?;
    match command {
        HeartbeatCommand::Create { agent, options } => {
            let target = ScheduleTargetParams::Agent { agent_id: agent };
            create(&mut client, target, options, json).await?;
        }
        HeartbeatCommand::Ls => list(&mut client, ScheduleKind::Agent, json).await?,
        HeartbeatCommand::Show { id } => show(&mut client, id, json).await?,
        HeartbeatCommand::Rm { id } => remove(&mut client, id, json).await?,
    }
    Ok(ExitCode::SUCCESS)
}

fn parse_expiry(value: &str) -> Result<i64, &'static str> {
    const INVALID: &str = "expected a positive integer followed by m, h, or d";
    const OVERFLOW: &str = "expiry is too far in the future";
    let (digits, seconds_per_unit) = if let Some(digits) = value.strip_suffix('m') {
        (digits, 60)
    } else if let Some(digits) = value.strip_suffix('h') {
        (digits, 3_600)
    } else if let Some(digits) = value.strip_suffix('d') {
        (digits, 86_400)
    } else {
        return Err(INVALID);
    };
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(INVALID);
    }
    let count = digits.parse::<i64>().map_err(|_| INVALID)?;
    if count == 0 {
        return Err(INVALID);
    }
    let seconds = count.checked_mul(seconds_per_unit).ok_or(OVERFLOW)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch")?;
    let now = i64::try_from(now.as_secs()).map_err(|_| OVERFLOW)?;
    now.checked_add(seconds).ok_or(OVERFLOW)
}

async fn create(
    client: &mut Client,
    target: ScheduleTargetParams,
    options: CreateOptions,
    json: bool,
) -> CliResult<()> {
    let heartbeat_mode = match &target {
        ScheduleTargetParams::Agent { agent_id } => {
            agent_auto_approve_mode(client, agent_id).await?
        }
        ScheduleTargetParams::NewAgent { .. } => None,
    };
    let params = ScheduleCreateParams {
        name: options.name,
        target,
        prompt: options.prompt,
        cron: options.cron,
        max_runs: options.max_runs,
        expires_at: options.expires_in,
    };
    let schedule: Schedule = client.call(SCHEDULE_CREATE, &params).await?;
    let auto_approve_mode = match &schedule.target {
        ScheduleTarget::NewAgent {
            auto_approve: true,
            mode,
            ..
        } => Some(mode.as_deref().unwrap_or("-")),
        _ => heartbeat_mode.as_deref(),
    };
    if let Some(mode) = auto_approve_mode {
        eprintln!(
            "warning: schedule {} runs in auto-approve mode {}: the provider acts \
             without asking for permission",
            schedule.id,
            output::escape_line(mode)
        );
    }
    if json {
        println!(
            "{}",
            output::escape_json_line(&serde_json::to_string(&schedule)?)
        );
    } else {
        println!("{}", schedule.id);
    }
    Ok(())
}

async fn list(client: &mut Client, kind: ScheduleKind, json: bool) -> CliResult<()> {
    let params = ScheduleListParams { kind: Some(kind) };
    let result: ScheduleListResult = client.call(SCHEDULE_LIST, &params).await?;
    if json {
        println!(
            "{}",
            output::escape_json_line(&serde_json::to_string(&result)?)
        );
    } else {
        for entry in &result.schedules {
            let auto_approve = target_auto_approve(client, &entry.target).await?;
            println!("{}", output::schedule_line(entry, auto_approve));
        }
    }
    Ok(())
}

async fn show(client: &mut Client, id: ScheduleId, json: bool) -> CliResult<()> {
    let schedule: Schedule = client.call(SCHEDULE_GET, &ScheduleIdParams { id }).await?;
    if json {
        println!(
            "{}",
            output::escape_json_line(&serde_json::to_string(&schedule)?)
        );
    } else {
        let auto_approve = target_auto_approve(client, &schedule.target).await?;
        println!("{}", output::schedule_details(&schedule, auto_approve));
    }
    Ok(())
}

async fn target_auto_approve(client: &mut Client, target: &ScheduleTarget) -> CliResult<bool> {
    match target {
        ScheduleTarget::NewAgent { auto_approve, .. } => Ok(*auto_approve),
        ScheduleTarget::Agent { agent_id } => {
            Ok(agent_auto_approve_mode(client, agent_id).await?.is_some())
        }
    }
}

async fn agent_auto_approve_mode(client: &mut Client, id: &AgentId) -> CliResult<Option<String>> {
    let params = AgentIdParams { id: id.clone() };
    match client.call::<_, Agent>(AGENT_GET, &params).await {
        Ok(agent) => Ok(agent
            .auto_approve
            .then(|| agent.mode.unwrap_or_else(|| "-".into()))),
        Err(ClientError::Rpc(err)) if err.code == NOT_FOUND => Ok(None),
        Err(err) => Err(err.into()),
    }
}

async fn set_state(client: &mut Client, method: &str, id: ScheduleId, json: bool) -> CliResult<()> {
    let schedule: Schedule = client.call(method, &ScheduleIdParams { id }).await?;
    if json {
        println!(
            "{}",
            output::escape_json_line(&serde_json::to_string(&schedule)?)
        );
    } else {
        println!(
            "{} {}",
            output::schedule_state_name(schedule.state),
            schedule.id
        );
    }
    Ok(())
}

async fn remove(client: &mut Client, id: ScheduleId, json: bool) -> CliResult<()> {
    let params = ScheduleIdParams { id };
    let result: Value = client.call(SCHEDULE_REMOVE, &params).await?;
    if json {
        println!("{result}");
    } else {
        println!("removed {}", params.id);
    }
    Ok(())
}
