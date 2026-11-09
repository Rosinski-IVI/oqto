//! GitLab Community integration. All upstream traffic is translated into a
//! closed runner operation; the browser and agent never receive the PAT.
use anyhow::Context;
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, header},
    routing::{get, post},
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest;
use sqlx::SqlitePool;
use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{Arc, OnceLock},
};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    api::{ApiError, ApiResult, AppState},
    auth::CurrentUser,
    gitlab::{
        EVENT_CATEGORIES, GitLabConnection, GitLabRepository, InboxEvent, ProjectMapping, Proposal,
        TOKEN_CREDENTIAL_ID, normalize_instance_url, project_from_remote, validate_project_path,
    },
    runner::router::{
        ExecutionTarget, resolve_runner_for_target, resolve_target_for_workspace_path,
    },
};
use oqto_protocol::gitlab::{
    GitLabAgentOperation, GitLabAgentRpcRequest, GitLabDiscussionPosition, GitLabHttpDispatch,
    GitLabIssueScope, GitLabIssueState, GitLabIssueStateEvent, GitLabMergeRequestScope,
    GitLabMergeRequestState, GitLabOperation,
};

const GITLAB_PAGE_SIZE: usize = 50;
const MAX_POLL_PAGES: u32 = 20;
const MAX_MENTION_ISSUE_LOOKUPS: usize = 20;
const MAX_TRACKED_MR_DISCUSSIONS_PER_POLL: usize = 20;

static POLL_LOCKS: OnceLock<DashMap<String, Arc<Mutex<()>>>> = OnceLock::new();
static ISSUE_WRITE_LOCKS: OnceLock<DashMap<String, Arc<Mutex<()>>>> = OnceLock::new();
static DRAFT_LOCKS: OnceLock<DashMap<String, Arc<Mutex<()>>>> = OnceLock::new();

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/gitlab/config",
            get(get_config).put(put_config).delete(delete_config),
        )
        .route("/gitlab/test", post(test_connection))
        .route(
            "/gitlab/mapping",
            get(get_mapping).put(put_mapping).delete(delete_mapping),
        )
        .route("/gitlab/inbox", get(get_inbox))
        .route("/gitlab/inbox/poll", post(poll_inbox_now))
        .route("/gitlab/issues", get(list_gitlab_issues))
        .route("/gitlab/session-selection", post(select_session_issue))
        .route("/gitlab/inbox/counts", get(get_unread_counts))
        .route("/gitlab/inbox/{event_id}/read", post(mark_event_read))
        .route(
            "/gitlab/proposals",
            get(list_proposals).post(create_proposal),
        )
        .route(
            "/gitlab/proposals/{proposal_id}/approve",
            post(approve_proposal),
        )
        .route(
            "/gitlab/proposals/{proposal_id}/reject",
            post(reject_proposal),
        )
        .route(
            "/gitlab/proposals/{proposal_id}/rebind",
            post(rebind_proposal),
        )
        .route("/gitlab/merge-requests/draft", post(create_or_update_draft))
        .route(
            "/gitlab/merge-requests/ready",
            post(mark_merge_request_ready),
        )
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
}

fn repository(state: &AppState) -> ApiResult<GitLabRepository> {
    let apps = state
        .apps
        .as_ref()
        .ok_or_else(|| ApiError::service_unavailable("GitLab service unavailable"))?;
    let pool: &SqlitePool = apps.repository().pool();
    Ok(GitLabRepository::new(pool.clone()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectSessionIssueRequest {
    workspace_path: String,
    session_id: String,
    issue_iid: u64,
}

async fn select_session_issue(
    State(state): State<AppState>,
    user: CurrentUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<SelectSessionIssueRequest>,
) -> ApiResult<Json<Value>> {
    protected_submission(&state, peer, &headers)?;
    if request.issue_iid == 0 {
        return Err(ApiError::bad_request("GitLab issue IID must be positive"));
    }
    validate_session_id(Some(&request.session_id))?;
    let session = state
        .sessions
        .get_session_for_user(user.id(), &request.session_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("Oqto session not found"))?;
    let session_workspace = canonical_workspace(&state, user.id(), &session.workspace_path).await?;
    let request_workspace = canonical_workspace(&state, user.id(), &request.workspace_path).await?;
    if session_workspace != request_workspace {
        return Err(ApiError::forbidden(
            "GitLab issue selection must belong to this session's workspace",
        ));
    }
    let context = workspace_gitlab(&state, user.id(), &request_workspace).await?;
    let issue = call_gitlab(
        &context,
        user.id(),
        GitLabOperation::GetIssue {
            project_path: context.mapping.project_path.clone(),
            issue_iid: request.issue_iid,
        },
    )
    .await?;
    if issue.get("iid").and_then(Value::as_u64) != Some(request.issue_iid) {
        return Err(ApiError::not_found(
            "GitLab issue not found in the mapped project",
        ));
    }
    repository(&state)?
        .select_issue_for_session(
            user.id(),
            &request.session_id,
            &context.canonical_path,
            &context.mapping.project_path,
            request.issue_iid,
        )
        .await
        .map_err(internal)?;
    Ok(Json(
        json!({"selected": true, "issue_iid": request.issue_iid}),
    ))
}

fn require_real_auth(state: &AppState) -> ApiResult<()> {
    super::credentials::require_real_auth(state)
}

fn protected_submission(state: &AppState, peer: SocketAddr, headers: &HeaderMap) -> ApiResult<()> {
    require_real_auth(state)?;
    super::credentials::validate_submission_channel(
        Some(peer),
        headers,
        state.auth.allowed_origins(),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveConfigRequest {
    instance_url: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "default_categories")]
    event_categories: Vec<String>,
}

fn default_true() -> bool {
    true
}
fn default_categories() -> Vec<String> {
    EVENT_CATEGORIES
        .iter()
        .filter(|value| **value != "project_activity")
        .map(|value| (*value).to_owned())
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceQuery {
    workspace_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveMappingRequest {
    workspace_path: String,
    #[serde(default)]
    remote_url: Option<String>,
    #[serde(default)]
    project_path_override: Option<String>,
}

#[derive(Debug, Serialize)]
struct ConnectionView {
    host: String,
    path_prefix: String,
    credential_id: String,
    revision: u32,
    enabled: bool,
    event_categories: Vec<String>,
    credential_ready: bool,
    connected: bool,
    gitlab_username: Option<String>,
}

async fn get_config(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<Json<Option<ConnectionView>>> {
    require_real_auth(&state)?;
    let repository = repository(&state)?;
    let Some(connection) = repository.connection(user.id()).await.map_err(internal)? else {
        return Ok(Json(None));
    };
    let credential = credential_descriptor(&state, user.id(), &connection)
        .await
        .ok();
    Ok(Json(Some(view(connection, credential.as_ref()))))
}

fn view(
    connection: GitLabConnection,
    credential: Option<&oqto_protocol::credential_connections::CredentialDescriptor>,
) -> ConnectionView {
    let credential_ready = credential.is_some();
    let connected = credential.is_some_and(|credential| {
        connection.validated_credential_revision.as_deref() == Some(credential.revision.as_str())
    });
    ConnectionView {
        host: connection.host,
        path_prefix: connection.path_prefix,
        credential_id: connection.credential_id,
        revision: connection.revision,
        enabled: connection.enabled,
        event_categories: connection.event_categories,
        credential_ready,
        connected,
        gitlab_username: connection.gitlab_username,
    }
}

async fn put_config(
    State(state): State<AppState>,
    user: CurrentUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<SaveConfigRequest>,
) -> ApiResult<Json<ConnectionView>> {
    protected_submission(&state, peer, &headers)?;
    let (host, path_prefix) = normalize_instance_url(&request.instance_url).map_err(|_| {
        ApiError::bad_request(
            "GitLab URL must be a valid HTTPS origin with a safe path prefix on port 443",
        )
    })?;
    if request
        .event_categories
        .iter()
        .any(|category| !EVENT_CATEGORIES.contains(&category.as_str()))
    {
        return Err(ApiError::bad_request(
            "Unsupported GitLab notification category",
        ));
    }
    let mut categories = request.event_categories;
    categories.sort();
    categories.dedup();
    let repository = repository(&state)?;
    let connection = repository
        .save_connection(user.id(), &host, &path_prefix, request.enabled, &categories)
        .await
        .map_err(internal)?;
    // The setup screen can save the instance before a credential is present;
    // report readiness without ever reading or returning its value.
    let credential = credential_descriptor(&state, user.id(), &connection)
        .await
        .ok();
    Ok(Json(view(connection, credential.as_ref())))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestConnectionRequest {
    workspace_path: String,
}

#[derive(Debug, Serialize)]
struct TestConnectionResponse {
    connected: bool,
    username: String,
    credential_revision: String,
}

async fn test_connection(
    State(state): State<AppState>,
    user: CurrentUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<TestConnectionRequest>,
) -> ApiResult<Json<TestConnectionResponse>> {
    protected_submission(&state, peer, &headers)?;
    let workspace_path = canonical_workspace(&state, user.id(), &request.workspace_path).await?;
    let target = resolve_target_for_workspace_path(&state, user.id(), &request.workspace_path)
        .await
        .map_err(|_| ApiError::forbidden("GitLab is unavailable for this workspace"))?;
    if !matches!(target, ExecutionTarget::Personal) {
        return Err(ApiError::forbidden(
            "GitLab currently requires a personal workspace",
        ));
    }
    let repository = repository(&state)?;
    let connection = repository
        .connection(user.id())
        .await
        .map_err(internal)?
        .filter(|connection| connection.enabled)
        .ok_or_else(|| ApiError::bad_request("GitLab is not connected"))?;
    let runner = resolve_runner_for_target(&state, user.id(), &ExecutionTarget::Personal)
        .await
        .map_err(|_| ApiError::service_unavailable("GitLab credential runner unavailable"))?
        .ok_or_else(|| ApiError::service_unavailable("GitLab credential runner unavailable"))?;
    let credential = credential_descriptor_with_runner(&runner, user.id(), &connection).await?;
    let response = runner
        .gitlab_http(GitLabHttpDispatch {
            account_id: user.id().to_owned(),
            workspace_path,
            credential: credential.clone(),
            host: connection.host.clone(),
            path_prefix: connection.path_prefix.clone(),
            connection_revision: connection.revision,
            audit_event_id: format!("gitlab_test_{}", Uuid::new_v4().simple()),
            operation: GitLabOperation::CurrentUser,
        })
        .await
        .map_err(|_| ApiError::service_unavailable("GitLab connection test failed"))?;
    let username = response
        .get("username")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 255)
        .ok_or_else(|| ApiError::service_unavailable("GitLab returned an invalid user identity"))?
        .to_owned();
    if !repository
        .mark_connection_tested(
            user.id(),
            &connection.host,
            &connection.path_prefix,
            connection.revision,
            &credential.revision,
            &username,
        )
        .await
        .map_err(internal)?
    {
        return Err(ApiError::conflict(
            "GitLab connection changed during the connection test; retry",
        ));
    }
    Ok(Json(TestConnectionResponse {
        connected: true,
        username,
        credential_revision: credential.revision,
    }))
}

async fn delete_config(
    State(state): State<AppState>,
    user: CurrentUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    protected_submission(&state, peer, &headers)?;
    let repository = repository(&state)?;
    let (_, previously_unread) = repository
        .unread_counts(user.id())
        .await
        .map_err(internal)?;
    repository
        .delete_connection(user.id())
        .await
        .map_err(internal)?;
    for (workspace_path, _) in previously_unread {
        publish_unread_counts(&state, &repository, user.id(), &workspace_path).await?;
    }
    Ok(Json(json!({"disconnected": true})))
}

async fn get_mapping(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<WorkspaceQuery>,
) -> ApiResult<Json<Option<ProjectMapping>>> {
    require_real_auth(&state)?;
    let canonical = canonical_workspace(&state, user.id(), &query.workspace_path).await?;
    Ok(Json(
        repository(&state)?
            .mapping(user.id(), &canonical)
            .await
            .map_err(internal)?,
    ))
}

async fn put_mapping(
    State(state): State<AppState>,
    user: CurrentUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<SaveMappingRequest>,
) -> ApiResult<Json<ProjectMapping>> {
    protected_submission(&state, peer, &headers)?;
    let canonical = canonical_workspace(&state, user.id(), &request.workspace_path).await?;
    let repository = repository(&state)?;
    let connection = repository
        .connection(user.id())
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::bad_request("Connect a GitLab instance first"))?;
    let project_path = match (request.project_path_override, request.remote_url) {
        (Some(project), None) => {
            validate_project_path(&project)
                .map_err(|_| ApiError::bad_request("Invalid GitLab project path"))?;
            (project, "override")
        }
        (None, Some(remote)) => {
            let project = project_from_remote(&remote, &connection.host, &connection.path_prefix)
                .map_err(|_| {
                ApiError::bad_request(
                    "Workspace Git remote does not match the configured GitLab instance",
                )
            })?;
            (project, "remote")
        }
        _ => {
            return Err(ApiError::bad_request(
                "Provide exactly one Git remote or project override",
            ));
        }
    };
    if repository
        .project_mapped_elsewhere(user.id(), &canonical, &project_path.0)
        .await
        .map_err(internal)?
    {
        return Err(ApiError::conflict(
            "This GitLab project is already mapped to another workspace for this account",
        ));
    }
    let mapping = repository
        .save_mapping(user.id(), &canonical, &project_path.0, project_path.1)
        .await
        .map_err(internal)?;
    publish_unread_counts(&state, &repository, user.id(), &canonical).await?;
    Ok(Json(mapping))
}

async fn delete_mapping(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<WorkspaceQuery>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    protected_submission(&state, peer, &headers)?;
    let canonical = canonical_workspace(&state, user.id(), &query.workspace_path).await?;
    let repository = repository(&state)?;
    if !repository
        .delete_mapping(user.id(), &canonical)
        .await
        .map_err(internal)?
    {
        return Err(ApiError::not_found("GitLab workspace mapping not found"));
    }
    publish_unread_counts(&state, &repository, user.id(), &canonical).await?;
    Ok(Json(json!({"removed": true})))
}

async fn canonical_workspace(state: &AppState, account_id: &str, path: &str) -> ApiResult<String> {
    let canonical = super::trx::validate_workspace_path(state, account_id, path).await?;
    Ok(canonical.to_string_lossy().into_owned())
}

struct WorkspaceGitLab {
    repository: GitLabRepository,
    connection: GitLabConnection,
    mapping: ProjectMapping,
    canonical_path: String,
    runner: oqto_runner::client::RunnerClient,
    credential: oqto_protocol::credential_connections::CredentialDescriptor,
}

async fn workspace_gitlab(
    state: &AppState,
    account_id: &str,
    workspace_path: &str,
) -> ApiResult<WorkspaceGitLab> {
    let canonical_path = canonical_workspace(state, account_id, workspace_path).await?;
    let target = resolve_target_for_workspace_path(state, account_id, workspace_path)
        .await
        .map_err(|_| ApiError::forbidden("GitLab is unavailable for this workspace"))?;
    if !matches!(target, ExecutionTarget::Personal) {
        return Err(ApiError::forbidden(
            "GitLab currently requires a personal workspace",
        ));
    }
    let repository = repository(state)?;
    let mut connection = repository
        .connection(account_id)
        .await
        .map_err(internal)?
        .filter(|connection| connection.enabled)
        .ok_or_else(|| ApiError::bad_request("GitLab is not connected"))?;
    let mapping = repository
        .mapping(account_id, &canonical_path)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::bad_request("Map this workspace to a GitLab project first"))?;
    let runner = resolve_runner_for_target(state, account_id, &ExecutionTarget::Personal)
        .await
        .map_err(|_| ApiError::service_unavailable("GitLab credential runner unavailable"))?
        .ok_or_else(|| ApiError::service_unavailable("GitLab credential runner unavailable"))?;
    let credential = credential_descriptor_with_runner(&runner, account_id, &connection).await?;
    if connection.validated_credential_revision.as_deref() != Some(credential.revision.as_str()) {
        let response = runner
            .gitlab_http(GitLabHttpDispatch {
                account_id: account_id.to_owned(),
                workspace_path: canonical_path.clone(),
                credential: credential.clone(),
                host: connection.host.clone(),
                path_prefix: connection.path_prefix.clone(),
                connection_revision: connection.revision,
                audit_event_id: format!("gitlab_test_{}", Uuid::new_v4().simple()),
                operation: GitLabOperation::CurrentUser,
            })
            .await
            .map_err(|_| ApiError::service_unavailable("GitLab connection test failed"))?;
        let username = response
            .get("username")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 255)
            .ok_or_else(|| {
                ApiError::service_unavailable("GitLab returned an invalid user identity")
            })?;
        if !repository
            .mark_connection_tested(
                account_id,
                &connection.host,
                &connection.path_prefix,
                connection.revision,
                &credential.revision,
                username,
            )
            .await
            .map_err(internal)?
        {
            return Err(ApiError::conflict(
                "GitLab connection changed during the credential test; retry",
            ));
        }
        connection = repository
            .connection(account_id)
            .await
            .map_err(internal)?
            .filter(|current| current.enabled)
            .ok_or_else(|| ApiError::bad_request("GitLab connection changed"))?;
    }
    Ok(WorkspaceGitLab {
        repository,
        connection,
        mapping,
        canonical_path,
        runner,
        credential,
    })
}

async fn credential_descriptor(
    state: &AppState,
    account_id: &str,
    connection: &GitLabConnection,
) -> ApiResult<oqto_protocol::credential_connections::CredentialDescriptor> {
    let runner = resolve_runner_for_target(state, account_id, &ExecutionTarget::Personal)
        .await
        .map_err(|_| ApiError::service_unavailable("GitLab credential runner unavailable"))?
        .ok_or_else(|| ApiError::service_unavailable("GitLab credential runner unavailable"))?;
    credential_descriptor_with_runner(&runner, account_id, connection).await
}

async fn credential_descriptor_with_runner(
    runner: &oqto_runner::client::RunnerClient,
    account_id: &str,
    connection: &GitLabConnection,
) -> ApiResult<oqto_protocol::credential_connections::CredentialDescriptor> {
    if connection.credential_id != TOKEN_CREDENTIAL_ID {
        return Err(ApiError::forbidden("GitLab credential policy mismatch"));
    }
    runner
        .describe_credential(account_id.to_owned(), connection.credential_id.clone())
        .await
        .map_err(|_| {
            ApiError::bad_request("Save a 'gitlab' token in Host credentials and unlock the vault")
        })
}

async fn call_gitlab(
    context: &WorkspaceGitLab,
    account_id: &str,
    operation: GitLabOperation,
) -> ApiResult<Value> {
    let current = context
        .repository
        .connection(account_id)
        .await
        .map_err(internal)?;
    if !current.is_some_and(|current| {
        current.enabled
            && current.host == context.connection.host
            && current.path_prefix == context.connection.path_prefix
            && current.revision == context.connection.revision
            && current.validated_credential_revision.as_deref()
                == Some(context.credential.revision.as_str())
    }) {
        return Err(ApiError::forbidden(
            "GitLab connection changed; refresh the workspace and retry",
        ));
    }
    let mapping = context
        .repository
        .mapping(account_id, &context.canonical_path)
        .await
        .map_err(internal)?;
    if !mapping.is_some_and(|mapping| {
        mapping.project_path == context.mapping.project_path
            && mapping.source == context.mapping.source
    }) {
        return Err(ApiError::forbidden(
            "GitLab project mapping changed; refresh the workspace and retry",
        ));
    }
    let dispatch = GitLabHttpDispatch {
        account_id: account_id.to_owned(),
        workspace_path: context.canonical_path.clone(),
        credential: context.credential.clone(),
        host: context.connection.host.clone(),
        path_prefix: context.connection.path_prefix.clone(),
        connection_revision: context.connection.revision,
        audit_event_id: format!("gitlab_evt_{}", Uuid::new_v4().simple()),
        operation,
    };
    context
        .runner
        .gitlab_http(dispatch)
        .await
        .map_err(|_| ApiError::service_unavailable("GitLab operation failed or was denied"))
}

async fn get_inbox(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<WorkspaceQuery>,
) -> ApiResult<Json<Vec<InboxEvent>>> {
    require_real_auth(&state)?;
    let canonical = canonical_workspace(&state, user.id(), &query.workspace_path).await?;
    Ok(Json(
        repository(&state)?
            .inbox(user.id(), &canonical)
            .await
            .map_err(internal)?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PollInboxRequest {
    workspace_path: String,
}

async fn poll_inbox_now(
    State(state): State<AppState>,
    user: CurrentUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<PollInboxRequest>,
) -> ApiResult<Json<Vec<InboxEvent>>> {
    protected_submission(&state, peer, &headers)?;
    Ok(Json(
        poll_workspace(&state, user.id(), &request.workspace_path).await?,
    ))
}

#[derive(Debug, Serialize)]
struct GitLabTicketView {
    iid: u64,
    id: String,
    title: String,
    description: String,
    state: String,
    status: String,
    status_conflict: bool,
    priority: i32,
    issue_type: String,
    labels: Vec<String>,
    parent_id: Option<String>,
    blocked_by: Vec<String>,
    web_url: Option<String>,
    updated_at: String,
    merge_request_iid: Option<u64>,
}

#[derive(Debug, Serialize)]
struct GitLabTicketList {
    issues: Vec<GitLabTicketView>,
    truncated: bool,
}

async fn list_gitlab_issues(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<WorkspaceQuery>,
) -> ApiResult<Json<GitLabTicketList>> {
    require_real_auth(&state)?;
    let context = workspace_gitlab(&state, user.id(), &query.workspace_path).await?;
    let mut by_iid = std::collections::BTreeMap::<u64, Value>::new();
    let mut truncated = false;
    for scope in [
        GitLabIssueScope::AssignedToMe,
        GitLabIssueScope::CreatedByMe,
    ] {
        for page in 1..=MAX_POLL_PAGES {
            let value = call_gitlab(
                &context,
                user.id(),
                GitLabOperation::ListIssues {
                    project_path: context.mapping.project_path.clone(),
                    scope,
                    state: GitLabIssueState::All,
                    page,
                    updated_after: None,
                },
            )
            .await?;
            let Some(rows) = value.as_array() else {
                return Err(ApiError::service_unavailable(
                    "GitLab returned an invalid issue list",
                ));
            };
            for row in rows {
                if let Some(iid) = row.get("iid").and_then(Value::as_u64) {
                    by_iid.entry(iid).or_insert_with(|| row.clone());
                }
            }
            if rows.len() < GITLAB_PAGE_SIZE {
                break;
            }
            if page == MAX_POLL_PAGES {
                truncated = true;
            }
        }
    }
    let linked_merge_requests = context
        .repository
        .tracked_issue_merge_requests(
            user.id(),
            &context.canonical_path,
            &context.mapping.project_path,
        )
        .await
        .map_err(internal)?;
    let issues = by_iid
        .into_iter()
        .map(|(iid, row)| {
            let labels = row
                .get("labels")
                .and_then(Value::as_array)
                .map(|labels| {
                    labels
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let state = row
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("opened")
                .to_owned();
            let (status, status_conflict) = issue_status(&state, &labels);
            GitLabTicketView {
                iid,
                id: iid.to_string(),
                title: row
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("GitLab issue")
                    .to_owned(),
                description: row
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .chars()
                    .take(32 * 1024)
                    .collect(),
                state,
                status,
                status_conflict,
                priority: 3,
                issue_type: "issue".to_owned(),
                labels,
                parent_id: None,
                blocked_by: Vec::new(),
                web_url: safe_web_url(
                    row.get("web_url").and_then(Value::as_str),
                    &context.connection.host,
                    &context.connection.path_prefix,
                ),
                updated_at: row
                    .get("updated_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                merge_request_iid: linked_merge_requests.get(&iid).copied(),
            }
        })
        .collect();
    Ok(Json(GitLabTicketList { issues, truncated }))
}

/// Handle one typed Pi-to-Oqto GitLab RPC. The authenticated user and active
/// session come from the server-side event subscription, not from Pi payload.
pub(crate) async fn handle_agent_rpc(
    state: &AppState,
    account_id: &str,
    session_id: &str,
    request: GitLabAgentRpcRequest,
) -> ApiResult<Value> {
    if request.version != 1
        || request.request_id.trim().is_empty()
        || request.request_id.len() > 128
        || request.request_id.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request("Invalid GitLab RPC envelope"));
    }
    let session = state
        .sessions
        .get_session_for_user(account_id, session_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::forbidden("Active Oqto session is unavailable"))?;
    let session_workspace = canonical_workspace(state, account_id, &session.workspace_path).await?;
    let context = workspace_gitlab(state, account_id, &session_workspace).await?;
    let selection = context
        .repository
        .selected_issue_for_session(account_id, session_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            ApiError::forbidden("Select a GitLab issue in this session before using GitLab tools")
        })?;
    if selection.0 != context.canonical_path || selection.1 != context.mapping.project_path {
        return Err(ApiError::conflict(
            "The selected GitLab issue is stale because the workspace mapping changed",
        ));
    }
    let issue_iid = selection.2;
    let rpc_request_id = request.request_id.clone();

    let response = match request.operation {
        GitLabAgentOperation::ReadSelectedIssue => {
            let issue = call_gitlab(
                &context,
                account_id,
                GitLabOperation::GetIssue {
                    project_path: context.mapping.project_path.clone(),
                    issue_iid,
                },
            )
            .await?;
            let labels = issue
                .get("labels")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let state = issue
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("opened");
            let (status, status_conflict) = issue_status(state, &labels);
            Ok(json!({
                "iid": issue_iid,
                "title": issue.get("title").and_then(Value::as_str).unwrap_or("GitLab issue").chars().take(255).collect::<String>(),
                "description": issue.get("description").and_then(Value::as_str).unwrap_or_default().chars().take(32 * 1024).collect::<String>(),
                "state": state,
                "status": status,
                "status_conflict": status_conflict,
                "priority": 3,
                "issue_type": "issue",
                "labels": labels,
                "parent_id": Value::Null,
                "blocked_by": [],
                "web_url": safe_web_url(issue.get("web_url").and_then(Value::as_str), &context.connection.host, &context.connection.path_prefix),
                "updated_at": issue.get("updated_at").and_then(Value::as_str).unwrap_or_default(),
            }))
        }
        GitLabAgentOperation::ReadSelectedIssueNotes => {
            let mut notes_out = Vec::new();
            for page in 1..=1 {
                let response = call_gitlab(
                    &context,
                    account_id,
                    GitLabOperation::ListIssueNotes {
                        project_path: context.mapping.project_path.clone(),
                        issue_iid,
                        page,
                    },
                )
                .await?;
                let Some(notes) = response.as_array() else {
                    return Err(ApiError::service_unavailable(
                        "GitLab returned an invalid issue note list",
                    ));
                };
                for note in notes {
                    if note.get("system").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    notes_out.push(json!({
                        "id": note.get("id").and_then(Value::as_u64),
                        "body": note.get("body").and_then(Value::as_str).unwrap_or_default().chars().take(1024).collect::<String>(),
                        "author": note.get("author").and_then(|author| author.get("username")).and_then(Value::as_str).unwrap_or("GitLab user"),
                        "created_at": note.get("created_at").and_then(Value::as_str).unwrap_or_default(),
                    }));
                    if notes_out.len() >= 15 {
                        break;
                    }
                }
                if notes.len() < GITLAB_PAGE_SIZE || notes_out.len() >= 15 {
                    break;
                }
            }
            Ok(json!({"notes": notes_out}))
        }
        GitLabAgentOperation::ReadLinkedMergeRequest => {
            let tracked = tracked_merge_request(state, account_id, &context, issue_iid).await?;
            let merge_request_iid =
                tracked.get("iid").and_then(Value::as_u64).ok_or_else(|| {
                    ApiError::service_unavailable("Linked merge request IID is invalid")
                })?;
            let merge_request = call_gitlab(
                &context,
                account_id,
                GitLabOperation::GetMergeRequest {
                    project_path: context.mapping.project_path.clone(),
                    merge_request_iid,
                    include_changes: true,
                },
            )
            .await?;
            Ok(sanitize_agent_merge_request(&merge_request, &context))
        }
        GitLabAgentOperation::ReadLinkedMergeRequestDiscussions => {
            let merge_request =
                tracked_merge_request(state, account_id, &context, issue_iid).await?;
            let merge_request_iid = merge_request
                .get("iid")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    ApiError::service_unavailable("Linked merge request IID is invalid")
                })?;
            let mut result = Vec::new();
            for page in 1..=2 {
                let response = call_gitlab(
                    &context,
                    account_id,
                    GitLabOperation::ListMergeRequestDiscussions {
                        project_path: context.mapping.project_path.clone(),
                        merge_request_iid,
                        page,
                    },
                )
                .await?;
                let Some(discussions) = response.as_array() else {
                    return Err(ApiError::service_unavailable(
                        "GitLab returned an invalid discussion list",
                    ));
                };
                for discussion in discussions {
                    if result.len() >= 8 {
                        break;
                    }
                    let Some(notes) = discussion.get("notes").and_then(Value::as_array) else {
                        continue;
                    };
                    let notes: Vec<Value> = notes
                        .iter()
                        .filter(|note| note.get("system").and_then(Value::as_bool) != Some(true))
                        .take(2)
                        .map(|note| json!({
                            "id": note.get("id").and_then(Value::as_u64),
                            "body": note.get("body").and_then(Value::as_str).unwrap_or_default().chars().take(1024).collect::<String>(),
                            "author": note.get("author").and_then(|author| author.get("username")).and_then(Value::as_str).unwrap_or("GitLab user"),
                            "created_at": note.get("created_at").and_then(Value::as_str).unwrap_or_default(),
                        }))
                        .collect();
                    result.push(json!({
                        "id": discussion.get("id").and_then(Value::as_str),
                        "notes": notes,
                    }));
                }
                if discussions.len() < GITLAB_PAGE_SIZE || result.len() >= 8 {
                    break;
                }
            }
            Ok(json!({"discussions": result}))
        }
        GitLabAgentOperation::ReadProjectContext => {
            let project = call_gitlab(
                &context,
                account_id,
                GitLabOperation::GetProject {
                    project_path: context.mapping.project_path.clone(),
                },
            )
            .await?;
            let git_context = crate::api::ws_multiplexed::git::inspect_gitlab_workspace(
                state,
                account_id,
                &context.canonical_path,
                &context.connection.host,
                &context.connection.path_prefix,
                &context.mapping.project_path,
            )
            .await
            .map_err(ApiError::bad_request)?;
            Ok(json!({
                "project_path": context.mapping.project_path,
                "default_branch": project.get("default_branch").and_then(Value::as_str),
                "git": git_context,
            }))
        }
        GitLabAgentOperation::ProposeImplementationStart => {
            create_agent_proposal(
                &context,
                account_id,
                session_id,
                &rpc_request_id,
                issue_iid,
                "start_implementation",
                json!({}),
            )
            .await
        }
        GitLabAgentOperation::ProposeIssueUpdate {
            title,
            description,
            labels,
            state_event,
        } => {
            if title.is_none() && description.is_none() && labels.is_none() && state_event.is_none()
                || title
                    .as_ref()
                    .is_some_and(|value| value.trim().is_empty() || value.len() > 255)
                || description
                    .as_ref()
                    .is_some_and(|value| value.len() > 64 * 1024)
                || labels.as_ref().is_some_and(|items| {
                    items.len() > 50 || items.iter().any(|label| label.len() > 128)
                })
            {
                return Err(ApiError::bad_request("Invalid issue proposal"));
            }
            if let Some(labels) = labels.as_ref() {
                let workflow_labels: Vec<&str> = labels
                    .iter()
                    .filter(|label| canonical_workflow_labels().contains(&label.as_str()))
                    .map(String::as_str)
                    .collect();
                if !workflow_labels.is_empty() {
                    require_existing_workflow_labels(&context, account_id, &workflow_labels)
                        .await?;
                }
            }
            let current = call_gitlab(
                &context,
                account_id,
                GitLabOperation::GetIssue {
                    project_path: context.mapping.project_path.clone(),
                    issue_iid,
                },
            )
            .await?;
            let original_labels = current
                .get("labels")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if labels.is_some()
                && original_labels
                    .iter()
                    .filter(|label| canonical_workflow_labels().contains(&label.as_str()))
                    .count()
                    > 1
            {
                return Err(ApiError::conflict(
                    "Resolve conflicting Oqto workflow labels before proposing a status change",
                ));
            }
            let original = json!({
                "title": current.get("title").and_then(Value::as_str).unwrap_or_default(),
                "description": current.get("description").and_then(Value::as_str).unwrap_or_default(),
                "labels": original_labels,
                "state": current.get("state").and_then(Value::as_str).unwrap_or("opened"),
                "updated_at": current.get("updated_at").and_then(Value::as_str).unwrap_or_default(),
            });
            create_agent_proposal(
                &context,
                account_id,
                session_id,
                &rpc_request_id,
                issue_iid,
                "update_issue",
                json!({"title":title,"description":description,"labels":labels,"state_event":state_event,"original":original}),
            )
            .await
        }
        GitLabAgentOperation::ProposeIssueComment { body } => {
            if body.trim().is_empty() || body.len() > 32 * 1024 {
                return Err(ApiError::bad_request("Invalid issue comment proposal"));
            }
            create_agent_proposal(
                &context,
                account_id,
                session_id,
                &rpc_request_id,
                issue_iid,
                "comment",
                json!({"body":body}),
            )
            .await
        }
        GitLabAgentOperation::ProposeDiscussionReply {
            discussion_id,
            body,
        } => {
            if discussion_id.is_empty()
                || discussion_id.len() > 128
                || !discussion_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                || body.trim().is_empty()
                || body.len() > 32 * 1024
            {
                return Err(ApiError::bad_request(
                    "Invalid merge request discussion reply",
                ));
            }
            let merge_request =
                tracked_merge_request(state, account_id, &context, issue_iid).await?;
            let merge_request_iid = merge_request
                .get("iid")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    ApiError::service_unavailable("Linked merge request IID is invalid")
                })?;
            create_agent_proposal(
                &context,
                account_id,
                session_id,
                &rpc_request_id,
                issue_iid,
                "discussion_reply",
                json!({"merge_request_iid":merge_request_iid,"discussion_id":discussion_id,"body":body}),
            )
            .await
        }
        GitLabAgentOperation::ProposeInlineReview { comments } => {
            if comments.is_empty()
                || comments.len() > 50
                || comments.iter().any(|comment| {
                    comment.path.is_empty()
                        || comment.path.len() > 1024
                        || comment.line == 0
                        || comment.body.trim().is_empty()
                        || comment.body.len() > 32 * 1024
                })
            {
                return Err(ApiError::bad_request("Invalid inline review proposal"));
            }
            let merge_request =
                tracked_merge_request(state, account_id, &context, issue_iid).await?;
            let merge_request_iid = merge_request
                .get("iid")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    ApiError::service_unavailable("Linked merge request IID is invalid")
                })?;
            let merge_request = call_gitlab(
                &context,
                account_id,
                GitLabOperation::GetMergeRequest {
                    project_path: context.mapping.project_path.clone(),
                    merge_request_iid,
                    include_changes: true,
                },
            )
            .await?;
            let diff_refs = merge_request
                .get("diff_refs")
                .cloned()
                .ok_or_else(|| ApiError::conflict("GitLab diff references are not available"))?;
            create_agent_proposal(
                &context,
                account_id,
                session_id,
                &rpc_request_id,
                issue_iid,
                "inline_review",
                json!({"merge_request_iid":merge_request_iid,"comments":comments,"diff_refs":diff_refs}),
            )
            .await
        }
        GitLabAgentOperation::CreateOrUpdateDraft {
            remote_name,
            title,
            description,
            target_branch,
        } => {
            let git_context = crate::api::ws_multiplexed::git::inspect_gitlab_workspace(
                state,
                account_id,
                &context.canonical_path,
                &context.connection.host,
                &context.connection.path_prefix,
                &context.mapping.project_path,
            )
            .await
            .map_err(ApiError::bad_request)?;
            let matching: Vec<String> = git_context
                .get("matching_remote_names")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let origin_matches = matching.iter().any(|name| name == "origin");
            let selected_remote = match (origin_matches, remote_name) {
                (true, Some(remote)) if remote == "origin" => remote,
                (true, Some(_)) => {
                    return Err(ApiError::bad_request(
                        "A matching origin is configured; Oqto uses origin for agent-driven GitLab pushes",
                    ));
                }
                (true, None) => "origin".to_owned(),
                (false, Some(remote)) if matching.iter().any(|name| name == &remote) => remote,
                (false, Some(_)) => {
                    return Err(ApiError::bad_request(
                        "Selected remote does not match the mapped GitLab project",
                    ));
                }
                (false, None) => {
                    return Err(ApiError::conflict(format!(
                        "origin does not match this GitLab project. Ask the user which matching remote to push: {}",
                        matching.join(", ")
                    )));
                }
            };
            let merge_request = create_draft_for_context(
                state,
                account_id,
                &context,
                issue_iid,
                &selected_remote,
                target_branch.as_deref(),
                &title,
                &description,
                false,
            )
            .await?;
            Ok(sanitize_agent_merge_request(&merge_request, &context))
        }
    }?;
    let response_size = serde_json::to_vec(&response).map_err(internal)?.len();
    if response_size > 128 * 1024 {
        return Err(ApiError::bad_request(
            "GitLab agent RPC response exceeds the size limit",
        ));
    }
    Ok(response)
}

async fn create_agent_proposal(
    context: &WorkspaceGitLab,
    account_id: &str,
    session_id: &str,
    rpc_request_id: &str,
    issue_iid: u64,
    kind: &str,
    payload: Value,
) -> ApiResult<Value> {
    let digest =
        sha2::Sha256::digest(format!("{account_id}:{session_id}:{rpc_request_id}").as_bytes());
    let id = format!(
        "glprop_agent_{}",
        digest
            .iter()
            .map(|byte| format!("{:02x}", *byte))
            .collect::<String>()
    );
    if let Some(existing) = context
        .repository
        .proposal(account_id, &id)
        .await
        .map_err(internal)?
    {
        if existing.kind == kind
            && existing.payload == payload
            && existing.issue_iid == issue_iid
            && existing.session_id.as_deref() == Some(session_id)
        {
            return serde_json::to_value(existing).map_err(internal);
        }
        return Err(ApiError::conflict(
            "GitLab RPC request ID was already used for a different operation",
        ));
    }
    let proposal = Proposal {
        id,
        workspace_path: context.canonical_path.clone(),
        project_path: context.mapping.project_path.clone(),
        issue_iid,
        session_id: Some(session_id.to_owned()),
        kind: kind.to_owned(),
        payload,
        status: "pending".to_owned(),
        created_at: chrono::Utc::now().to_rfc3339(),
        session_stale: false,
    };
    context
        .repository
        .create_proposal(account_id, &proposal)
        .await
        .map_err(internal)?;
    serde_json::to_value(proposal).map_err(internal)
}

fn sanitize_agent_merge_request(merge_request: &Value, context: &WorkspaceGitLab) -> Value {
    let changes = merge_request
        .get("changes")
        .and_then(Value::as_array)
        .map(|changes| {
            changes
                .iter()
                .take(12)
                .map(|change| {
                    json!({
                        "old_path": change.get("old_path").and_then(Value::as_str).unwrap_or_default(),
                        "new_path": change.get("new_path").and_then(Value::as_str).unwrap_or_default(),
                        "diff": change.get("diff").and_then(Value::as_str).unwrap_or_default().chars().take(2 * 1024).collect::<String>(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "iid": merge_request.get("iid").and_then(Value::as_u64),
        "title": merge_request.get("title").and_then(Value::as_str).unwrap_or_default().chars().take(255).collect::<String>(),
        "description": merge_request.get("description").and_then(Value::as_str).unwrap_or_default().chars().take(8 * 1024).collect::<String>(),
        "state": merge_request.get("state").and_then(Value::as_str).unwrap_or_default(),
        "draft": merge_request.get("draft").and_then(Value::as_bool),
        "source_branch": merge_request.get("source_branch").and_then(Value::as_str),
        "target_branch": merge_request.get("target_branch").and_then(Value::as_str),
        "web_url": safe_web_url(merge_request.get("web_url").and_then(Value::as_str), &context.connection.host, &context.connection.path_prefix),
        "diff_refs": merge_request.get("diff_refs"),
        "changes": changes,
    })
}

fn issue_status(state: &str, labels: &[String]) -> (String, bool) {
    let workflow_labels = [
        ("oqto::ready", "ready"),
        ("oqto::in_progress", "in_progress"),
        ("oqto::blocked", "blocked"),
    ];
    let matched: Vec<&str> = workflow_labels
        .iter()
        .filter_map(|(label, status)| labels.iter().any(|value| value == label).then_some(*status))
        .collect();
    if state == "closed" {
        return ("closed".to_owned(), matched.len() > 1);
    }
    if matched.len() > 1 {
        return ("open".to_owned(), true);
    }
    (matched.first().copied().unwrap_or("open").to_owned(), false)
}

async fn project_label_names(
    context: &WorkspaceGitLab,
    account_id: &str,
) -> ApiResult<HashSet<String>> {
    let mut names = HashSet::new();
    for page in 1..=MAX_POLL_PAGES {
        let response = call_gitlab(
            context,
            account_id,
            GitLabOperation::ListProjectLabels {
                project_path: context.mapping.project_path.clone(),
                page,
            },
        )
        .await?;
        let Some(labels) = response.as_array() else {
            return Err(ApiError::service_unavailable(
                "GitLab returned an invalid project-label list",
            ));
        };
        for label in labels {
            if let Some(name) = label.get("name").and_then(Value::as_str) {
                names.insert(name.to_owned());
            }
        }
        if labels.len() < GITLAB_PAGE_SIZE {
            return Ok(names);
        }
    }
    Ok(names)
}

fn canonical_workflow_labels() -> [&'static str; 3] {
    ["oqto::ready", "oqto::in_progress", "oqto::blocked"]
}

async fn require_existing_workflow_labels(
    context: &WorkspaceGitLab,
    account_id: &str,
    wanted: &[&str],
) -> ApiResult<()> {
    let existing = project_label_names(context, account_id).await?;
    if let Some(missing) = wanted.iter().find(|label| !existing.contains(**label)) {
        return Err(ApiError::bad_request(format!(
            "Create the GitLab label '{missing}' in this project before using Oqto workflow statuses"
        )));
    }
    Ok(())
}

async fn poll_workspace(
    state: &AppState,
    account_id: &str,
    workspace_path: &str,
) -> ApiResult<Vec<InboxEvent>> {
    let canonical = canonical_workspace(state, account_id, workspace_path).await?;
    let key = format!("{account_id}:{canonical}");
    let locks = POLL_LOCKS.get_or_init(DashMap::new);
    let lock = locks
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    let _poll_guard = lock.lock().await;
    let context = workspace_gitlab(state, account_id, &canonical).await?;
    let repository = context.repository.clone();
    for category in &context.connection.event_categories {
        match category.as_str() {
            "assigned_issues" => {
                poll_issues(
                    &repository,
                    &context,
                    account_id,
                    category,
                    "assigned_issues",
                    GitLabIssueScope::AssignedToMe,
                )
                .await?
            }
            "authored_issues" => {
                poll_issues(
                    &repository,
                    &context,
                    account_id,
                    category,
                    "authored_issues",
                    GitLabIssueScope::CreatedByMe,
                )
                .await?
            }
            "project_activity" => {
                poll_issues(
                    &repository,
                    &context,
                    account_id,
                    category,
                    "project_issues",
                    GitLabIssueScope::All,
                )
                .await?;
                poll_merge_requests(
                    &repository,
                    &context,
                    account_id,
                    category,
                    "project_mrs",
                    GitLabMergeRequestScope::All,
                    GitLabMergeRequestState::All,
                    None,
                )
                .await?;
            }
            "mentions_review_requests" => {
                let (reviewer_id, username) = current_gitlab_identity(&context, account_id).await?;
                poll_merge_requests(
                    &repository,
                    &context,
                    account_id,
                    category,
                    "review_requests",
                    GitLabMergeRequestScope::All,
                    GitLabMergeRequestState::Opened,
                    Some(reviewer_id),
                )
                .await?;
                poll_issue_mentions(&repository, &context, account_id, &username).await?;
                poll_merge_request_mentions(&repository, &context, account_id, &username).await?;
            }
            _ => {}
        }
    }
    let discussion_category = if context
        .connection
        .event_categories
        .iter()
        .any(|value| value == "mentions_review_requests")
    {
        "mentions_review_requests"
    } else if context
        .connection
        .event_categories
        .iter()
        .any(|value| value == "project_activity")
    {
        "project_activity"
    } else {
        "mentions_review_requests"
    };
    poll_linked_discussions(&repository, &context, account_id, discussion_category).await?;
    let events = repository
        .inbox(account_id, &context.canonical_path)
        .await
        .map_err(internal)?;
    publish_unread_counts(state, &repository, account_id, &context.canonical_path).await?;
    Ok(events)
}

/// Start the backend-owned per-account GitLab poller. The returned task is
/// joined after the HTTP server begins graceful shutdown.
pub fn start_background_polling(
    state: AppState,
    cancellation: tokio_util::sync::CancellationToken,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    let interval_seconds = match std::env::var("OQTO_GITLAB_POLL_INTERVAL_SECONDS") {
        Ok(raw) => parse_poll_interval(Some(&raw))?,
        Err(std::env::VarError::NotPresent) => parse_poll_interval(None)?,
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("OQTO_GITLAB_POLL_INTERVAL_SECONDS must be valid Unicode")
        }
    };
    if state.apps.is_none() {
        return Ok(None);
    }
    let handle = tokio::spawn(async move {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
        let mut failures: std::collections::HashMap<String, (u32, tokio::time::Instant)> =
            std::collections::HashMap::new();
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_seconds));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => break,
                _ = interval.tick() => {
                    let target_repository = match repository(&state) {
                        Ok(repository) => repository,
                        Err(error) => {
                            tracing::warn!(error = %error, "GitLab poll repository unavailable");
                            continue;
                        }
                    };
                    if let Err(error) = target_repository.expire_and_prune().await {
                        tracing::warn!(error = %error, "GitLab retention cleanup failed");
                    }
                    let targets = match target_repository.poll_targets().await {
                        Ok(targets) => targets,
                        Err(error) => {
                            tracing::warn!(error = %error, "GitLab poll target enumeration failed");
                            continue;
                        }
                    };
                    let now = tokio::time::Instant::now();
                    let active_keys: HashSet<String> = targets
                        .iter()
                        .map(|(account_id, workspace_path)| format!("{account_id}:{workspace_path}"))
                        .collect();
                    failures.retain(|key, _| active_keys.contains(key));
                    let mut tasks = tokio::task::JoinSet::new();
                    for (account_id, workspace_path) in targets {
                        let key = format!("{account_id}:{workspace_path}");
                        if failures.get(&key).is_some_and(|(_, next)| *next > now) {
                            continue;
                        }
                        let permit = semaphore.clone();
                        let task_state = state.clone();
                        let task_cancel = cancellation.child_token();
                        let task_key = key.clone();
                        tasks.spawn(async move {
                            let Ok(_permit) = permit.acquire_owned().await else {
                                return (task_key, Err(ApiError::service_unavailable("GitLab poll capacity unavailable")));
                            };
                            let result = tokio::select! {
                                _ = task_cancel.cancelled() => Err(ApiError::service_unavailable("GitLab poll cancelled")),
                                result = poll_workspace(&task_state, &account_id, &workspace_path) => result.map(|_| ()),
                            };
                            (task_key, result)
                        });
                    }
                    loop {
                        tokio::select! {
                            _ = cancellation.cancelled() => {
                                tasks.abort_all();
                                while tasks.join_next().await.is_some() {}
                                return;
                            }
                            result = tasks.join_next() => {
                                let Some(result) = result else { break; };
                                match result {
                                    Ok((key, Ok(()))) => { failures.remove(&key); }
                                    Ok((key, Err(error))) => {
                                        let (attempts, _) = failures.get(&key).copied().unwrap_or((0, now));
                                        let attempts = attempts.saturating_add(1);
                                        let exponent = attempts.min(6);
                                        let delay = std::time::Duration::from_secs(interval_seconds)
                                            .saturating_mul(1_u32 << exponent)
                                            .min(std::time::Duration::from_secs(3600));
                                        failures.insert(key.clone(), (attempts, tokio::time::Instant::now() + delay));
                                        tracing::warn!(workspace = %key, error = %error, backoff_secs = delay.as_secs(), "GitLab poll failed");
                                    }
                                    Err(error) => tracing::warn!(error = %error, "GitLab poll task failed"),
                                }
                            }
                        }
                    }
                }
            }
        }
    });
    Ok(Some(handle))
}

fn parse_poll_interval(value: Option<&str>) -> anyhow::Result<u64> {
    let Some(value) = value else {
        return Ok(60);
    };
    let seconds: u64 = value
        .parse()
        .context("OQTO_GITLAB_POLL_INTERVAL_SECONDS must be an integer")?;
    anyhow::ensure!(
        (60..=3600).contains(&seconds),
        "OQTO_GITLAB_POLL_INTERVAL_SECONDS must be in 60..=3600"
    );
    Ok(seconds)
}

async fn publish_unread_counts(
    state: &AppState,
    repository: &GitLabRepository,
    account_id: &str,
    workspace_path: &str,
) -> ApiResult<()> {
    let (total_unread, workspaces) = repository
        .unread_counts(account_id)
        .await
        .map_err(internal)?;
    let workspace_unread = workspaces
        .iter()
        .find_map(|(path, count)| (path == workspace_path).then_some(*count))
        .unwrap_or_default();
    state
        .ws_hub
        .send_to_user(
            account_id,
            crate::ws::types::WsEvent::GitLabInboxUpdated {
                workspace_path: workspace_path.to_owned(),
                total_unread,
                workspace_unread,
            },
        )
        .await;
    Ok(())
}

async fn current_gitlab_identity(
    context: &WorkspaceGitLab,
    account_id: &str,
) -> ApiResult<(u64, String)> {
    let response = call_gitlab(context, account_id, GitLabOperation::CurrentUser).await?;
    let id = response
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| ApiError::service_unavailable("GitLab returned an invalid user identity"))?;
    let username = response
        .get("username")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 255)
        .ok_or_else(|| ApiError::service_unavailable("GitLab returned an invalid username"))?
        .to_owned();
    Ok((id, username))
}

#[derive(Debug, Serialize)]
struct WorkspaceUnreadCount {
    workspace_path: String,
    count: i64,
}

#[derive(Debug, Serialize)]
struct UnreadCountsResponse {
    total: i64,
    workspaces: Vec<WorkspaceUnreadCount>,
}

async fn get_unread_counts(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<Json<UnreadCountsResponse>> {
    require_real_auth(&state)?;
    let (total, workspaces) = repository(&state)?
        .unread_counts(user.id())
        .await
        .map_err(internal)?;
    Ok(Json(UnreadCountsResponse {
        total,
        workspaces: workspaces
            .into_iter()
            .map(|(workspace_path, count)| WorkspaceUnreadCount {
                workspace_path,
                count,
            })
            .collect(),
    }))
}

async fn mark_event_read(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(event_id): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    protected_submission(&state, peer, &headers)?;
    let repository = repository(&state)?;
    let Some(workspace_path) = repository
        .mark_event_read(user.id(), &event_id)
        .await
        .map_err(internal)?
    else {
        return Err(ApiError::not_found("GitLab inbox event not found"));
    };
    publish_unread_counts(&state, &repository, user.id(), &workspace_path).await?;
    Ok(Json(json!({"read": true})))
}

async fn poll_linked_discussions(
    repository: &GitLabRepository,
    context: &WorkspaceGitLab,
    account_id: &str,
    event_category: &str,
) -> ApiResult<()> {
    let workflows = repository
        .tracked_merge_requests(
            account_id,
            &context.canonical_path,
            &context.mapping.project_path,
        )
        .await
        .map_err(internal)?;
    let initial_cutoff = initial_poll_cursor(&context.connection);
    let total_workflows = workflows.len();
    if total_workflows == 0 {
        return Ok(());
    }
    let previous_offset = repository
        .cursor(
            account_id,
            &context.canonical_path,
            &context.mapping.project_path,
            "tracked_discussion_offset",
        )
        .await
        .map_err(internal)?
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default()
        % total_workflows;
    let count = total_workflows.min(MAX_TRACKED_MR_DISCUSSIONS_PER_POLL);
    let selected_workflows: Vec<_> = (0..count)
        .map(|index| workflows[(previous_offset + index) % total_workflows].clone())
        .collect();
    if total_workflows > count {
        repository
            .save_cursor(
                account_id,
                &context.canonical_path,
                &context.mapping.project_path,
                "tracked_discussion_offset",
                &((previous_offset + count) % total_workflows).to_string(),
            )
            .await
            .map_err(internal)?;
    }
    for workflow in selected_workflows {
        let Ok(merge_request_iid) = u64::try_from(workflow.merge_request_iid) else {
            continue;
        };
        let mr_iid = merge_request_iid;
        let related_issue_iid = (workflow.issue_iid > 0).then_some(workflow.issue_iid);
        for page in 1..=MAX_POLL_PAGES {
            let value = call_gitlab(
                context,
                account_id,
                GitLabOperation::ListMergeRequestDiscussions {
                    project_path: workflow.project_path.clone(),
                    merge_request_iid,
                    page,
                },
            )
            .await?;
            let Some(discussions) = value.as_array() else {
                return Err(ApiError::service_unavailable(
                    "GitLab returned an invalid discussion list",
                ));
            };
            for discussion in discussions {
                let Some(notes) = discussion.get("notes").and_then(Value::as_array) else {
                    continue;
                };
                for note in notes {
                    if note.get("system").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    let note_id = note.get("id").and_then(Value::as_u64).unwrap_or_default();
                    let occurred_at = note
                        .get("updated_at")
                        .or_else(|| note.get("created_at"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if note_id == 0 || occurred_at.is_empty() {
                        continue;
                    }
                    if !timestamp_at_or_after(occurred_at, Some(initial_cutoff.as_str())) {
                        continue;
                    }
                    let body = note.get("body").and_then(Value::as_str).unwrap_or_default();
                    if body.trim().is_empty() {
                        continue;
                    }
                    let author = note
                        .get("author")
                        .and_then(|author| author.get("username"))
                        .and_then(Value::as_str)
                        .unwrap_or("GitLab user");
                    let event_key = format!(
                        "discussion:{}:{}:{mr_iid}:{note_id}:{occurred_at}",
                        context.canonical_path, workflow.project_path
                    );
                    let project_path = format!("/{}", workflow.project_path);
                    repository
                        .record_event(
                            account_id,
                            &InboxEvent {
                                id: Uuid::new_v4().to_string(),
                                workspace_path: context.canonical_path.clone(),
                                project_path: workflow.project_path.clone(),
                                category: event_category.to_owned(),
                                categories: vec![event_category.to_owned()],
                                coalesce_key: None,
                                entity_kind: "merge_request".to_owned(),
                                entity_iid: workflow.merge_request_iid,
                                related_issue_iid,
                                discussion_id: discussion
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                                session_id: workflow.session_id.clone(),
                                title: format!("New discussion on merge request !{mr_iid}"),
                                summary: summarize(&format!("{author}: {body}")),
                                url: Some(format!(
                                    "https://{}{}{}/-/merge_requests/{mr_iid}",
                                    context.connection.host,
                                    context.connection.path_prefix,
                                    project_path
                                )),
                                occurred_at: occurred_at.to_owned(),
                                read_at: None,
                            },
                            &event_key,
                        )
                        .await
                        .map_err(internal)?;
                }
            }
            if discussions.len() < GITLAB_PAGE_SIZE {
                break;
            }
        }
    }
    Ok(())
}

async fn poll_issues(
    repository: &GitLabRepository,
    context: &WorkspaceGitLab,
    account_id: &str,
    event_category: &str,
    cursor_category: &str,
    scope: GitLabIssueScope,
) -> ApiResult<()> {
    let cursor = repository
        .cursor(
            account_id,
            &context.canonical_path,
            &context.mapping.project_path,
            cursor_category,
        )
        .await
        .map_err(internal)?;
    let mut cursor_state = decode_poll_cursor(cursor, initial_poll_cursor(&context.connection));
    let mut page = cursor_state.next_page.max(1);
    for _ in 0..MAX_POLL_PAGES {
        let response = call_gitlab(
            context,
            account_id,
            GitLabOperation::ListIssues {
                project_path: context.mapping.project_path.clone(),
                scope,
                state: GitLabIssueState::All,
                page,
                updated_after: cursor_state.updated_after.clone(),
            },
        )
        .await?;
        let Some(rows) = response.as_array() else {
            return Err(ApiError::service_unavailable(
                "GitLab returned an invalid issue list",
            ));
        };
        for row in rows {
            let iid = row.get("iid").and_then(Value::as_u64).unwrap_or_default();
            let Some(entity_iid) = i64::try_from(iid).ok() else {
                continue;
            };
            let updated_at = row
                .get("updated_at")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if iid == 0 || updated_at.is_empty() {
                continue;
            }
            if cursor_state
                .pending_latest
                .as_deref()
                .is_none_or(|current| updated_at > current)
            {
                cursor_state.pending_latest = Some(updated_at.to_owned());
            }
            let event_key = format!(
                "issue:{}:{}:{iid}:{updated_at}",
                context.canonical_path, context.mapping.project_path
            );
            let event = InboxEvent {
                id: Uuid::new_v4().to_string(),
                workspace_path: context.canonical_path.clone(),
                project_path: context.mapping.project_path.clone(),
                category: event_category.to_owned(),
                categories: vec![event_category.to_owned()],
                coalesce_key: Some(format!(
                    "{}:{}:issue:{iid}",
                    context.canonical_path, context.mapping.project_path
                )),
                entity_kind: "issue".to_owned(),
                entity_iid,
                related_issue_iid: None,
                discussion_id: None,
                session_id: None,
                title: row
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("GitLab issue update")
                    .chars()
                    .take(255)
                    .collect(),
                summary: summarize(
                    row.get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
                url: safe_web_url(
                    row.get("web_url").and_then(Value::as_str),
                    &context.connection.host,
                    &context.connection.path_prefix,
                ),
                occurred_at: updated_at.to_owned(),
                read_at: None,
            };
            repository
                .record_event(account_id, &event, &event_key)
                .await
                .map_err(internal)?;
        }
        if rows.len() < GITLAB_PAGE_SIZE {
            if let Some(latest) = cursor_state.pending_latest.take() {
                cursor_state.updated_after = Some(overlap_timestamp(&latest));
            }
            cursor_state.next_page = 1;
            cursor_state.next_offset = 0;
            let next_cursor = serde_json::to_string(&cursor_state).map_err(internal)?;
            repository
                .save_cursor(
                    account_id,
                    &context.canonical_path,
                    &context.mapping.project_path,
                    cursor_category,
                    &next_cursor,
                )
                .await
                .map_err(internal)?;
            return Ok(());
        }
        page = page.saturating_add(1);
    }
    cursor_state.next_page = page;
    let next_cursor = serde_json::to_string(&cursor_state).map_err(internal)?;
    repository
        .save_cursor(
            account_id,
            &context.canonical_path,
            &context.mapping.project_path,
            cursor_category,
            &next_cursor,
        )
        .await
        .map_err(internal)?;
    Ok(())
}

fn contains_gitlab_mention(text: &str, username: &str) -> bool {
    let pattern = format!(
        r"(?i)(^|[^A-Za-z0-9_])@{}($|[^A-Za-z0-9_])",
        regex::escape(username)
    );
    regex::Regex::new(&pattern).is_ok_and(|regex| regex.is_match(text))
}

async fn poll_issue_mentions(
    repository: &GitLabRepository,
    context: &WorkspaceGitLab,
    account_id: &str,
    username: &str,
) -> ApiResult<()> {
    let cursor = repository
        .cursor(
            account_id,
            &context.canonical_path,
            &context.mapping.project_path,
            "mentions_issues",
        )
        .await
        .map_err(internal)?;
    let mut cursor_state = decode_poll_cursor(cursor, initial_poll_cursor(&context.connection));
    let mut page = cursor_state.next_page.max(1);
    let mut offset = cursor_state.next_offset;
    let mut lookups = 0;
    for _ in 0..MAX_POLL_PAGES {
        let response = call_gitlab(
            context,
            account_id,
            GitLabOperation::ListIssues {
                project_path: context.mapping.project_path.clone(),
                scope: GitLabIssueScope::All,
                state: GitLabIssueState::All,
                page,
                updated_after: cursor_state.updated_after.clone(),
            },
        )
        .await?;
        let Some(rows) = response.as_array() else {
            return Err(ApiError::service_unavailable(
                "GitLab returned an invalid mention search result",
            ));
        };
        for (index, row) in rows.iter().enumerate().skip(offset) {
            if lookups >= MAX_MENTION_ISSUE_LOOKUPS {
                cursor_state.next_page = page;
                cursor_state.next_offset = index;
                save_poll_cursor(
                    repository,
                    account_id,
                    context,
                    "mentions_issues",
                    &cursor_state,
                )
                .await?;
                return Ok(());
            }
            lookups += 1;
            let iid = row.get("iid").and_then(Value::as_u64).unwrap_or_default();
            let Some(entity_iid) = i64::try_from(iid).ok() else {
                continue;
            };
            let updated_at = row
                .get("updated_at")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if iid == 0 || updated_at.is_empty() {
                continue;
            }
            if cursor_state
                .pending_latest
                .as_deref()
                .is_none_or(|current| updated_at > current)
            {
                cursor_state.pending_latest = Some(updated_at.to_owned());
            }
            let title = row
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("GitLab issue");
            let description = row
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let issue_url = safe_web_url(
                row.get("web_url").and_then(Value::as_str),
                &context.connection.host,
                &context.connection.path_prefix,
            );
            if contains_gitlab_mention(description, username) {
                let event_key = format!(
                    "issue:{}:{}:{iid}:{updated_at}",
                    context.canonical_path, context.mapping.project_path
                );
                repository
                    .record_event(
                        account_id,
                        &InboxEvent {
                            id: Uuid::new_v4().to_string(),
                            workspace_path: context.canonical_path.clone(),
                            project_path: context.mapping.project_path.clone(),
                            category: "mentions_review_requests".to_owned(),
                            categories: vec!["mentions_review_requests".to_owned()],
                            coalesce_key: Some(format!(
                                "{}:{}:issue:{iid}",
                                context.canonical_path, context.mapping.project_path
                            )),
                            entity_kind: "issue".to_owned(),
                            entity_iid,
                            related_issue_iid: None,
                            discussion_id: None,
                            session_id: None,
                            title: title.chars().take(255).collect(),
                            summary: summarize(description),
                            url: issue_url.clone(),
                            occurred_at: updated_at.to_owned(),
                            read_at: None,
                        },
                        &event_key,
                    )
                    .await
                    .map_err(internal)?;
            }
            for note_page in 1..=MAX_POLL_PAGES {
                let notes = call_gitlab(
                    context,
                    account_id,
                    GitLabOperation::ListIssueNotes {
                        project_path: context.mapping.project_path.clone(),
                        issue_iid: iid,
                        page: note_page,
                    },
                )
                .await?;
                let Some(notes) = notes.as_array() else {
                    return Err(ApiError::service_unavailable(
                        "GitLab returned an invalid issue note list",
                    ));
                };
                for note in notes {
                    if note.get("system").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    let note_time = note
                        .get("updated_at")
                        .or_else(|| note.get("created_at"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !timestamp_at_or_after(note_time, cursor_state.updated_after.as_deref()) {
                        continue;
                    }
                    let body = note.get("body").and_then(Value::as_str).unwrap_or_default();
                    if !contains_gitlab_mention(body, username) {
                        continue;
                    }
                    let note_id = note.get("id").and_then(Value::as_u64).unwrap_or_default();
                    if note_id == 0 || note_time.is_empty() {
                        continue;
                    }
                    let author = note
                        .get("author")
                        .and_then(|author| author.get("username"))
                        .and_then(Value::as_str)
                        .unwrap_or("GitLab user");
                    let event_key = format!(
                        "issue-note:{}:{}:{iid}:{note_id}:{note_time}",
                        context.canonical_path, context.mapping.project_path
                    );
                    repository
                        .record_event(
                            account_id,
                            &InboxEvent {
                                id: Uuid::new_v4().to_string(),
                                workspace_path: context.canonical_path.clone(),
                                project_path: context.mapping.project_path.clone(),
                                category: "mentions_review_requests".to_owned(),
                                categories: vec!["mentions_review_requests".to_owned()],
                                coalesce_key: None,
                                entity_kind: "issue".to_owned(),
                                entity_iid,
                                related_issue_iid: None,
                                discussion_id: None,
                                session_id: None,
                                title: format!("Mention on issue #{iid}: {title}")
                                    .chars()
                                    .take(255)
                                    .collect(),
                                summary: summarize(&format!("{author}: {body}")),
                                url: issue_url.clone(),
                                occurred_at: note_time.to_owned(),
                                read_at: None,
                            },
                            &event_key,
                        )
                        .await
                        .map_err(internal)?;
                }
                if notes.len() < GITLAB_PAGE_SIZE {
                    break;
                }
            }
        }
        if rows.len() < GITLAB_PAGE_SIZE {
            if let Some(latest) = cursor_state.pending_latest.take() {
                cursor_state.updated_after = Some(overlap_timestamp(&latest));
            }
            cursor_state.next_page = 1;
            cursor_state.next_offset = 0;
            save_poll_cursor(
                repository,
                account_id,
                context,
                "mentions_issues",
                &cursor_state,
            )
            .await?;
            return Ok(());
        }
        page = page.saturating_add(1);
        offset = 0;
    }
    cursor_state.next_page = page;
    cursor_state.next_offset = offset;
    save_poll_cursor(
        repository,
        account_id,
        context,
        "mentions_issues",
        &cursor_state,
    )
    .await
}

async fn save_poll_cursor(
    repository: &GitLabRepository,
    account_id: &str,
    context: &WorkspaceGitLab,
    category: &str,
    cursor: &GitLabPollCursor,
) -> ApiResult<()> {
    let value = serde_json::to_string(cursor).map_err(internal)?;
    repository
        .save_cursor(
            account_id,
            &context.canonical_path,
            &context.mapping.project_path,
            category,
            &value,
        )
        .await
        .map_err(internal)
}

async fn poll_merge_request_mentions(
    repository: &GitLabRepository,
    context: &WorkspaceGitLab,
    account_id: &str,
    username: &str,
) -> ApiResult<()> {
    let cursor = repository
        .cursor(
            account_id,
            &context.canonical_path,
            &context.mapping.project_path,
            "mentions_mrs",
        )
        .await
        .map_err(internal)?;
    let mut cursor_state = decode_poll_cursor(cursor, initial_poll_cursor(&context.connection));
    let mut page = cursor_state.next_page.max(1);
    let mut offset = cursor_state.next_offset;
    let mut lookups = 0;
    for _ in 0..MAX_POLL_PAGES {
        let response = call_gitlab(
            context,
            account_id,
            GitLabOperation::ListMergeRequests {
                project_path: context.mapping.project_path.clone(),
                scope: GitLabMergeRequestScope::All,
                state: GitLabMergeRequestState::All,
                reviewer_id: None,
                updated_after: cursor_state.updated_after.clone(),
                page,
            },
        )
        .await?;
        let Some(rows) = response.as_array() else {
            return Err(ApiError::service_unavailable(
                "GitLab returned an invalid merge request mention search",
            ));
        };
        for (index, row) in rows.iter().enumerate().skip(offset) {
            let iid = row.get("iid").and_then(Value::as_u64).unwrap_or_default();
            let Some(entity_iid) = i64::try_from(iid).ok() else {
                continue;
            };
            let updated_at = row
                .get("updated_at")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if iid == 0 || updated_at.is_empty() {
                continue;
            }
            if cursor_state
                .pending_latest
                .as_deref()
                .is_none_or(|current| updated_at > current)
            {
                cursor_state.pending_latest = Some(updated_at.to_owned());
            }
            let title = row
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("GitLab merge request");
            let description = row
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let related_issue_iid = closing_issue_iid(description);
            let url = safe_web_url(
                row.get("web_url").and_then(Value::as_str),
                &context.connection.host,
                &context.connection.path_prefix,
            );
            if contains_gitlab_mention(description, username) {
                let event_key = format!(
                    "mr:{}:{}:{iid}:{updated_at}",
                    context.canonical_path, context.mapping.project_path
                );
                repository
                    .record_event(
                        account_id,
                        &InboxEvent {
                            id: Uuid::new_v4().to_string(),
                            workspace_path: context.canonical_path.clone(),
                            project_path: context.mapping.project_path.clone(),
                            category: "mentions_review_requests".to_owned(),
                            categories: vec!["mentions_review_requests".to_owned()],
                            coalesce_key: Some(format!(
                                "{}:{}:merge_request:{iid}",
                                context.canonical_path, context.mapping.project_path
                            )),
                            entity_kind: "merge_request".to_owned(),
                            entity_iid,
                            related_issue_iid,
                            discussion_id: None,
                            session_id: None,
                            title: title.chars().take(255).collect(),
                            summary: summarize(description),
                            url: url.clone(),
                            occurred_at: updated_at.to_owned(),
                            read_at: None,
                        },
                        &event_key,
                    )
                    .await
                    .map_err(internal)?;
            }

            if lookups >= MAX_MENTION_ISSUE_LOOKUPS {
                cursor_state.next_page = page;
                cursor_state.next_offset = index;
                save_poll_cursor(
                    repository,
                    account_id,
                    context,
                    "mentions_mrs",
                    &cursor_state,
                )
                .await?;
                return Ok(());
            }
            lookups += 1;
            for note_page in 1..=MAX_POLL_PAGES {
                let value = call_gitlab(
                    context,
                    account_id,
                    GitLabOperation::ListMergeRequestNotes {
                        project_path: context.mapping.project_path.clone(),
                        merge_request_iid: iid,
                        page: note_page,
                    },
                )
                .await?;
                let Some(notes) = value.as_array() else {
                    return Err(ApiError::service_unavailable(
                        "GitLab returned an invalid merge request note list",
                    ));
                };
                for note in notes {
                    if note.get("system").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    let note_time = note
                        .get("updated_at")
                        .or_else(|| note.get("created_at"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !timestamp_at_or_after(note_time, cursor_state.updated_after.as_deref()) {
                        continue;
                    }
                    let body = note.get("body").and_then(Value::as_str).unwrap_or_default();
                    if !contains_gitlab_mention(body, username) {
                        continue;
                    }
                    let note_id = note.get("id").and_then(Value::as_u64).unwrap_or_default();
                    if note_id == 0 || note_time.is_empty() {
                        continue;
                    }
                    let author = note
                        .get("author")
                        .and_then(|author| author.get("username"))
                        .and_then(Value::as_str)
                        .unwrap_or("GitLab user");
                    let event_key = format!(
                        "discussion:{}:{}:{iid}:{note_id}:{note_time}",
                        context.canonical_path, context.mapping.project_path
                    );
                    repository
                        .record_event(
                            account_id,
                            &InboxEvent {
                                id: Uuid::new_v4().to_string(),
                                workspace_path: context.canonical_path.clone(),
                                project_path: context.mapping.project_path.clone(),
                                category: "mentions_review_requests".to_owned(),
                                categories: vec!["mentions_review_requests".to_owned()],
                                coalesce_key: None,
                                entity_kind: "merge_request".to_owned(),
                                entity_iid,
                                related_issue_iid,
                                discussion_id: None,
                                session_id: None,
                                title: format!("Mention on merge request !{iid}: {title}")
                                    .chars()
                                    .take(255)
                                    .collect(),
                                summary: summarize(&format!("{author}: {body}")),
                                url: url.clone(),
                                occurred_at: note_time.to_owned(),
                                read_at: None,
                            },
                            &event_key,
                        )
                        .await
                        .map_err(internal)?;
                }
                if notes.len() < GITLAB_PAGE_SIZE {
                    break;
                }
            }
        }
        if rows.len() < GITLAB_PAGE_SIZE {
            if let Some(latest) = cursor_state.pending_latest.take() {
                cursor_state.updated_after = Some(overlap_timestamp(&latest));
            }
            cursor_state.next_page = 1;
            cursor_state.next_offset = 0;
            save_poll_cursor(
                repository,
                account_id,
                context,
                "mentions_mrs",
                &cursor_state,
            )
            .await?;
            return Ok(());
        }
        page = page.saturating_add(1);
        offset = 0;
    }
    cursor_state.next_page = page;
    cursor_state.next_offset = offset;
    save_poll_cursor(
        repository,
        account_id,
        context,
        "mentions_mrs",
        &cursor_state,
    )
    .await
}

fn closing_issue_iid(description: &str) -> Option<i64> {
    let regex = regex::Regex::new(
        r"(?i)\b(?:close[sd]?|fix(?:es|ed)?|resolve[sd]?)\s+#([1-9][0-9]{0,18})\b",
    )
    .ok()?;
    regex.captures(description)?.get(1)?.as_str().parse().ok()
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GitLabPollCursor {
    updated_after: Option<String>,
    #[serde(default)]
    pending_latest: Option<String>,
    next_page: u32,
    #[serde(default)]
    next_offset: usize,
}

fn decode_poll_cursor(value: Option<String>, initial_updated_after: String) -> GitLabPollCursor {
    let Some(value) = value else {
        return GitLabPollCursor {
            updated_after: Some(initial_updated_after),
            pending_latest: None,
            next_page: 1,
            next_offset: 0,
        };
    };
    serde_json::from_str(&value).unwrap_or(GitLabPollCursor {
        updated_after: Some(value),
        pending_latest: None,
        next_page: 1,
        next_offset: 0,
    })
}

fn initial_poll_cursor(_connection: &GitLabConnection) -> String {
    (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339()
}

fn overlap_timestamp(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|timestamp| timestamp.checked_sub_signed(chrono::Duration::seconds(1)))
        .map(|timestamp| timestamp.to_rfc3339())
        .unwrap_or_else(|| value.to_owned())
}

fn timestamp_at_or_after(value: &str, lower_bound: Option<&str>) -> bool {
    let Some(lower_bound) = lower_bound else {
        return true;
    };
    let Ok(value) = chrono::DateTime::parse_from_rfc3339(value) else {
        return false;
    };
    let Ok(lower_bound) = chrono::DateTime::parse_from_rfc3339(lower_bound) else {
        return false;
    };
    value >= lower_bound
}

async fn poll_merge_requests(
    repository: &GitLabRepository,
    context: &WorkspaceGitLab,
    account_id: &str,
    event_category: &str,
    cursor_category: &str,
    scope: GitLabMergeRequestScope,
    state: GitLabMergeRequestState,
    reviewer_id: Option<u64>,
) -> ApiResult<()> {
    let cursor = repository
        .cursor(
            account_id,
            &context.canonical_path,
            &context.mapping.project_path,
            cursor_category,
        )
        .await
        .map_err(internal)?;
    let mut cursor_state = decode_poll_cursor(cursor, initial_poll_cursor(&context.connection));
    let mut page = cursor_state.next_page.max(1);
    for _ in 0..MAX_POLL_PAGES {
        let response = call_gitlab(
            context,
            account_id,
            GitLabOperation::ListMergeRequests {
                project_path: context.mapping.project_path.clone(),
                scope,
                state,
                reviewer_id,
                updated_after: cursor_state.updated_after.clone(),
                page,
            },
        )
        .await?;
        let Some(rows) = response.as_array() else {
            return Err(ApiError::service_unavailable(
                "GitLab returned an invalid merge request list",
            ));
        };
        for row in rows {
            let iid = row.get("iid").and_then(Value::as_u64).unwrap_or_default();
            let Some(entity_iid) = i64::try_from(iid).ok() else {
                continue;
            };
            let updated_at = row
                .get("updated_at")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if iid == 0 || updated_at.is_empty() {
                continue;
            }
            if cursor_state
                .pending_latest
                .as_deref()
                .is_none_or(|current| updated_at > current)
            {
                cursor_state.pending_latest = Some(updated_at.to_owned());
            }
            let event_key = format!(
                "mr:{}:{}:{iid}:{updated_at}",
                context.canonical_path, context.mapping.project_path
            );
            let event = InboxEvent {
                id: Uuid::new_v4().to_string(),
                workspace_path: context.canonical_path.clone(),
                project_path: context.mapping.project_path.clone(),
                category: event_category.to_owned(),
                categories: vec![event_category.to_owned()],
                coalesce_key: Some(format!(
                    "{}:{}:merge_request:{iid}",
                    context.canonical_path, context.mapping.project_path
                )),
                entity_kind: "merge_request".to_owned(),
                entity_iid,
                related_issue_iid: None,
                discussion_id: None,
                session_id: None,
                title: row
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("GitLab merge request update")
                    .chars()
                    .take(255)
                    .collect(),
                summary: summarize(
                    row.get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
                url: safe_web_url(
                    row.get("web_url").and_then(Value::as_str),
                    &context.connection.host,
                    &context.connection.path_prefix,
                ),
                occurred_at: updated_at.to_owned(),
                read_at: None,
            };
            repository
                .record_event(account_id, &event, &event_key)
                .await
                .map_err(internal)?;
        }
        if rows.len() < GITLAB_PAGE_SIZE {
            if let Some(latest) = cursor_state.pending_latest.take() {
                cursor_state.updated_after = Some(overlap_timestamp(&latest));
            }
            cursor_state.next_page = 1;
            cursor_state.next_offset = 0;
            let next_cursor = serde_json::to_string(&cursor_state).map_err(internal)?;
            repository
                .save_cursor(
                    account_id,
                    &context.canonical_path,
                    &context.mapping.project_path,
                    cursor_category,
                    &next_cursor,
                )
                .await
                .map_err(internal)?;
            return Ok(());
        }
        page = page.saturating_add(1);
    }
    cursor_state.next_page = page;
    let next_cursor = serde_json::to_string(&cursor_state).map_err(internal)?;
    repository
        .save_cursor(
            account_id,
            &context.canonical_path,
            &context.mapping.project_path,
            cursor_category,
            &next_cursor,
        )
        .await
        .map_err(internal)?;
    Ok(())
}

fn summarize(value: &str) -> String {
    let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    text.chars().take(320).collect()
}

fn safe_web_url(value: Option<&str>, host: &str, path_prefix: &str) -> Option<String> {
    let (_, authority_and_path) = value?.split_once("://")?;
    let authority = authority_and_path.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    let url = reqwest::Url::parse(value?).ok()?;
    let path_is_scoped = path_prefix.is_empty()
        || url.path() == path_prefix
        || url.path().starts_with(&format!("{path_prefix}/"));
    (url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && path_is_scoped)
        .then(|| url.to_string())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ProposalDraft {
    StartImplementation {
        #[serde(default)]
        session_id: Option<String>,
    },
    UpdateIssue {
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        labels: Option<Vec<String>>,
        #[serde(default)]
        state_event: Option<GitLabIssueStateEvent>,
    },
    Comment {
        #[serde(default)]
        session_id: Option<String>,
        body: String,
    },
    InlineReview {
        #[serde(default)]
        session_id: Option<String>,
        merge_request_iid: u64,
        comments: Vec<InlineReviewComment>,
    },
    DiscussionReply {
        #[serde(default)]
        session_id: Option<String>,
        merge_request_iid: u64,
        discussion_id: String,
        body: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InlineReviewComment {
    path: String,
    line: u64,
    side: oqto_protocol::gitlab::GitLabDiscussionSide,
    body: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateProposalRequest {
    workspace_path: String,
    issue_iid: u64,
    proposal: ProposalDraft,
}

async fn create_proposal(
    State(state): State<AppState>,
    user: CurrentUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<CreateProposalRequest>,
) -> ApiResult<Json<Proposal>> {
    protected_submission(&state, peer, &headers)?;
    if request.issue_iid == 0 {
        return Err(ApiError::bad_request("GitLab issue IID must be positive"));
    }
    let context = workspace_gitlab(&state, user.id(), &request.workspace_path).await?;
    let (kind, payload, session_id) = match request.proposal {
        ProposalDraft::StartImplementation { session_id } => {
            if session_id.as_ref().is_some_and(|id| {
                id.is_empty() || id.len() > 128 || id.chars().any(char::is_control)
            }) {
                return Err(ApiError::bad_request("Invalid session identifier"));
            }
            ("start_implementation", json!({}), session_id)
        }
        ProposalDraft::UpdateIssue {
            session_id,
            title,
            description,
            labels,
            state_event,
        } => {
            if title.is_none() && description.is_none() && labels.is_none() && state_event.is_none()
            {
                return Err(ApiError::bad_request("Issue proposal is empty"));
            }
            if title
                .as_ref()
                .is_some_and(|value| value.trim().is_empty() || value.len() > 255)
                || description
                    .as_ref()
                    .is_some_and(|value| value.len() > 64 * 1024)
                || labels.as_ref().is_some_and(|items| {
                    items.len() > 50 || items.iter().any(|item| item.len() > 128)
                })
            {
                return Err(ApiError::bad_request("Invalid issue proposal"));
            }
            if let Some(labels) = labels.as_ref() {
                let workflow_labels: Vec<&str> = labels
                    .iter()
                    .filter(|label| canonical_workflow_labels().contains(&label.as_str()))
                    .map(String::as_str)
                    .collect();
                if !workflow_labels.is_empty() {
                    require_existing_workflow_labels(&context, user.id(), &workflow_labels).await?;
                }
            }
            let current = call_gitlab(
                &context,
                user.id(),
                GitLabOperation::GetIssue {
                    project_path: context.mapping.project_path.clone(),
                    issue_iid: request.issue_iid,
                },
            )
            .await?;
            let original_labels = current
                .get("labels")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if labels.is_some()
                && original_labels
                    .iter()
                    .filter(|label| canonical_workflow_labels().contains(&label.as_str()))
                    .count()
                    > 1
            {
                return Err(ApiError::conflict(
                    "Resolve conflicting Oqto workflow labels before proposing a status change",
                ));
            }
            let original = json!({
                "title": current.get("title").and_then(Value::as_str).unwrap_or_default(),
                "description": current.get("description").and_then(Value::as_str).unwrap_or_default(),
                "labels": original_labels,
                "state": current.get("state").and_then(Value::as_str).unwrap_or("opened"),
                "updated_at": current.get("updated_at").and_then(Value::as_str).unwrap_or_default(),
            });
            validate_session_id(session_id.as_deref())?;
            (
                "update_issue",
                json!({
                    "title":title,
                    "description":description,
                    "labels":labels,
                    "state_event":state_event,
                    "original":original,
                }),
                session_id,
            )
        }
        ProposalDraft::Comment { session_id, body } => {
            if body.trim().is_empty() || body.len() > 32 * 1024 {
                return Err(ApiError::bad_request("Invalid comment proposal"));
            }
            validate_session_id(session_id.as_deref())?;
            ("comment", json!({"body":body}), session_id)
        }
        ProposalDraft::InlineReview {
            session_id,
            merge_request_iid,
            comments,
        } => {
            if merge_request_iid == 0
                || comments.is_empty()
                || comments.len() > 50
                || comments.iter().any(|comment| {
                    comment.path.is_empty()
                        || comment.path.len() > 1024
                        || comment.line == 0
                        || comment.body.trim().is_empty()
                        || comment.body.len() > 32 * 1024
                })
            {
                return Err(ApiError::bad_request("Invalid inline review proposal"));
            }
            validate_session_id(session_id.as_deref())?;
            let current = call_gitlab(
                &context,
                user.id(),
                GitLabOperation::GetMergeRequest {
                    project_path: context.mapping.project_path.clone(),
                    merge_request_iid,
                    include_changes: true,
                },
            )
            .await?;
            let diff_refs = current
                .get("diff_refs")
                .cloned()
                .ok_or_else(|| ApiError::conflict("GitLab diff references are not available"))?;
            (
                "inline_review",
                json!({"merge_request_iid":merge_request_iid,"comments":comments,"diff_refs":diff_refs}),
                session_id,
            )
        }
        ProposalDraft::DiscussionReply {
            session_id,
            merge_request_iid,
            discussion_id,
            body,
        } => {
            if merge_request_iid == 0
                || discussion_id.is_empty()
                || discussion_id.len() > 128
                || !discussion_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                || body.trim().is_empty()
                || body.len() > 32 * 1024
            {
                return Err(ApiError::bad_request(
                    "Invalid merge request discussion reply",
                ));
            }
            validate_session_id(session_id.as_deref())?;
            (
                "discussion_reply",
                json!({"merge_request_iid":merge_request_iid,"discussion_id":discussion_id,"body":body}),
                session_id,
            )
        }
    };
    let proposal = Proposal {
        id: format!("glprop_{}", Uuid::new_v4().simple()),
        workspace_path: context.canonical_path.clone(),
        project_path: context.mapping.project_path.clone(),
        issue_iid: request.issue_iid,
        session_id,
        kind: kind.to_owned(),
        payload,
        status: "pending".to_owned(),
        created_at: chrono::Utc::now().to_rfc3339(),
        session_stale: false,
    };
    if matches!(proposal.kind.as_str(), "inline_review" | "discussion_reply") {
        if !repository(&state)?
            .workflow_started(
                user.id(),
                &proposal.workspace_path,
                &proposal.project_path,
                proposal.issue_iid,
            )
            .await
            .map_err(internal)?
        {
            return Err(ApiError::forbidden(
                "Approve implementation before interacting with its merge request",
            ));
        }
        let requested_mr = proposal
            .payload
            .get("merge_request_iid")
            .and_then(Value::as_u64)
            .ok_or_else(|| ApiError::bad_request("Invalid merge request proposal"))?;
        let tracked_mr =
            tracked_merge_request(&state, user.id(), &context, proposal.issue_iid).await?;
        if tracked_mr.get("iid").and_then(Value::as_u64) != Some(requested_mr) {
            return Err(ApiError::bad_request(
                "This proposal must target the merge request linked to this issue",
            ));
        }
    }
    if let Some(session_id) = proposal.session_id.as_deref() {
        let session = state
            .sessions
            .get_session_for_user(user.id(), session_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| ApiError::bad_request("GitLab session was not found"))?;
        let session_workspace =
            canonical_workspace(&state, user.id(), &session.workspace_path).await?;
        if session_workspace != proposal.workspace_path {
            return Err(ApiError::bad_request(
                "GitLab session must belong to the mapped workspace",
            ));
        }
    }
    repository(&state)?
        .create_proposal(user.id(), &proposal)
        .await
        .map_err(internal)?;
    Ok(Json(proposal))
}

async fn list_proposals(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<WorkspaceQuery>,
) -> ApiResult<Json<Vec<Proposal>>> {
    require_real_auth(&state)?;
    let canonical = canonical_workspace(&state, user.id(), &query.workspace_path).await?;
    let mut proposals = repository(&state)?
        .proposals(user.id(), &canonical)
        .await
        .map_err(internal)?;
    for proposal in &mut proposals {
        let Some(session_id) = proposal.session_id.as_deref() else {
            continue;
        };
        let session = state
            .sessions
            .get_session_for_user(user.id(), session_id)
            .await
            .map_err(internal)?;
        proposal.session_stale = match session {
            Some(session) => {
                canonical_workspace(&state, user.id(), &session.workspace_path).await?
                    != proposal.workspace_path
            }
            None => true,
        };
    }
    Ok(Json(proposals))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RebindProposalRequest {
    session_id: String,
}

async fn rebind_proposal(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(proposal_id): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<RebindProposalRequest>,
) -> ApiResult<Json<Proposal>> {
    protected_submission(&state, peer, &headers)?;
    validate_session_id(Some(&request.session_id))?;
    let repository = repository(&state)?;
    let proposal = repository
        .proposal(user.id(), &proposal_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("GitLab proposal not found"))?;
    let lock_key = format!(
        "{}:{}:{}:{}",
        user.id(),
        proposal.workspace_path,
        proposal.project_path,
        proposal.issue_iid
    );
    let lock = ISSUE_WRITE_LOCKS
        .get_or_init(DashMap::new)
        .entry(lock_key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    let _write_guard = lock.lock().await;
    let proposal = repository
        .proposal(user.id(), &proposal_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("GitLab proposal not found"))?;
    if !matches!(proposal.status.as_str(), "pending" | "approved") {
        return Err(ApiError::conflict(
            "Only an undecided GitLab proposal can be rebound",
        ));
    }
    let session = state
        .sessions
        .get_session_for_user(user.id(), &request.session_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::bad_request("GitLab session was not found"))?;
    if canonical_workspace(&state, user.id(), &session.workspace_path).await?
        != proposal.workspace_path
    {
        return Err(ApiError::bad_request(
            "GitLab session must belong to the proposal workspace",
        ));
    }
    if !repository
        .rebind_proposal(user.id(), &proposal_id, &request.session_id)
        .await
        .map_err(internal)?
    {
        return Err(ApiError::conflict(
            "GitLab proposal changed while rebinding",
        ));
    }
    let mut proposal = repository
        .proposal(user.id(), &proposal_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("GitLab proposal not found"))?;
    proposal.session_stale = false;
    Ok(Json(proposal))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveProposalRequest {
    #[serde(default)]
    overwrite_stale: bool,
}

async fn approve_proposal(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(proposal_id): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<ApproveProposalRequest>,
) -> ApiResult<Json<Proposal>> {
    protected_submission(&state, peer, &headers)?;
    let repository = repository(&state)?;
    let proposal = repository
        .proposal(user.id(), &proposal_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("GitLab proposal not found"))?;
    let write_lock_key = format!(
        "{}:{}:{}:{}",
        user.id(),
        proposal.workspace_path,
        proposal.project_path,
        proposal.issue_iid
    );
    let write_lock = ISSUE_WRITE_LOCKS
        .get_or_init(DashMap::new)
        .entry(write_lock_key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    let _write_guard = write_lock.lock().await;
    let proposal = repository
        .proposal(user.id(), &proposal_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found("GitLab proposal not found"))?;
    let resuming_approved_review =
        proposal.kind == "inline_review" && proposal.status == "approved";
    if proposal.status != "pending" && !resuming_approved_review {
        return Err(ApiError::bad_request(
            "GitLab proposal is no longer pending",
        ));
    }
    if let Some(session_id) = proposal.session_id.as_deref() {
        let session = state
            .sessions
            .get_session_for_user(user.id(), session_id)
            .await
            .map_err(internal)?;
        let session_workspace = match session {
            Some(session) => {
                Some(canonical_workspace(&state, user.id(), &session.workspace_path).await?)
            }
            None => None,
        };
        if session_workspace.as_deref() != Some(proposal.workspace_path.as_str()) {
            return Err(ApiError::conflict(
                "The session bound to this proposal is no longer available in this workspace. Rebind it to a current session; the proposal will require approval again.",
            ));
        }
    }
    let context = workspace_gitlab(&state, user.id(), &proposal.workspace_path).await?;
    let start_labels = if proposal.kind == "start_implementation" {
        let issue = call_gitlab(
            &context,
            user.id(),
            GitLabOperation::GetIssue {
                project_path: proposal.project_path.clone(),
                issue_iid: proposal.issue_iid,
            },
        )
        .await?;
        if issue.get("state").and_then(Value::as_str) == Some("closed") {
            return Err(ApiError::conflict(
                "Reopen this GitLab issue with a separate approved proposal before starting implementation",
            ));
        }
        let mut labels: Vec<String> = issue
            .get("labels")
            .and_then(Value::as_array)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let existing_statuses = labels
            .iter()
            .filter(|label| canonical_workflow_labels().contains(&label.as_str()))
            .count();
        if existing_statuses > 1 {
            return Err(ApiError::conflict(
                "Resolve conflicting Oqto workflow labels before starting implementation",
            ));
        }
        require_existing_workflow_labels(&context, user.id(), &["oqto::in_progress"]).await?;
        labels.retain(|label| !canonical_workflow_labels().contains(&label.as_str()));
        labels.push("oqto::in_progress".to_owned());
        Some(labels)
    } else {
        None
    };
    if matches!(proposal.kind.as_str(), "inline_review" | "discussion_reply")
        && !repository
            .workflow_started(
                user.id(),
                &proposal.workspace_path,
                &proposal.project_path,
                proposal.issue_iid,
            )
            .await
            .map_err(internal)?
    {
        return Err(ApiError::forbidden(
            "Approve implementation before interacting with the linked merge request",
        ));
    }
    if matches!(proposal.kind.as_str(), "inline_review" | "discussion_reply") {
        let requested_mr = proposal
            .payload
            .get("merge_request_iid")
            .and_then(Value::as_u64)
            .ok_or_else(|| ApiError::bad_request("Invalid merge request proposal"))?;
        let tracked_mr =
            tracked_merge_request(&state, user.id(), &context, proposal.issue_iid).await?;
        if tracked_mr.get("iid").and_then(Value::as_u64) != Some(requested_mr) {
            return Err(ApiError::bad_request(
                "This proposal must target the merge request linked to this issue",
            ));
        }
    }
    let prepared_issue_update = if proposal.kind == "update_issue" {
        Some(prepare_issue_update(&context, user.id(), &proposal, request.overwrite_stale).await?)
    } else {
        None
    };
    if !resuming_approved_review
        && !repository
            .decide_proposal(user.id(), &proposal.id, "approved")
            .await
            .map_err(internal)?
    {
        return Err(ApiError::conflict(
            "GitLab proposal has already been decided",
        ));
    }
    let applied = match proposal.kind.as_str() {
        "start_implementation" => match start_labels {
            Some(labels) => match call_gitlab(
                &context,
                user.id(),
                GitLabOperation::UpdateIssue {
                    project_path: proposal.project_path.clone(),
                    issue_iid: proposal.issue_iid,
                    title: None,
                    description: None,
                    labels: Some(labels),
                    state_event: None,
                },
            )
            .await
            {
                Ok(_) => repository
                    .start_implementation(
                        user.id(),
                        &proposal.workspace_path,
                        &proposal.project_path,
                        proposal.issue_iid,
                        proposal.session_id.as_deref(),
                    )
                    .await
                    .map(|_| Value::Null)
                    .map_err(|_| {
                        ApiError::service_unavailable("Could not record implementation approval")
                    }),
                Err(error) => Err(error),
            },
            None => Err(ApiError::internal(
                "Implementation approval lost its status-label proposal",
            )),
        },
        "update_issue" => match prepared_issue_update {
            Some(Some(operation)) => call_gitlab(&context, user.id(), operation).await,
            Some(None) => Ok(json!({"unchanged": true})),
            None => Err(ApiError::internal(
                "Issue proposal lost its approved update",
            )),
        },
        "comment" => {
            let body = value_string(&proposal.payload, "body").unwrap_or_default();
            call_gitlab(
                &context,
                user.id(),
                GitLabOperation::CreateIssueNote {
                    project_path: proposal.project_path.clone(),
                    issue_iid: proposal.issue_iid,
                    body,
                },
            )
            .await
        }
        "inline_review" => submit_inline_review(&context, user.id(), &proposal).await,
        "discussion_reply" => submit_discussion_reply(&context, user.id(), &proposal).await,
        _ => Err(ApiError::bad_request("Unsupported GitLab proposal")),
    };
    let has_partial_review = if applied.is_err() && proposal.kind == "inline_review" {
        repository
            .proposal(user.id(), &proposal.id)
            .await
            .map_err(internal)?
            .and_then(|proposal| {
                proposal
                    .payload
                    .get("submitted_discussions")
                    .and_then(Value::as_array)
                    .map(|items| !items.is_empty())
            })
            .unwrap_or(false)
    } else {
        false
    };
    let stale_review_position = matches!(
        &applied,
        Err(ApiError::Conflict(message))
            if message.contains("diff changed")
                || message.contains("current diff")
                || message.contains("diff references")
                || message.contains("diff is unavailable")
    );
    let final_status = if applied.is_ok() {
        "applied"
    } else if has_partial_review && !stale_review_position {
        "approved"
    } else {
        "failed"
    };
    if final_status != "approved" {
        repository
            .decide_proposal(user.id(), &proposal.id, final_status)
            .await
            .map_err(internal)?;
    }
    let mut result = proposal;
    result.status = final_status.to_owned();
    if applied.is_err() {
        return Err(ApiError::service_unavailable(
            "GitLab proposal could not be applied",
        ));
    }
    Ok(Json(result))
}

async fn reject_proposal(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(proposal_id): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    protected_submission(&state, peer, &headers)?;
    let found = repository(&state)?
        .decide_proposal(user.id(), &proposal_id, "rejected")
        .await
        .map_err(internal)?;
    if !found {
        return Err(ApiError::not_found("Pending GitLab proposal not found"));
    }
    Ok(Json(json!({"rejected": true})))
}

fn value_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn display_label_set(labels: &[String]) -> String {
    if labels.is_empty() {
        "(none)".to_owned()
    } else {
        labels.join(", ")
    }
}

async fn prepare_issue_update(
    context: &WorkspaceGitLab,
    account_id: &str,
    proposal: &Proposal,
    overwrite_stale: bool,
) -> ApiResult<Option<GitLabOperation>> {
    let current = call_gitlab(
        context,
        account_id,
        GitLabOperation::GetIssue {
            project_path: proposal.project_path.clone(),
            issue_iid: proposal.issue_iid,
        },
    )
    .await?;
    let original = proposal.payload.get("original").ok_or_else(|| {
        ApiError::conflict("This proposal has no original ticket snapshot; create a new proposal.")
    })?;
    let confirmation_snapshot = proposal.payload.get("stale_snapshot");
    let mut changed_since_confirmation = confirmation_snapshot.is_none();
    let proposed_title = value_string(&proposal.payload, "title");
    let proposed_description = value_string(&proposal.payload, "description");
    let proposed_labels = proposal
        .payload
        .get("labels")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
    if let Some(labels) = proposed_labels.as_ref() {
        let workflow_labels: Vec<&str> = labels
            .iter()
            .filter(|label| canonical_workflow_labels().contains(&label.as_str()))
            .map(String::as_str)
            .collect();
        if !workflow_labels.is_empty() {
            require_existing_workflow_labels(context, account_id, &workflow_labels).await?;
        }
    }
    let state_event = match proposal.payload.get("state_event").and_then(Value::as_str) {
        Some("close") => Some(GitLabIssueStateEvent::Close),
        Some("reopen") => Some(GitLabIssueStateEvent::Reopen),
        _ => None,
    };

    let current_title = current
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let current_description = current
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let current_state = current
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("opened");
    let original_title = original
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let original_description = original
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let original_state = original
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("opened");
    let current_labels = current
        .get("labels")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let original_labels = original
        .get("labels")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| {
            ApiError::conflict(
                "This proposal has no original label snapshot; create a new proposal.",
            )
        })?;

    let status_labels = canonical_workflow_labels();
    let workflow_labels = |labels: &[String]| -> Vec<String> {
        labels
            .iter()
            .filter(|label| status_labels.contains(&label.as_str()))
            .cloned()
            .collect()
    };
    let original_workflow = workflow_labels(&original_labels);
    let current_workflow = workflow_labels(&current_labels);
    let proposed_workflow = proposed_labels
        .as_ref()
        .map(|labels| workflow_labels(labels));
    let workflow_status_changed = proposed_workflow
        .as_ref()
        .is_some_and(|proposed| proposed != &original_workflow);

    let mut stale_fields = Vec::new();
    if let Some(proposed) = proposed_title.as_deref()
        && current_title != original_title
        && current_title != proposed
    {
        if confirmation_snapshot
            .and_then(|snapshot| snapshot.get("title"))
            .and_then(Value::as_str)
            != Some(current_title)
        {
            changed_since_confirmation = true;
        }
        stale_fields.push(format!(
            "title: original='{}', current='{}', proposed='{}'",
            summarize(original_title),
            summarize(current_title),
            summarize(proposed)
        ));
    }
    if let Some(proposed) = proposed_description.as_deref()
        && current_description != original_description
        && current_description != proposed
    {
        if confirmation_snapshot
            .and_then(|snapshot| snapshot.get("description"))
            .and_then(Value::as_str)
            != Some(current_description)
        {
            changed_since_confirmation = true;
        }
        stale_fields.push(format!(
            "description: original='{}', current='{}', proposed='{}'",
            summarize(original_description),
            summarize(current_description),
            summarize(proposed)
        ));
    }

    if workflow_status_changed
        && current_workflow != original_workflow
        && Some(&current_workflow) != proposed_workflow.as_ref()
    {
        let snapshot_workflow = confirmation_snapshot
            .and_then(|snapshot| snapshot.get("labels"))
            .and_then(Value::as_array)
            .map(|items| {
                workflow_labels(
                    &items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                )
            });
        if snapshot_workflow.as_ref() != Some(&current_workflow) {
            changed_since_confirmation = true;
        }
        stale_fields.push(format!(
            "workflow status labels: original={}, current={}, proposed={}",
            display_label_set(&original_workflow),
            display_label_set(&current_workflow),
            display_label_set(proposed_workflow.as_ref().map_or(&[], Vec::as_slice))
        ));
    }

    let mut merged_labels = current_labels.clone();
    if let Some(proposed_labels) = proposed_labels.as_ref() {
        let all_labels: HashSet<&str> = original_labels
            .iter()
            .chain(proposed_labels)
            .map(String::as_str)
            .collect();
        for label in all_labels {
            let was_present = original_labels.iter().any(|value| value == label);
            let should_be_present = proposed_labels.iter().any(|value| value == label);
            if was_present == should_be_present {
                continue;
            }
            let is_present_now = current_labels.iter().any(|value| value == label);
            if is_present_now != was_present && is_present_now != should_be_present {
                let was_present_at_confirmation = confirmation_snapshot
                    .and_then(|snapshot| snapshot.get("labels"))
                    .and_then(Value::as_array)
                    .is_some_and(|items| items.iter().any(|value| value.as_str() == Some(label)));
                if was_present_at_confirmation != is_present_now {
                    changed_since_confirmation = true;
                }
                stale_fields.push(format!(
                    "label '{label}': original={}, current={}, proposed={}",
                    was_present, is_present_now, should_be_present
                ));
            }
            merged_labels.retain(|value| value != label);
            if should_be_present {
                merged_labels.push(label.to_owned());
            }
        }
        if workflow_status_changed {
            merged_labels.retain(|label| !status_labels.contains(&label.as_str()));
            merged_labels.extend(proposed_workflow.as_ref().into_iter().flatten().cloned());
        }
        merged_labels.sort();
        merged_labels.dedup();
    }

    let desired_state = match state_event {
        Some(GitLabIssueStateEvent::Close) => Some("closed"),
        Some(GitLabIssueStateEvent::Reopen) => Some("opened"),
        None => None,
    };
    if let Some(desired) = desired_state
        && current_state != original_state
        && current_state != desired
    {
        if confirmation_snapshot
            .and_then(|snapshot| snapshot.get("state"))
            .and_then(Value::as_str)
            != Some(current_state)
        {
            changed_since_confirmation = true;
        }
        stale_fields.push(format!(
            "state: original='{original_state}', current='{current_state}', proposed='{desired}'"
        ));
    }
    if !stale_fields.is_empty() && (!overwrite_stale || changed_since_confirmation) {
        let mut payload = proposal.payload.clone();
        payload["stale_snapshot"] = json!({
            "title": current_title,
            "description": current_description,
            "labels": current_labels,
            "state": current_state,
        });
        if !context
            .repository
            .update_pending_proposal_payload(account_id, &proposal.id, &payload)
            .await
            .map_err(internal)?
        {
            return Err(ApiError::conflict(
                "GitLab proposal changed while checking for stale fields; reload and review it again.",
            ));
        }
        return Err(ApiError::conflict(format!(
            "GitLab ticket changed since this proposal. Review the current values before overwriting: {}. Confirm overwrite to continue.",
            stale_fields.join("; ")
        )));
    }

    let title = proposed_title.filter(|value| value != current_title);
    let description = proposed_description.filter(|value| value != current_description);
    let labels = proposed_labels.map(|_| merged_labels);
    let state_event = desired_state
        .filter(|desired| *desired != current_state)
        .map(|desired| match desired {
            "closed" => GitLabIssueStateEvent::Close,
            _ => GitLabIssueStateEvent::Reopen,
        });
    if title.is_none() && description.is_none() && labels.is_none() && state_event.is_none() {
        return Ok(None);
    }
    Ok(Some(GitLabOperation::UpdateIssue {
        project_path: proposal.project_path.clone(),
        issue_iid: proposal.issue_iid,
        title,
        description,
        labels,
        state_event,
    }))
}

fn validate_session_id(session_id: Option<&str>) -> ApiResult<()> {
    if session_id
        .is_some_and(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
    {
        return Err(ApiError::bad_request("Invalid session identifier"));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftMergeRequestRequest {
    workspace_path: String,
    issue_iid: u64,
    remote_name: String,
    #[serde(default)]
    target_branch: Option<String>,
    title: String,
    description: String,
    #[serde(default)]
    confirm_redraft_ready: bool,
}

async fn create_or_update_draft(
    State(state): State<AppState>,
    user: CurrentUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<DraftMergeRequestRequest>,
) -> ApiResult<Json<Value>> {
    protected_submission(&state, peer, &headers)?;
    if request.issue_iid == 0
        || request.remote_name.is_empty()
        || request.title.trim().is_empty()
        || request.title.len() > 255
        || request.description.len() > 64 * 1024
        || request
            .target_branch
            .as_ref()
            .is_some_and(|branch| branch.len() > 255)
    {
        return Err(ApiError::bad_request("Invalid draft merge request"));
    }
    let context = workspace_gitlab(&state, user.id(), &request.workspace_path).await?;
    Ok(Json(
        create_draft_for_context(
            &state,
            user.id(),
            &context,
            request.issue_iid,
            &request.remote_name,
            request.target_branch.as_deref(),
            &request.title,
            &request.description,
            request.confirm_redraft_ready,
        )
        .await?,
    ))
}

async fn create_draft_for_context(
    state: &AppState,
    account_id: &str,
    context: &WorkspaceGitLab,
    issue_iid: u64,
    remote_name: &str,
    requested_target_branch: Option<&str>,
    title: &str,
    description: &str,
    confirm_redraft_ready: bool,
) -> ApiResult<Value> {
    if issue_iid == 0
        || remote_name.is_empty()
        || title.trim().is_empty()
        || title.len() > 255
        || description.len() > 64 * 1024
        || requested_target_branch.is_some_and(|branch| branch.len() > 255)
    {
        return Err(ApiError::bad_request("Invalid draft merge request"));
    }
    let lock_key = format!(
        "{account_id}:{}:{}:{issue_iid}",
        context.canonical_path, context.mapping.project_path
    );
    let lock = DRAFT_LOCKS
        .get_or_init(DashMap::new)
        .entry(lock_key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    let _draft_guard = lock.lock().await;
    let repository = &context.repository;
    if !repository
        .workflow_started(
            account_id,
            &context.canonical_path,
            &context.mapping.project_path,
            issue_iid,
        )
        .await
        .map_err(internal)?
    {
        return Err(ApiError::forbidden(
            "Approve implementation before creating a draft merge request",
        ));
    }
    let project = call_gitlab(
        context,
        account_id,
        GitLabOperation::GetProject {
            project_path: context.mapping.project_path.clone(),
        },
    )
    .await?;
    let default_branch = project
        .get("default_branch")
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty() && branch.len() <= 255)
        .ok_or_else(|| {
            ApiError::service_unavailable("GitLab project default branch is unavailable")
        })?
        .to_owned();
    let target_branch = requested_target_branch
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .unwrap_or(&default_branch)
        .to_owned();
    let update_target_branch = requested_target_branch
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned);
    if target_branch.starts_with('-') || target_branch.chars().any(char::is_control) {
        return Err(ApiError::bad_request("Invalid target branch"));
    }
    let mut matching_issues = Vec::new();
    let mut same_source_branch = Vec::new();
    for page in 1..=MAX_POLL_PAGES {
        let response = call_gitlab(
            context,
            account_id,
            GitLabOperation::ListMergeRequests {
                project_path: context.mapping.project_path.clone(),
                scope: GitLabMergeRequestScope::All,
                state: GitLabMergeRequestState::All,
                reviewer_id: None,
                updated_after: None,
                page,
            },
        )
        .await?;
        let Some(items) = response.as_array() else {
            return Err(ApiError::service_unavailable(
                "GitLab returned an invalid merge request list",
            ));
        };
        for item in items {
            if item
                .get("description")
                .and_then(Value::as_str)
                .is_some_and(|mr_description| has_closing_reference(mr_description, issue_iid))
            {
                matching_issues.push(item.clone());
            }
            if let Some(source) = item.get("source_branch").and_then(Value::as_str) {
                same_source_branch.push((source.to_owned(), item.clone()));
            }
        }
        if items.len() < GITLAB_PAGE_SIZE {
            break;
        }
        if page == MAX_POLL_PAGES {
            return Err(ApiError::service_unavailable(
                "GitLab has more merge requests than Oqto can safely scan for an existing issue link",
            ));
        }
    }
    if matching_issues.len() > 1 {
        return Err(ApiError::conflict(
            "More than one merge request links to this issue; resolve duplicates before updating the draft",
        ));
    }
    let existing = matching_issues.into_iter().next();
    if existing
        .as_ref()
        .is_some_and(|item| item.get("state").and_then(Value::as_str) != Some("opened"))
    {
        return Err(ApiError::conflict(
            "This issue's linked merge request is closed. Oqto will not create a second merge request for the same issue.",
        ));
    }
    if existing
        .as_ref()
        .is_some_and(|item| item.get("draft").and_then(Value::as_bool) == Some(false))
        && !confirm_redraft_ready
    {
        return Err(ApiError::conflict(
            "This issue already has a ready merge request. Explicitly confirm moving it back to draft before updating it.",
        ));
    }

    let source_branch = crate::api::ws_multiplexed::git::push_current_branch(
        state,
        account_id,
        &context.canonical_path,
        remote_name,
        &context.connection.host,
        &context.connection.path_prefix,
        &context.mapping.project_path,
        &default_branch,
    )
    .await
    .map_err(ApiError::bad_request)?;
    let linked_description = if has_explicit_closes_reference(description, issue_iid) {
        description.to_owned()
    } else {
        format!("{}\n\nCloses #{}", description, issue_iid)
    };
    if linked_description.len() > 64 * 1024 {
        return Err(ApiError::bad_request(
            "Draft merge request description exceeds GitLab's size limit after adding the closing reference",
        ));
    }
    let result = if let Some(existing) = existing {
        let existing_branch = existing
            .get("source_branch")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if existing_branch != source_branch {
            return Err(ApiError::conflict(
                "This issue's linked merge request uses a different source branch. Check out that branch manually before updating it; Oqto never switches branches.",
            ));
        }
        let iid = existing.get("iid").and_then(Value::as_u64).ok_or_else(|| {
            ApiError::service_unavailable("GitLab returned an invalid merge request")
        })?;
        let latest = call_gitlab(
            context,
            account_id,
            GitLabOperation::GetMergeRequest {
                project_path: context.mapping.project_path.clone(),
                merge_request_iid: iid,
                include_changes: false,
            },
        )
        .await?;
        if latest.get("state").and_then(Value::as_str) != Some("opened") {
            return Err(ApiError::conflict(
                "The linked merge request was closed before the update could be applied",
            ));
        }
        if latest.get("draft").and_then(Value::as_bool) == Some(false) && !confirm_redraft_ready {
            return Err(ApiError::conflict(
                "The linked merge request became ready. Confirm the ready-to-draft transition before updating it.",
            ));
        }
        let updated = call_gitlab(
            context,
            account_id,
            GitLabOperation::UpdateMergeRequest {
                project_path: context.mapping.project_path.clone(),
                merge_request_iid: iid,
                title: title.to_owned(),
                description: linked_description,
                draft: true,
                target_branch: update_target_branch,
            },
        )
        .await?;
        (iid, updated)
    } else {
        if same_source_branch
            .iter()
            .any(|(branch, _)| branch == &source_branch)
        {
            return Err(ApiError::conflict(
                "A merge request already uses the current source branch but is not linked to this issue",
            ));
        }
        let created = call_gitlab(
            context,
            account_id,
            GitLabOperation::CreateMergeRequest {
                project_path: context.mapping.project_path.clone(),
                source_branch,
                target_branch,
                title: title.to_owned(),
                description: linked_description,
                draft: true,
            },
        )
        .await?;
        let iid = created.get("iid").and_then(Value::as_u64).ok_or_else(|| {
            ApiError::service_unavailable("GitLab returned an invalid merge request")
        })?;
        (iid, created)
    };
    repository
        .save_merge_request(
            account_id,
            &context.canonical_path,
            &context.mapping.project_path,
            issue_iid,
            result.0,
        )
        .await
        .map_err(internal)?;
    Ok(result.1)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadyRequest {
    workspace_path: String,
    issue_iid: u64,
    confirmed: bool,
}

async fn mark_merge_request_ready(
    State(state): State<AppState>,
    user: CurrentUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<ReadyRequest>,
) -> ApiResult<Json<Value>> {
    protected_submission(&state, peer, &headers)?;
    if !request.confirmed {
        return Err(ApiError::bad_request("Explicit confirmation is required"));
    }
    let context = workspace_gitlab(&state, user.id(), &request.workspace_path).await?;
    let lock_key = format!(
        "{}:{}:{}:{}",
        user.id(),
        context.canonical_path,
        context.mapping.project_path,
        request.issue_iid
    );
    let lock = DRAFT_LOCKS
        .get_or_init(DashMap::new)
        .entry(lock_key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    let _draft_guard = lock.lock().await;
    let merge_request =
        tracked_merge_request(&state, user.id(), &context, request.issue_iid).await?;
    if merge_request.get("state").and_then(Value::as_str) != Some("opened") {
        return Err(ApiError::conflict(
            "The linked merge request is closed and cannot be marked ready",
        ));
    }
    if merge_request.get("draft").and_then(Value::as_bool) == Some(false) {
        return Err(ApiError::conflict(
            "The linked merge request is already ready for review",
        ));
    }
    let title = merge_request
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let description = merge_request
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let iid = merge_request
        .get("iid")
        .and_then(Value::as_u64)
        .ok_or_else(|| ApiError::not_found("Draft merge request not found"))?;
    call_gitlab(
        &context,
        user.id(),
        GitLabOperation::UpdateMergeRequest {
            project_path: context.mapping.project_path.clone(),
            merge_request_iid: iid,
            title,
            description,
            draft: false,
            target_branch: None,
        },
    )
    .await
    .map(Json)
}

async fn tracked_merge_request(
    state: &AppState,
    account_id: &str,
    context: &WorkspaceGitLab,
    issue_iid: u64,
) -> ApiResult<Value> {
    let repository = repository(state)?;
    let iid: Option<Option<i64>> = sqlx::query_scalar("SELECT merge_request_iid FROM gitlab_workflows WHERE account_id=? AND workspace_path=? AND project_path=? AND issue_iid=? AND implementation_started=1")
        .bind(account_id).bind(&context.canonical_path).bind(&context.mapping.project_path).bind(i64::try_from(issue_iid).map_err(|_| ApiError::bad_request("Invalid issue IID"))?)
        .fetch_optional(repository.pool())
        .await.map_err(internal)?;
    let iid = iid
        .flatten()
        .ok_or_else(|| ApiError::not_found("Draft merge request not found"))?;
    call_gitlab(
        context,
        account_id,
        GitLabOperation::GetMergeRequest {
            project_path: context.mapping.project_path.clone(),
            merge_request_iid: u64::try_from(iid)
                .map_err(|_| ApiError::internal("Invalid stored merge request IID"))?,
            include_changes: false,
        },
    )
    .await
}

fn has_closing_reference(description: &str, issue_iid: u64) -> bool {
    let Ok(expected) = i64::try_from(issue_iid) else {
        return false;
    };
    let Ok(regex) = regex::Regex::new(
        r"(?i)\b(?:close[sd]?|fix(?:es|ed)?|resolve[sd]?)\s+#([1-9][0-9]{0,18})\b",
    ) else {
        return false;
    };
    regex
        .captures_iter(description)
        .filter_map(|capture| capture.get(1)?.as_str().parse::<i64>().ok())
        .any(|issue| issue == expected)
}

fn has_explicit_closes_reference(description: &str, issue_iid: u64) -> bool {
    let expected = format!("Closes #{issue_iid}");
    description
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case(&expected))
}

async fn submit_inline_review(
    context: &WorkspaceGitLab,
    account_id: &str,
    proposal: &Proposal,
) -> ApiResult<Value> {
    let mr_iid = proposal
        .payload
        .get("merge_request_iid")
        .and_then(Value::as_u64)
        .ok_or_else(|| ApiError::bad_request("Invalid review proposal"))?;
    let comments = proposal
        .payload
        .get("comments")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("Invalid review proposal"))?;
    let expected_refs = proposal
        .payload
        .get("diff_refs")
        .ok_or_else(|| ApiError::bad_request("Review proposal has no diff-version binding"))?;
    let current = call_gitlab(
        context,
        account_id,
        GitLabOperation::GetMergeRequest {
            project_path: proposal.project_path.clone(),
            merge_request_iid: mr_iid,
            include_changes: true,
        },
    )
    .await?;
    let current_refs = current.get("diff_refs").ok_or_else(|| {
        ApiError::conflict("GitLab diff references are no longer available; refresh the review.")
    })?;
    if !same_diff_refs(expected_refs, current_refs) {
        return Err(ApiError::conflict(
            "The merge request diff changed after this review was proposed. Refresh the diff and create a new review proposal.",
        ));
    }
    let sha = |key| {
        current_refs
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                ApiError::conflict("GitLab diff references are incomplete; refresh the review.")
            })
    };
    let changes = current
        .get("changes")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::conflict("GitLab diff is unavailable; refresh the review."))?;

    let mut validated = Vec::with_capacity(comments.len());
    for (index, comment) in comments.iter().enumerate() {
        let path = comment
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let line = comment
            .get("line")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let body = comment
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let side = match comment.get("side").and_then(Value::as_str) {
            Some("old") => oqto_protocol::gitlab::GitLabDiscussionSide::Old,
            Some("new") => oqto_protocol::gitlab::GitLabDiscussionSide::New,
            _ => {
                return Err(ApiError::bad_request(
                    "Review comment has an invalid diff side",
                ));
            }
        };
        let change = changes
            .iter()
            .find(|change| {
                let expected_path = match side {
                    oqto_protocol::gitlab::GitLabDiscussionSide::Old => "old_path",
                    oqto_protocol::gitlab::GitLabDiscussionSide::New => "new_path",
                };
                change.get(expected_path).and_then(Value::as_str) == Some(path)
            })
            .ok_or_else(|| {
                ApiError::conflict(
                    "Review path is not present in the current diff; refresh the review.",
                )
            })?;
        let old_path = change
            .get("old_path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let new_path = change
            .get("new_path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let diff = change
            .get("diff")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !is_actual_diff_line(diff, side, line) {
            return Err(ApiError::conflict(
                "Review line is not present on the selected side of the current diff; refresh the review.",
            ));
        }
        let position = GitLabDiscussionPosition {
            base_sha: sha("base_sha")?,
            start_sha: sha("start_sha")?,
            head_sha: sha("head_sha")?,
            old_path: old_path.to_owned(),
            new_path: new_path.to_owned(),
            old_line: (side == oqto_protocol::gitlab::GitLabDiscussionSide::Old).then_some(line),
            new_line: (side == oqto_protocol::gitlab::GitLabDiscussionSide::New).then_some(line),
        };
        validated.push((index, body.to_owned(), position));
    }

    let mut submitted: HashSet<usize> = proposal
        .payload
        .get("submitted_discussions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("index").and_then(Value::as_u64))
        .filter_map(|index| usize::try_from(index).ok())
        .collect();
    let mut payload = proposal.payload.clone();
    let mut accepted: Vec<Value> = payload
        .get("submitted_discussions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (index, body, position) in validated {
        if submitted.contains(&index) {
            continue;
        }
        let response = call_gitlab(
            context,
            account_id,
            GitLabOperation::CreateMergeRequestDiscussion {
                project_path: proposal.project_path.clone(),
                merge_request_iid: mr_iid,
                body,
                position,
            },
        )
        .await?;
        let discussion_id = response
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 128)
            .ok_or_else(|| {
                ApiError::service_unavailable("GitLab did not return the submitted discussion ID")
            })?;
        accepted.push(json!({"index":index,"discussion_id":discussion_id}));
        payload["submitted_discussions"] = Value::Array(accepted.clone());
        if !context
            .repository
            .update_proposal_payload(account_id, &proposal.id, &payload)
            .await
            .map_err(internal)?
        {
            return Err(ApiError::service_unavailable(
                "GitLab accepted a review comment, but Oqto could not record its discussion ID; do not retry until you inspect the merge request.",
            ));
        }
        submitted.insert(index);
    }
    Ok(json!({"submitted": submitted.len(), "discussion_ids": accepted}))
}

fn same_diff_refs(expected: &Value, current: &Value) -> bool {
    ["base_sha", "start_sha", "head_sha"].iter().all(|key| {
        expected.get(key).and_then(Value::as_str) == current.get(key).and_then(Value::as_str)
    })
}

fn is_actual_diff_line(
    diff: &str,
    side: oqto_protocol::gitlab::GitLabDiscussionSide,
    requested_line: u64,
) -> bool {
    let (mut old_line, mut new_line) = (0_u64, 0_u64);
    for line in diff.lines() {
        if line.starts_with("@@") {
            let mut ranges = line
                .split_whitespace()
                .filter(|part| part.starts_with('-') || part.starts_with('+'));
            let Some(old_range) = ranges.next() else {
                continue;
            };
            let Some(new_range) = ranges.next() else {
                continue;
            };
            old_line = parse_hunk_start(old_range);
            new_line = parse_hunk_start(new_range);
        } else if line.starts_with("+++") || line.starts_with("---") || line.starts_with('\\') {
            continue;
        } else if let Some(content) = line.strip_prefix('+') {
            let _ = content;
            if side == oqto_protocol::gitlab::GitLabDiscussionSide::New
                && new_line == requested_line
            {
                return true;
            }
            new_line = new_line.saturating_add(1);
        } else if let Some(content) = line.strip_prefix('-') {
            let _ = content;
            if side == oqto_protocol::gitlab::GitLabDiscussionSide::Old
                && old_line == requested_line
            {
                return true;
            }
            old_line = old_line.saturating_add(1);
        } else if line.starts_with(' ') {
            if (side == oqto_protocol::gitlab::GitLabDiscussionSide::Old
                && old_line == requested_line)
                || (side == oqto_protocol::gitlab::GitLabDiscussionSide::New
                    && new_line == requested_line)
            {
                return true;
            }
            old_line = old_line.saturating_add(1);
            new_line = new_line.saturating_add(1);
        }
    }
    false
}

fn parse_hunk_start(range: &str) -> u64 {
    range
        .trim_start_matches(['-', '+'])
        .split(',')
        .next()
        .and_then(|number| number.parse().ok())
        .unwrap_or_default()
}

async fn submit_discussion_reply(
    context: &WorkspaceGitLab,
    account_id: &str,
    proposal: &Proposal,
) -> ApiResult<Value> {
    let merge_request_iid = proposal
        .payload
        .get("merge_request_iid")
        .and_then(Value::as_u64)
        .ok_or_else(|| ApiError::bad_request("Invalid discussion-reply proposal"))?;
    let discussion_id = proposal
        .payload
        .get("discussion_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .ok_or_else(|| ApiError::bad_request("Invalid discussion-reply proposal"))?;
    let body = value_string(&proposal.payload, "body")
        .ok_or_else(|| ApiError::bad_request("Invalid discussion-reply body"))?;
    call_gitlab(
        context,
        account_id,
        GitLabOperation::CreateMergeRequestDiscussionReply {
            project_path: proposal.project_path.clone(),
            merge_request_iid,
            discussion_id: discussion_id.to_owned(),
            body,
        },
    )
    .await
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %error, "GitLab integration internal error");
    ApiError::internal("GitLab integration unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_review_validation_allows_real_lines_on_the_selected_side() {
        let diff = "@@ -4,2 +4,3 @@\n unchanged\n-old\n+new one\n+new two\n";
        use oqto_protocol::gitlab::GitLabDiscussionSide::{New, Old};
        assert!(is_actual_diff_line(diff, New, 4));
        assert!(is_actual_diff_line(diff, New, 5));
        assert!(is_actual_diff_line(diff, New, 6));
        assert!(is_actual_diff_line(diff, Old, 4));
        assert!(is_actual_diff_line(diff, Old, 5));
        assert!(!is_actual_diff_line(diff, Old, 6));
        assert!(!is_actual_diff_line(diff, New, 7));
    }

    #[test]
    fn inbox_summary_is_compact_and_urls_must_match_configured_tls_host() {
        assert_eq!(summarize("  one\n  two  "), "one two");
        assert_eq!(
            safe_web_url(
                Some("https://gitlab.example/group/project/-/issues/1"),
                "gitlab.example",
                ""
            )
            .as_deref(),
            Some("https://gitlab.example/group/project/-/issues/1")
        );
        assert_eq!(
            safe_web_url(
                Some("https://gitlab.example/gitlab/group/project/-/issues/1"),
                "gitlab.example",
                "/gitlab"
            )
            .as_deref(),
            Some("https://gitlab.example/gitlab/group/project/-/issues/1")
        );
        assert!(safe_web_url(Some("https://evil.example/"), "gitlab.example", "").is_none());
        assert!(safe_web_url(Some("http://gitlab.example/"), "gitlab.example", "").is_none());
        assert!(safe_web_url(Some("https://gitlab.example.evil/"), "gitlab.example", "").is_none());
    }

    #[test]
    fn poll_interval_is_bounded_and_invalid_explicit_values_fail() {
        assert_eq!(parse_poll_interval(None).unwrap(), 60);
        assert_eq!(parse_poll_interval(Some("60")).unwrap(), 60);
        assert_eq!(parse_poll_interval(Some("3600")).unwrap(), 3600);
        for invalid in ["", "abc", "59", "3601", "-1", "60.5"] {
            assert!(parse_poll_interval(Some(invalid)).is_err(), "{invalid}");
        }
    }

    #[test]
    fn issue_status_and_closing_references_are_conservative() {
        assert_eq!(issue_status("opened", &[]), ("open".into(), false));
        assert_eq!(
            issue_status("opened", &["oqto::ready".into()]),
            ("ready".into(), false)
        );
        assert_eq!(
            issue_status("opened", &["oqto::ready".into(), "oqto::blocked".into()]),
            ("open".into(), true)
        );
        assert_eq!(
            issue_status("closed", &["oqto::ready".into()]),
            ("closed".into(), false)
        );
        assert!(has_closing_reference("Fixes #12", 12));
        assert!(has_closing_reference("Closes #12 and resolves #42", 42));
        assert!(!has_explicit_closes_reference("Fixes #12", 12));
        assert!(has_explicit_closes_reference("Closes #12", 12));
    }

    #[test]
    fn mentions_are_username_scoped_and_cursor_dates_are_parsed() {
        assert!(contains_gitlab_mention(
            "Please review @Alice's change",
            "alice"
        ));
        assert!(!contains_gitlab_mention(
            "@alice2 is a different user",
            "alice"
        ));
        assert!(timestamp_at_or_after(
            "2026-11-05T10:00:00Z",
            Some("2026-11-05T09:00:00Z")
        ));
        assert!(!timestamp_at_or_after(
            "2026-11-05T08:00:00Z",
            Some("2026-11-05T09:00:00Z")
        ));
    }
}
