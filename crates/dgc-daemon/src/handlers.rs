//! JSON-RPC handlers for project and workspace methods: parameter parsing and
//! error mapping around [`WorkspaceManager`]. Results are always JSON objects.

use dgc_protocol::methods::{
    ProjectAddParams, ProjectRemoveParams, WorkspaceArchiveParams, WorkspaceCreateParams,
    WorkspaceListParams,
};
use dgc_protocol::rpc::{INTERNAL, INVALID_PARAMS, RpcError};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::workspace::WorkspaceManager;

/// Longest piece of a deserialization error echoed to the client, in chars.
const PARSE_ERROR_ECHO: usize = 200;

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

pub async fn workspace_archive(
    manager: &WorkspaceManager,
    params: Value,
) -> Result<Value, RpcError> {
    let params: WorkspaceArchiveParams = parse(params)?;
    encode(&manager.archive_workspace(&params.id).await?)
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
            workspace_archive(&m, json!([])).await,
        ] {
            assert_eq!(result.expect_err("must fail").code, INVALID_PARAMS);
        }
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
