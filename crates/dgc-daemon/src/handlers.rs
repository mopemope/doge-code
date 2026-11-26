//! JSON-RPC handlers: parameter parsing and error mapping around
//! [`WorkspaceManager`] and [`AgentManager`]. Results are always JSON objects.

use std::sync::Arc;

use dgc_protocol::methods::{
    AgentCreateParams, AgentEventsParams, AgentIdParams, AgentListParams, AgentSendParams,
    PermissionListParams, PermissionRespondParams, ProjectAddParams, ProjectRemoveParams,
    SubscribeParams, UnsubscribeParams, WorkspaceArchiveParams, WorkspaceCreateParams,
    WorkspaceListParams,
};
use dgc_protocol::model::AgentState;
use dgc_protocol::rpc::{INTERNAL, INVALID_PARAMS, RpcError};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::agent::{AgentError, AgentManager};
use crate::server::{ConnCtx, ECHO_LIMIT};
use crate::workspace::WorkspaceManager;

/// Longest piece of a deserialization error echoed to the client, in chars
/// (the same limit as other echoed input).
const PARSE_ERROR_ECHO: usize = ECHO_LIMIT;

/// Decode request params. A missing (`null`) params value counts as `{}`.
fn parse<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params).map_err(|err| {
        let detail: String = err.to_string().chars().take(PARSE_ERROR_ECHO).collect();
        RpcError::new(INVALID_PARAMS, format!("invalid params: {detail}"))
    })
}

fn encode<T: Serialize>(value: &T) -> Result<Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|err| RpcError::new(INTERNAL, format!("cannot encode result: {err}")))
}

pub async fn project_add(manager: &WorkspaceManager, params: Value) -> Result<Value, RpcError> {
    let params: ProjectAddParams = parse(params)?;
    encode(&manager.add_project(&params.path).await?)
}

pub async fn project_list(manager: &WorkspaceManager) -> Result<Value, RpcError> {
    Ok(json!({ "projects": manager.list_projects().await? }))
}

pub async fn project_remove(manager: &WorkspaceManager, params: Value) -> Result<Value, RpcError> {
    let params: ProjectRemoveParams = parse(params)?;
    manager.remove_project(&params.id).await?;
    Ok(json!({}))
}

pub async fn workspace_create(
    manager: &WorkspaceManager,
    params: Value,
) -> Result<Value, RpcError> {
    let params: WorkspaceCreateParams = parse(params)?;
    encode(&manager.create_workspace(params).await?)
}

pub async fn workspace_list(manager: &WorkspaceManager, params: Value) -> Result<Value, RpcError> {
    let params: WorkspaceListParams = parse(params)?;
    Ok(json!({ "workspaces": manager.list_workspaces(params.project_id.as_ref()).await? }))
}

impl From<AgentError> for RpcError {
    fn from(err: AgentError) -> Self {
        RpcError::new(err.code(), err.to_string())
    }
}

/// Archive the workspace's agents that are not archived yet, then the
/// workspace itself.
pub async fn workspace_archive(
    manager: &WorkspaceManager,
    agents: &AgentManager,
    params: Value,
) -> Result<Value, RpcError> {
    let params: WorkspaceArchiveParams = parse(params)?;
    manager.check_archive(&params.id, params.force).await?;
    for agent in agents.list()? {
        if agent.workspace_id != params.id || agent.state == AgentState::Archived {
            continue;
        }
        match agents.archive(&agent.id).await {
            // Archived concurrently.
            Ok(_) | Err(AgentError::InvalidState { .. }) => {}
            Err(err) => return Err(err.into()),
        }
    }
    encode(&manager.archive_workspace(&params.id, params.force).await?)
}

pub async fn agent_create(agents: &AgentManager, params: Value) -> Result<Value, RpcError> {
    let params: AgentCreateParams = parse(params)?;
    encode(&agents.create(params).await?)
}

pub fn agent_list(agents: &AgentManager, params: Value) -> Result<Value, RpcError> {
    let params: AgentListParams = parse(params)?;
    let listed: Vec<_> = agents
        .list()?
        .into_iter()
        .filter(|agent| params.include_archived || agent.state != AgentState::Archived)
        .collect();
    Ok(json!({ "agents": listed }))
}

pub fn agent_get(agents: &AgentManager, params: Value) -> Result<Value, RpcError> {
    let params: AgentIdParams = parse(params)?;
    encode(&agents.get(&params.id)?)
}

pub async fn agent_send(agents: &AgentManager, params: Value) -> Result<Value, RpcError> {
    let params: AgentSendParams = parse(params)?;
    Ok(json!({ "outcome": agents.send(params).await? }))
}

pub async fn agent_interrupt(agents: &AgentManager, params: Value) -> Result<Value, RpcError> {
    let params: AgentIdParams = parse(params)?;
    agents.interrupt(&params.id).await?;
    Ok(json!({}))
}

pub async fn agent_archive(agents: &AgentManager, params: Value) -> Result<Value, RpcError> {
    let params: AgentIdParams = parse(params)?;
    encode(&agents.archive(&params.id).await?)
}

pub async fn agent_resume(agents: &AgentManager, params: Value) -> Result<Value, RpcError> {
    let params: AgentIdParams = parse(params)?;
    encode(&agents.resume(&params.id).await?)
}

/// One page of stored events. `tail` replaces forward paging when supplied.
pub fn agent_events(agents: &AgentManager, params: Value) -> Result<Value, RpcError> {
    let params: AgentEventsParams = parse(params)?;
    Ok(json!({ "events": agents.events_page(&params)? }))
}

pub fn permission_list(agents: &AgentManager, params: Value) -> Result<Value, RpcError> {
    let params: PermissionListParams = parse(params)?;
    Ok(json!({ "permissions": agents.pending_permissions(params.agent_id) }))
}

pub async fn permission_respond(agents: &AgentManager, params: Value) -> Result<Value, RpcError> {
    let params: PermissionRespondParams = parse(params)?;
    agents.respond_permission(params).await?;
    Ok(json!({}))
}

/// Start (or replace) a subscription of this connection. `since_seq`
/// requires `agent_id`; the agent must exist.
pub fn subscribe(
    agents: &Arc<AgentManager>,
    conn: &ConnCtx,
    params: Value,
) -> Result<Value, RpcError> {
    let params: SubscribeParams = parse(params)?;
    if params.since_seq.is_some() && params.agent_id.is_none() {
        return Err(RpcError::new(INVALID_PARAMS, "since_seq requires agent_id"));
    }
    if let Some(id) = &params.agent_id {
        agents.get(id)?;
    }
    conn.subscriptions().start(
        Arc::clone(agents),
        conn.clone(),
        params.agent_id,
        params.since_seq,
    );
    Ok(json!({}))
}

pub fn unsubscribe(conn: &ConnCtx, params: Value) -> Result<Value, RpcError> {
    let params: UnsubscribeParams = parse(params)?;
    conn.subscriptions().stop(params.agent_id.as_ref());
    Ok(json!({}))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dgc_protocol::rpc::NOT_FOUND;
    use tempfile::tempdir;

    use super::*;
    use crate::store::Store;

    fn manager(root: &std::path::Path) -> WorkspaceManager {
        let store = Arc::new(Store::open(&root.join("state")).expect("store"));
        WorkspaceManager::new(store, root.join("worktrees"))
    }

    #[tokio::test]
    async fn malformed_params_are_invalid_params() {
        let tmp = tempdir().expect("tempdir");
        let m = manager(tmp.path());
        for result in [
            project_add(&m, json!({})).await,
            project_add(&m, json!({"path": 5})).await,
            project_add(&m, Value::Null).await,
            project_remove(&m, json!({"id": "not-an-id"})).await,
            workspace_create(&m, json!({"project_id": "x"})).await,
            workspace_list(&m, json!({"project_id": 3})).await,
            workspace_archive(&m, &agents(tmp.path()), json!([])).await,
        ] {
            assert_eq!(result.expect_err("must fail").code, INVALID_PARAMS);
        }
    }

    fn agents(root: &std::path::Path) -> AgentManager {
        let paths = crate::paths::DaemonPaths::under(root);
        let store = Arc::new(Store::open(&paths.state_dir).expect("store"));
        AgentManager::new(
            store,
            paths,
            crate::config::DaemonConfig::default(),
            std::collections::HashMap::new(),
        )
    }

    #[tokio::test]
    async fn agent_handlers_reject_malformed_params() {
        let tmp = tempdir().expect("tempdir");
        let a = agents(tmp.path());
        for result in [
            agent_create(&a, json!({"title": "t"})).await,
            agent_list(&a, json!({"include_archived": "yes"})),
            agent_get(&a, json!({"id": "agt_nope"})),
            agent_send(&a, json!({"agent_id": 1, "text": "x"})).await,
            agent_interrupt(&a, Value::Null).await,
            agent_archive(&a, json!([])).await,
            agent_resume(&a, json!({})).await,
            agent_events(&a, json!({"since_seq": 1})),
            permission_list(&a, json!({"agent_id": false})),
            permission_respond(&a, json!({"id": "perm_x", "decision": "maybe"})).await,
        ] {
            assert_eq!(result.expect_err("must fail").code, INVALID_PARAMS);
        }
    }

    #[tokio::test]
    async fn agent_lists_are_objects_and_errors_keep_their_codes() {
        let tmp = tempdir().expect("tempdir");
        let a = agents(tmp.path());
        assert_eq!(
            agent_list(&a, Value::Null).expect("ok"),
            json!({"agents": []})
        );
        assert_eq!(
            permission_list(&a, Value::Null).expect("ok"),
            json!({"permissions": []})
        );
        let id = dgc_protocol::ids::AgentId::generate();
        let err = agent_get(&a, json!({"id": id})).expect_err("unknown");
        assert_eq!(err.code, NOT_FOUND);
        let err = agent_events(&a, json!({"agent_id": id})).expect_err("unknown");
        assert_eq!(err.code, NOT_FOUND);
    }

    #[tokio::test]
    async fn echoed_parse_errors_are_cut_to_the_echo_limit() {
        let tmp = tempdir().expect("tempdir");
        let a = agents(tmp.path());
        let long = "x".repeat(500);
        let err = agent_get(&a, json!({"id": long})).expect_err("invalid id");
        let detail = err
            .message
            .strip_prefix("invalid params: ")
            .expect("prefix");
        assert!(detail.chars().count() <= ECHO_LIMIT, "{}", err.message);
    }

    #[tokio::test]
    async fn list_results_are_objects_and_missing_params_mean_all() {
        let tmp = tempdir().expect("tempdir");
        let m = manager(tmp.path());
        assert_eq!(project_list(&m).await.expect("ok"), json!({"projects": []}));
        assert_eq!(
            workspace_list(&m, Value::Null).await.expect("ok"),
            json!({"workspaces": []})
        );
    }

    #[tokio::test]
    async fn manager_errors_keep_their_codes() {
        let tmp = tempdir().expect("tempdir");
        let m = manager(tmp.path());
        let unknown = dgc_protocol::ids::ProjectId::generate();
        let err = project_remove(&m, json!({"id": unknown}))
            .await
            .expect_err("unknown project");
        assert_eq!(err.code, NOT_FOUND);
    }

    #[tokio::test]
    async fn project_add_returns_the_project_and_remove_returns_an_object() {
        let tmp = tempdir().expect("tempdir");
        let m = manager(tmp.path());
        let dir = std::fs::canonicalize(tmp.path()).expect("canon").join("p");
        std::fs::create_dir(&dir).expect("mkdir");
        let added = project_add(&m, json!({"path": dir})).await.expect("add");
        assert_eq!(added["name"], "p");
        let removed = project_remove(&m, json!({"id": added["id"]}))
            .await
            .expect("remove");
        assert_eq!(removed, json!({}));
    }
}
