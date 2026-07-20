use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::{Notify, RwLock, broadcast, mpsc};
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
};
use uuid::Uuid;

use crate::{
    auth::{AuthState, BootstrapRequest, BootstrapResponse, TicketResponse},
    config::Config,
    git::GitService,
    integration::IntegrationManager,
    model::{
        ActionContext, AgentIntegration, AgentState, CreateProjectRequest, CreateSessionRequest,
        GitStatus, HistoryPage, HistorySearchResult, Project, ReportActionStartInput,
        ScreenSnapshot, Session,
    },
    persistence::Store,
    protocol::{ClientMessage, PROTOCOL_VERSION, ServerMessage, decode_client, encode},
    session::{DaemonEvent, SessionManager},
};

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<AuthState>,
    pub sessions: SessionManager,
    pub integrations: IntegrationManager,
    pub git: GitService,
    pub store: Store,
    pub config: Config,
    pub input_leases: Arc<RwLock<HashMap<Uuid, Uuid>>>,
    pub resize_leases: Arc<RwLock<HashMap<Uuid, Uuid>>>,
    pub lease_events: broadcast::Sender<LeaseEvent>,
    pub control_credential: Arc<str>,
}

#[derive(Debug, Clone, Copy)]
pub enum LeaseKind {
    Input,
    Resize,
}

#[derive(Debug, Clone, Copy)]
pub struct LeaseEvent {
    pub kind: LeaseKind,
    pub session_id: Uuid,
    pub owner: Option<Uuid>,
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/bootstrap", post(bootstrap))
        .route("/control/open", post(issue_open_url))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}/snapshot", get(session_snapshot))
        .route("/sessions/{id}/history", get(session_history))
        .route("/sessions/{id}/history/search", get(search_session_history))
        .route("/sessions/{id}/history/export", get(export_session_history))
        .route("/sessions/{id}/actions", get(session_actions))
        .route("/sessions/{id}", delete(terminate_session))
        .route("/sessions/{id}/record", delete(delete_session_record))
        .route("/environment", get(environment))
        .route("/actions/current", get(current_actions))
        .route("/sessions/{id}/git-status", get(session_git_status))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/{id}", delete(delete_project))
        .route("/ws-ticket", post(websocket_ticket))
        .route("/ws", get(websocket_upgrade))
        .route("/integrations/{id}/hook", post(agent_hook))
        .route("/integrations/{id}/mcp", post(mcp_request))
        .route(
            "/integrations/{id}/runtime/{provider}/{invocation_id}",
            post(dynamic_agent_start),
        )
        .route(
            "/integrations/{id}/hook/{provider}/{invocation_id}",
            post(dynamic_agent_hook),
        )
        .route(
            "/integrations/{id}/mcp/{provider}/{invocation_id}",
            post(dynamic_mcp_request),
        )
        .layer(DefaultBodyLimit::max(1024 * 1024));

    let fallback = ServeDir::new(&state.config.web_dir)
        .not_found_service(ServeFile::new(state.config.web_dir.join("index.html")));
    Router::new()
        .nest("/api", api)
        .fallback_service(fallback)
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws:; frame-ancestors 'none'; base-uri 'none'",
            ),
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            validate_request_origin,
        ))
        .with_state(state)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenResponse {
    pub url: String,
}

async fn issue_open_url(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<OpenResponse>, ApiError> {
    let supplied = bearer(&headers)?;
    let valid = supplied
        .as_bytes()
        .ct_eq(state.control_credential.as_bytes());
    if !bool::from(valid) {
        return Err(ApiError::unauthorized("invalid local control credential"));
    }
    let bootstrap = state.auth.issue_bootstrap();
    Ok(Json(OpenResponse {
        url: format!("{}/#bootstrap={bootstrap}", state.config.browser_origin),
    }))
}

async fn validate_request_origin(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let expected_host = state.config.listen.to_string();
    let host_valid = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| host == expected_host);
    if !host_valid {
        return ApiError::forbidden("invalid Host header").into_response();
    }
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let origin_valid = origin
            .to_str()
            .is_ok_and(|origin| origin == state.config.browser_origin);
        if !origin_valid {
            return ApiError::forbidden("invalid Origin header").into_response();
        }
    }
    next.run(request).await
}

async fn bootstrap(
    State(state): State<AppState>,
    Json(request): Json<BootstrapRequest>,
) -> Result<Json<BootstrapResponse>, ApiError> {
    let credential = state.auth.exchange_bootstrap(&request).ok_or_else(|| {
        ApiError::unauthorized("bootstrap token or browser capabilities rejected")
    })?;
    Ok(Json(BootstrapResponse { credential }))
}

async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Session>>, ApiError> {
    authorize(&state, &headers)?;
    Ok(Json(state.sessions.list().map_err(ApiError::internal)?))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentResponse {
    home: String,
    shell: String,
}

async fn environment(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<EnvironmentResponse>, ApiError> {
    authorize(&state, &headers)?;
    Ok(Json(EnvironmentResponse {
        home: std::env::var("HOME").unwrap_or_else(|_| "/".into()),
        shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into()),
    }))
}

async fn current_actions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ActionContext>>, ApiError> {
    authorize(&state, &headers)?;
    let mut actions = Vec::new();
    for session in state.sessions.list().map_err(ApiError::internal)? {
        if let Some(action) = state
            .store
            .current_action(session.id)
            .map_err(ApiError::internal)?
        {
            actions.push(action);
        }
    }
    Ok(Json(actions))
}

async fn session_git_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<GitStatus>, ApiError> {
    authorize(&state, &headers)?;
    let action = state
        .store
        .current_action(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("session has no action context"))?;
    let root = action
        .git_working_tree_root
        .ok_or_else(|| ApiError::not_found("working directory is not managed by Git"))?;
    Ok(Json(
        state
            .git
            .status(std::path::Path::new(&root))
            .await
            .map_err(ApiError::bad_request)?,
    ))
}

async fn session_actions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ActionContext>>, ApiError> {
    authorize(&state, &headers)?;
    Ok(Json(
        state
            .store
            .list_actions(id, 100)
            .map_err(ApiError::internal)?,
    ))
}

async fn list_projects(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Project>>, ApiError> {
    authorize(&state, &headers)?;
    Ok(Json(
        state.store.list_projects().map_err(ApiError::internal)?,
    ))
}

async fn create_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    authorize(&state, &headers)?;
    let name = request.name.trim();
    if name.is_empty() || name.len() > 120 || request.tags.len() > 32 {
        return Err(ApiError::bad_request("invalid project name or tags"));
    }
    let root = std::fs::canonicalize(&request.root_path).map_err(ApiError::bad_request)?;
    if !root.is_dir() {
        return Err(ApiError::bad_request("project root must be a directory"));
    }
    let now = chrono::Utc::now();
    let project = Project {
        id: Uuid::new_v4(),
        name: name.to_owned(),
        root_path: root.to_string_lossy().into_owned(),
        color: request.color,
        tags: request.tags,
        repository_url: request.repository_url,
        created_at: now,
        updated_at: now,
    };
    state
        .store
        .upsert_project(&project)
        .map_err(ApiError::internal)?;
    Ok((StatusCode::CREATED, Json(project)))
}

async fn delete_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &headers)?;
    if state.store.delete_project(id).map_err(ApiError::internal)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("project not found"))
    }
}

async fn agent_hook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = bearer(&headers)?;
    if state.integrations.authorize_hook(id, token).is_none() {
        return Err(ApiError::unauthorized("invalid session hook credential"));
    }
    let event = payload
        .get("hook_event_name")
        .and_then(serde_json::Value::as_str);
    let agent_state = match event {
        Some("SessionStart") => AgentState::Initializing,
        Some("UserPromptSubmit") => {
            state
                .sessions
                .clear_current_action(id)
                .map_err(ApiError::internal)?;
            AgentState::Working
        }
        Some("PermissionRequest") => AgentState::WaitingApproval,
        Some("Stop") => AgentState::AwaitingUser,
        _ => {
            let _ = state
                .sessions
                .update_agent_state(id, AgentState::IntegrationError);
            return Err(ApiError::bad_request("unsupported or malformed hook event"));
        }
    };
    state
        .sessions
        .update_agent_state(id, agent_state)
        .map_err(ApiError::bad_request)?;
    Ok(Json(serde_json::json!({})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DynamicAgentStart {
    pid: u32,
    integration_supported: bool,
}

async fn dynamic_agent_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, provider, invocation_id)): Path<(Uuid, AgentIntegration, Uuid)>,
    Json(request): Json<DynamicAgentStart>,
) -> Result<StatusCode, ApiError> {
    let token = bearer(&headers)?;
    if !state.integrations.authorize_terminal_hook(id, token) {
        return Err(ApiError::unauthorized(
            "invalid terminal integration credential",
        ));
    }
    state
        .sessions
        .begin_dynamic_agent(
            id,
            invocation_id,
            provider,
            request.pid,
            request.integration_supported,
        )
        .map_err(ApiError::bad_request)?;
    let sessions = state.sessions.clone();
    tokio::spawn(async move {
        sessions.monitor_dynamic_agent(id, invocation_id).await;
    });
    Ok(StatusCode::NO_CONTENT)
}

async fn dynamic_agent_hook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, provider, invocation_id)): Path<(Uuid, AgentIntegration, Uuid)>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = bearer(&headers)?;
    if !state.integrations.authorize_terminal_hook(id, token) {
        return Err(ApiError::unauthorized("invalid terminal hook credential"));
    }
    let event = payload
        .get("hook_event_name")
        .and_then(serde_json::Value::as_str);
    let agent_state = match event {
        Some("SessionStart") => AgentState::Initializing,
        Some("UserPromptSubmit") => {
            state
                .sessions
                .clear_current_action(id)
                .map_err(ApiError::internal)?;
            AgentState::Working
        }
        Some("PermissionRequest") => AgentState::WaitingApproval,
        Some("Stop") => AgentState::AwaitingUser,
        _ => {
            let _ = state.sessions.update_dynamic_agent_state(
                id,
                invocation_id,
                provider,
                AgentState::IntegrationError,
            );
            return Err(ApiError::bad_request("unsupported or malformed hook event"));
        }
    };
    state
        .sessions
        .update_dynamic_agent_state(id, invocation_id, provider, agent_state)
        .map_err(ApiError::bad_request)?;
    Ok(Json(serde_json::json!({})))
}

async fn mcp_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    let token = bearer(&headers)?;
    if state.integrations.authorize_mcp(id, token).is_none() {
        return Err(ApiError::unauthorized("invalid session MCP credential"));
    }
    mcp_response(state, id, request).await
}

async fn dynamic_mcp_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, provider, invocation_id)): Path<(Uuid, AgentIntegration, Uuid)>,
    Json(request): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    let token = bearer(&headers)?;
    if !state.integrations.authorize_terminal_mcp(id, token) {
        return Err(ApiError::unauthorized("invalid terminal MCP credential"));
    }
    if !state
        .sessions
        .is_dynamic_agent_current(id, invocation_id, provider)
    {
        return Err(ApiError::bad_request(
            "agent invocation is no longer active",
        ));
    }
    mcp_response(state, id, request).await
}

async fn mcp_response(
    state: AppState,
    id: Uuid,
    request: serde_json::Value,
) -> Result<Response, ApiError> {
    if request.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Ok(mcp_error(
            request.get("id").cloned(),
            -32600,
            "Invalid Request",
        ));
    }
    let id_value = request.get("id").cloned();
    let Some(method) = request.get("method").and_then(serde_json::Value::as_str) else {
        return Ok(mcp_error(id_value, -32600, "Invalid Request"));
    };
    if id_value.is_none() {
        return Ok(StatusCode::ACCEPTED.into_response());
    }
    let result = match method {
        "initialize" => {
            let protocol_version = request
                .pointer("/params/protocolVersion")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("2025-06-18");
            serde_json::json!({
                "protocolVersion": protocol_version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "Panestra", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "新しいユーザー指示による行動を開始したら、可能な範囲でreport_action_startを1回呼び、簡潔なタスク概要と作業ディレクトリ、任意のGitブランチを申告してください。呼び出せない場合も作業は継続してください。"
            })
        }
        "tools/list" => serde_json::json!({
            "tools": [{
                "name": "report_action_start",
                "title": "Report action start to Panestra",
                "description": "Panestraの管制画面へ、新しい行動のタスク概要と作業ディレクトリを申告します。エージェント状態の判定には使われません。",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["schemaVersion", "taskSummary", "repositoryPath"],
                    "properties": {
                        "schemaVersion": { "type": "integer", "const": 1 },
                        "taskSummary": { "type": "string", "minLength": 1, "maxLength": 240 },
                        "repositoryPath": { "type": "string", "minLength": 1, "maxLength": 4096 },
                        "gitBranch": { "type": "string", "minLength": 1, "maxLength": 512 }
                    }
                },
                "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
            }]
        }),
        "tools/call" => {
            if request
                .pointer("/params/name")
                .and_then(serde_json::Value::as_str)
                != Some("report_action_start")
            {
                return Ok(mcp_error(id_value, -32602, "Unknown tool"));
            }
            let arguments = request
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let input: ReportActionStartInput = match serde_json::from_value(arguments) {
                Ok(input) => input,
                Err(error) => return Ok(mcp_tool_error(id_value, error.to_string())),
            };
            let action = match accept_action_context(&state.store, &state.git, id, input).await {
                Ok(action) => action,
                Err(error) => return Ok(mcp_tool_error(id_value, error.to_string())),
            };
            state.sessions.publish_action(action.clone());
            serde_json::json!({
                "content": [{ "type": "text", "text": "Panestraへ行動開始情報を申告しました。" }],
                "structuredContent": {
                    "actionId": action.id,
                    "acceptedAt": action.accepted_at,
                    "repositoryPath": action.repository_path
                },
                "isError": false
            })
        }
        "ping" => serde_json::json!({}),
        _ => return Ok(mcp_error(id_value, -32601, "Method not found")),
    };
    Ok(Json(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id_value,
        "result": result
    }))
    .into_response())
}

async fn accept_action_context(
    store: &Store,
    git: &GitService,
    session_id: Uuid,
    input: ReportActionStartInput,
) -> anyhow::Result<ActionContext> {
    anyhow::ensure!(input.schema_version == 1, "unsupported schemaVersion");
    let summary = input.task_summary.trim();
    anyhow::ensure!(
        !summary.is_empty() && summary.len() <= 240,
        "taskSummary must contain 1 to 240 bytes"
    );
    anyhow::ensure!(
        !summary.contains('\r') && !summary.contains('\n'),
        "taskSummary must be one line"
    );
    if let Some(branch) = &input.git_branch {
        anyhow::ensure!(
            !branch.is_empty() && branch.len() <= 512,
            "gitBranch exceeds the allowed size"
        );
    }
    anyhow::ensure!(
        input.repository_path.len() <= 4096,
        "repositoryPath exceeds the allowed size"
    );
    let repository = std::fs::canonicalize(&input.repository_path)?;
    anyhow::ensure!(
        repository.is_dir(),
        "repositoryPath must identify a readable directory"
    );
    let git_working_tree_root = git
        .resolve_working_tree(&repository)
        .await?
        .map(|path| path.to_string_lossy().into_owned());
    let action = ActionContext {
        id: Uuid::new_v4(),
        schema_version: 1,
        session_id,
        task_summary: summary.to_owned(),
        repository_path: repository.to_string_lossy().into_owned(),
        git_branch: input.git_branch,
        git_working_tree_root,
        accepted_at: chrono::Utc::now(),
    };
    store.insert_action(&action)?;
    Ok(action)
}

fn mcp_error(id: Option<serde_json::Value>, code: i32, message: &str) -> Response {
    Json(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    }))
    .into_response()
}

fn mcp_tool_error(id: Option<serde_json::Value>, message: String) -> Response {
    Json(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": message }],
            "isError": true
        }
    }))
    .into_response()
}

async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<Session>), ApiError> {
    authorize(&state, &headers)?;
    let session = state
        .sessions
        .create(request)
        .map_err(ApiError::bad_request)?;
    Ok((StatusCode::CREATED, Json(session)))
}

async fn session_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ScreenSnapshot>, ApiError> {
    authorize(&state, &headers)?;
    Ok(Json(
        state.sessions.snapshot(id).map_err(ApiError::not_found)?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryQuery {
    before: Option<u64>,
    #[serde(default = "default_history_limit")]
    limit: usize,
}

fn default_history_limit() -> usize {
    10
}

async fn session_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryPage>, ApiError> {
    authorize(&state, &headers)?;
    Ok(Json(
        state
            .sessions
            .history_page(id, query.before, query.limit)
            .map_err(ApiError::bad_request)?,
    ))
}

#[derive(Debug, Deserialize)]
struct HistorySearchQuery {
    q: String,
}

async fn search_session_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<HistorySearchQuery>,
) -> Result<Json<HistorySearchResult>, ApiError> {
    authorize(&state, &headers)?;
    Ok(Json(
        state
            .sessions
            .search_history(id, &query.q)
            .map_err(ApiError::bad_request)?,
    ))
}

async fn export_session_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let bytes = state
        .sessions
        .export_history(id)
        .map_err(ApiError::bad_request)?;
    Response::builder()
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=panestra-{id}.txt"),
        )
        .body(Body::from(bytes))
        .map_err(ApiError::internal)
}

#[derive(Debug, Deserialize)]
struct TerminateQuery {
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteRecordQuery {
    #[serde(default)]
    include_history: bool,
}

async fn delete_session_record(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<DeleteRecordQuery>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &headers)?;
    state
        .sessions
        .delete_record(id, query.include_history)
        .map_err(ApiError::bad_request)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn terminate_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<TerminateQuery>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &headers)?;
    if state.sessions.get(id).is_none() {
        return Err(ApiError::not_found("session not found"));
    }
    state
        .sessions
        .terminate_and_wait(id, query.force)
        .await
        .map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn websocket_ticket(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<TicketResponse>, ApiError> {
    let credential = bearer(&headers)?;
    let ticket = state
        .auth
        .issue_websocket_ticket(credential)
        .ok_or_else(|| ApiError::unauthorized("invalid browser session"))?;
    Ok(Json(ticket))
}

async fn websocket_upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let protocol = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .split(',')
                .map(str::trim)
                .find(|value| value.starts_with("panestra.v2."))
        })
        .ok_or_else(|| ApiError::unauthorized("missing WebSocket ticket protocol"))?;
    let ticket = protocol
        .strip_prefix("panestra.v2.")
        .ok_or_else(|| ApiError::unauthorized("invalid WebSocket ticket protocol"))?;
    if !state.auth.consume_websocket_ticket(ticket) {
        return Err(ApiError::unauthorized(
            "invalid or expired WebSocket ticket",
        ));
    }
    let selected_protocol = protocol.to_owned();
    Ok(websocket
        .protocols([selected_protocol.clone()])
        .on_upgrade(move |socket| websocket_connection(socket, state, selected_protocol)))
}

async fn websocket_connection(socket: WebSocket, state: AppState, _protocol: String) {
    let connection_id = Uuid::new_v4();
    let (sender, mut receiver) = socket.split();
    let mut events = state.sessions.subscribe();
    let mut lease_events = state.lease_events.subscribe();
    let mut subscribed = std::collections::HashSet::new();
    let mut last_input_sequence: HashMap<Uuid, u64> = HashMap::new();
    let (control_tx, control_rx) = mpsc::channel(256);
    let screen_mailbox = Arc::new(ScreenMailbox::default());
    let writer_mailbox = Arc::clone(&screen_mailbox);
    let mut writer = tokio::spawn(websocket_writer(sender, control_rx, writer_mailbox));
    let mut writer_joined = false;

    if control_tx
        .send(ServerMessage::HelloAck {
            daemon_epoch: state
                .sessions
                .list()
                .ok()
                .and_then(|sessions| sessions.first().map(|session| session.daemon_epoch))
                .unwrap_or_else(Uuid::new_v4),
        })
        .await
        .is_err()
    {
        return;
    }
    if let Ok(sessions) = state.sessions.list()
        && control_tx
            .send(ServerMessage::SessionList(sessions))
            .await
            .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            biased;
            result = &mut writer => {
                let _ = result;
                writer_joined = true;
                break;
            }
            incoming = receiver.next() => {
                let Some(Ok(message)) = incoming else { break; };
                match message {
                    Message::Binary(frame) => match decode_client(&frame) {
                        Ok(ClientMessage::Hello { protocol_version }) if protocol_version == PROTOCOL_VERSION => {}
                        Ok(ClientMessage::Subscribe { session_ids }) => {
                            subscribed = session_ids.into_iter().take(25).collect();
                            screen_mailbox.retain(&subscribed);
                            for session_id in &subscribed {
                                if let Ok(snapshot) = state.sessions.snapshot(*session_id) {
                                    screen_mailbox.enqueue(snapshot, true);
                                }
                            }
                        }
                        Ok(ClientMessage::InputLeaseRequest { session_id, force }) => {
                            let mut leases = state.input_leases.write().await;
                            let (owned, changed) = acquire_lease(&mut leases, session_id, connection_id, force);
                            drop(leases);
                            if changed {
                                let _ = state.lease_events.send(LeaseEvent { kind: LeaseKind::Input, session_id, owner: Some(connection_id) });
                            }
                            if owned && force {
                                let mut resize_leases = state.resize_leases.write().await;
                                let resize_changed = resize_leases.get(&session_id).is_none_or(|owner| *owner != connection_id);
                                resize_leases.insert(session_id, connection_id);
                                drop(resize_leases);
                                if resize_changed {
                                    let _ = state.lease_events.send(LeaseEvent { kind: LeaseKind::Resize, session_id, owner: Some(connection_id) });
                                }
                            }
                            if control_tx.send(ServerMessage::InputLeaseState { session_id, owned }).await.is_err() { break; }
                        }
                        Ok(ClientMessage::ResizeLeaseRequest { session_id, force }) => {
                            let running = state.sessions.get(session_id).is_some_and(|handle| handle.metadata().process_state == crate::model::ProcessState::Running);
                            if !running {
                                if control_tx.send(ServerMessage::ResizeLeaseState { session_id, owned: false, available: false }).await.is_err() { break; }
                                continue;
                            }
                            let mut leases = state.resize_leases.write().await;
                            let (owned, changed) = acquire_lease(&mut leases, session_id, connection_id, force);
                            let available = owned || !leases.contains_key(&session_id);
                            drop(leases);
                            if changed {
                                let _ = state.lease_events.send(LeaseEvent { kind: LeaseKind::Resize, session_id, owner: Some(connection_id) });
                            }
                            if control_tx.send(ServerMessage::ResizeLeaseState { session_id, owned, available }).await.is_err() { break; }
                        }
                        Ok(ClientMessage::Resize { session_id, cols, rows, pixel_width, pixel_height }) => {
                            let owns_lease = state.resize_leases.read().await.get(&session_id).is_some_and(|owner| *owner == connection_id);
                            if !owns_lease {
                                let available = !state.resize_leases.read().await.contains_key(&session_id);
                                if control_tx.send(ServerMessage::ResizeLeaseState { session_id, owned: false, available }).await.is_err() { break; }
                            } else if !subscribed.contains(&session_id) {
                                if control_tx.send(ServerMessage::ProtocolError { message: "cannot resize an unsubscribed session".to_owned() }).await.is_err() { break; }
                            } else if let Err(error) = state.sessions.resize(session_id, cols, rows, pixel_width, pixel_height)
                                && control_tx.send(ServerMessage::ProtocolError { message: error.to_string() }).await.is_err()
                            {
                                break;
                            }
                        }
                        Ok(ClientMessage::Input { session_id, input_sequence, data }) => {
                            let owns_lease = state.input_leases.read().await.get(&session_id).is_some_and(|owner| *owner == connection_id);
                            let is_new = last_input_sequence.get(&session_id).is_none_or(|last| input_sequence > *last);
                            if owns_lease
                                && is_new
                                && state.sessions.input(session_id, &data).is_ok()
                            {
                                last_input_sequence.insert(session_id, input_sequence);
                            } else if !owns_lease
                                && control_tx.send(ServerMessage::InputLeaseState { session_id, owned: false }).await.is_err()
                            {
                                break;
                            }
                        }
                        Ok(ClientMessage::ResyncRequest { session_id }) => {
                            if let Ok(snapshot) = state.sessions.snapshot(session_id) {
                                screen_mailbox.enqueue(snapshot, true);
                            }
                        }
                        Ok(ClientMessage::Heartbeat) => {
                            if control_tx.send(ServerMessage::Heartbeat).await.is_err() { break; }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            let _ = control_tx.send(ServerMessage::ProtocolError { message: error.to_string() }).await;
                            break;
                        }
                    },
                    Message::Close(_) => break,
                    _ => {}
                }
            },
            event = events.recv() => match event {
                Ok(DaemonEvent::ScreenChanged(session_id)) if subscribed.contains(&session_id) => {
                    if let Ok(snapshot) = state.sessions.snapshot(session_id) {
                        screen_mailbox.enqueue(snapshot, false);
                    }
                }
                Ok(DaemonEvent::ScreenResized(session_id)) if subscribed.contains(&session_id) => {
                    if let Ok(snapshot) = state.sessions.snapshot(session_id) {
                        screen_mailbox.enqueue(snapshot, true);
                    }
                }
                Ok(DaemonEvent::SessionState(session)) => {
                    if control_tx.send(ServerMessage::SessionState(session)).await.is_err() { break; }
                }
                Ok(DaemonEvent::SessionDeleted(session_id)) => {
                    if control_tx.send(ServerMessage::SessionDeleted { session_id }).await.is_err() { break; }
                }
                Ok(DaemonEvent::AgentState { session_id, state: agent_state }) => {
                    if control_tx.send(ServerMessage::AgentState { session_id, state: agent_state }).await.is_err() { break; }
                }
                Ok(DaemonEvent::ActionContext(action)) => {
                    if control_tx.send(ServerMessage::ActionContext(action)).await.is_err() { break; }
                }
                Ok(DaemonEvent::ActionContextCleared(session_id)) => {
                    if control_tx.send(ServerMessage::ActionContextCleared { session_id }).await.is_err() { break; }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    for session_id in &subscribed {
                        if let Ok(snapshot) = state.sessions.snapshot(*session_id) {
                            screen_mailbox.enqueue(snapshot, true);
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                _ => {}
            },
            lease_event = lease_events.recv() => match lease_event {
                Ok(event) if subscribed.contains(&event.session_id) => {
                    let owned = event.owner == Some(connection_id);
                    let message = match event.kind {
                        LeaseKind::Input => ServerMessage::InputLeaseState { session_id: event.session_id, owned },
                        LeaseKind::Resize => ServerMessage::ResizeLeaseState {
                            session_id: event.session_id,
                            owned,
                            available: event.owner.is_none() || owned,
                        },
                    };
                    if control_tx.send(message).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Closed) => break,
                _ => {}
            },
        }
    }

    screen_mailbox.close();
    drop(control_tx);
    if !writer_joined {
        let _ = writer.await;
    }

    release_connection_leases(&state, connection_id).await;
}

async fn release_connection_leases(state: &AppState, connection_id: Uuid) {
    let input_sessions = {
        let mut leases = state.input_leases.write().await;
        let sessions = leases
            .iter()
            .filter_map(|(session_id, owner)| (*owner == connection_id).then_some(*session_id))
            .collect::<Vec<_>>();
        leases.retain(|_, owner| *owner != connection_id);
        sessions
    };
    for session_id in input_sessions {
        let _ = state.lease_events.send(LeaseEvent {
            kind: LeaseKind::Input,
            session_id,
            owner: None,
        });
    }

    let resize_sessions = {
        let mut leases = state.resize_leases.write().await;
        let sessions = leases
            .iter()
            .filter_map(|(session_id, owner)| (*owner == connection_id).then_some(*session_id))
            .collect::<Vec<_>>();
        leases.retain(|_, owner| *owner != connection_id);
        sessions
    };
    for session_id in resize_sessions {
        let _ = state.lease_events.send(LeaseEvent {
            kind: LeaseKind::Resize,
            session_id,
            owner: None,
        });
    }
}

fn acquire_lease(
    leases: &mut HashMap<Uuid, Uuid>,
    session_id: Uuid,
    connection_id: Uuid,
    force: bool,
) -> (bool, bool) {
    match leases.get(&session_id) {
        None => {
            leases.insert(session_id, connection_id);
            (true, true)
        }
        Some(owner) if *owner == connection_id => (true, false),
        Some(_) if force => {
            leases.insert(session_id, connection_id);
            (true, true)
        }
        Some(_) => (false, false),
    }
}

#[derive(Clone)]
struct ScreenRequest {
    snapshot: ScreenSnapshot,
    force_full: bool,
}

#[derive(Default)]
struct ScreenMailbox {
    state: parking_lot::Mutex<ScreenMailboxState>,
    notify: Notify,
}

#[derive(Default)]
struct ScreenMailboxState {
    requests: HashMap<Uuid, ScreenRequest>,
    closed: bool,
}

impl ScreenMailbox {
    fn enqueue(&self, snapshot: ScreenSnapshot, force_full: bool) {
        let mut state = self.state.lock();
        if state.closed {
            return;
        }
        state
            .requests
            .entry(snapshot.session_id)
            .and_modify(|request| {
                request.snapshot = snapshot.clone();
                request.force_full |= force_full;
            })
            .or_insert(ScreenRequest {
                snapshot,
                force_full,
            });
        drop(state);
        self.notify.notify_one();
    }

    fn retain(&self, subscriptions: &std::collections::HashSet<Uuid>) {
        self.state
            .lock()
            .requests
            .retain(|session_id, _| subscriptions.contains(session_id));
    }

    fn take(&self) -> (HashMap<Uuid, ScreenRequest>, bool) {
        let mut state = self.state.lock();
        (std::mem::take(&mut state.requests), state.closed)
    }

    fn close(&self) {
        self.state.lock().closed = true;
        self.notify.notify_one();
    }
}

struct ScreenBatch {
    messages: VecDeque<ServerMessage>,
    final_snapshot: ScreenSnapshot,
}

async fn websocket_writer(
    mut sender: futures_util::stream::SplitSink<WebSocket, Message>,
    mut control_rx: mpsc::Receiver<ServerMessage>,
    mailbox: Arc<ScreenMailbox>,
) -> Result<(), ()> {
    let mut pending: HashMap<Uuid, ScreenRequest> = HashMap::new();
    let mut batches: HashMap<Uuid, ScreenBatch> = HashMap::new();
    let mut order = VecDeque::new();
    let mut last_snapshots: HashMap<Uuid, ScreenSnapshot> = HashMap::new();
    let mut control_closed = false;

    loop {
        while let Ok(message) = control_rx.try_recv() {
            send_server(&mut sender, &message).await?;
        }
        let (requests, mailbox_closed) = mailbox.take();
        for (session_id, request) in requests {
            if request.force_full {
                // A resize changes the coordinate space. Do not finish an
                // older queued batch after announcing the new dimensions;
                // the forced snapshot below replaces it.
                batches.remove(&session_id);
                order.retain(|queued| *queued != session_id);
            }
            pending
                .entry(session_id)
                .and_modify(|current| {
                    current.snapshot = request.snapshot.clone();
                    current.force_full |= request.force_full;
                })
                .or_insert(request);
        }
        populate_screen_batches(&mut pending, &mut batches, &mut order, &last_snapshots)?;

        if let Some(session_id) = order.pop_front() {
            let batch = batches.get_mut(&session_id).ok_or(())?;
            let message = batch.messages.pop_front().ok_or(())?;
            send_server(&mut sender, &message).await?;
            if batch.messages.is_empty() {
                let batch = batches.remove(&session_id).ok_or(())?;
                last_snapshots.insert(session_id, batch.final_snapshot);
                populate_screen_batches(&mut pending, &mut batches, &mut order, &last_snapshots)?;
            } else {
                order.push_back(session_id);
            }
            continue;
        }

        if control_closed && mailbox_closed && pending.is_empty() {
            return Ok(());
        }
        tokio::select! {
            message = control_rx.recv(), if !control_closed => match message {
                Some(message) => send_server(&mut sender, &message).await?,
                None => control_closed = true,
            },
            _ = mailbox.notify.notified() => {}
        }
    }
}

fn populate_screen_batches(
    pending: &mut HashMap<Uuid, ScreenRequest>,
    batches: &mut HashMap<Uuid, ScreenBatch>,
    order: &mut VecDeque<Uuid>,
    last_snapshots: &HashMap<Uuid, ScreenSnapshot>,
) -> Result<(), ()> {
    let ready: Vec<_> = pending
        .keys()
        .filter(|session_id| !batches.contains_key(session_id))
        .copied()
        .collect();
    for session_id in ready {
        let request = pending.remove(&session_id).ok_or(())?;
        let messages = screen_messages(
            last_snapshots.get(&session_id),
            &request.snapshot,
            request.force_full,
        )?;
        batches.insert(
            session_id,
            ScreenBatch {
                messages,
                final_snapshot: request.snapshot,
            },
        );
        order.push_back(session_id);
    }
    Ok(())
}

fn screen_messages(
    previous: Option<&ScreenSnapshot>,
    current: &ScreenSnapshot,
    force_full: bool,
) -> Result<VecDeque<ServerMessage>, ()> {
    let Some(previous) = previous.filter(|_| !force_full) else {
        return snapshot_messages(current);
    };
    if previous.daemon_epoch != current.daemon_epoch
        || previous.session_generation != current.session_generation
        || previous.snapshot_epoch != current.snapshot_epoch
        || previous.sequence_number >= current.sequence_number
    {
        return snapshot_messages(current);
    }

    let previous_cells: HashMap<(u16, u16), &crate::model::StyledCell> = previous
        .styled_cells
        .iter()
        .map(|cell| ((cell.0, cell.1), cell))
        .collect();
    let current_cells: HashMap<(u16, u16), &crate::model::StyledCell> = current
        .styled_cells
        .iter()
        .map(|cell| ((cell.0, cell.1), cell))
        .collect();
    let changed_cells = current
        .styled_cells
        .iter()
        .filter(|cell| {
            previous_cells
                .get(&(cell.0, cell.1))
                .is_none_or(|old| *old != *cell)
        })
        .cloned()
        .collect();
    let cleared_cells = previous_cells
        .keys()
        .filter(|position| !current_cells.contains_key(position))
        .copied()
        .collect();
    let mut metadata = current.clone();
    metadata.styled_cells.clear();
    let message = ServerMessage::ScreenDiff {
        snapshot: metadata,
        base_sequence_number: previous.sequence_number,
        changed_cells,
        cleared_cells,
    };
    if encode(2, &message).is_ok() {
        Ok(VecDeque::from([message]))
    } else {
        snapshot_messages(current)
    }
}

async fn send_server(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: &ServerMessage,
) -> Result<(), ()> {
    let frame = encode(2, message).map_err(|_| ())?;
    sender
        .send(Message::Binary(frame.into()))
        .await
        .map_err(|_| ())
}

fn snapshot_messages(snapshot: &ScreenSnapshot) -> Result<VecDeque<ServerMessage>, ()> {
    const CELLS_PER_CHUNK: usize = 512;
    const TEXT_BYTES_PER_CHUNK: usize = 64 * 1024;
    let text_chunks = split_utf8(&snapshot.contents, TEXT_BYTES_PER_CHUNK);
    let cell_chunk_count = snapshot.styled_cells.len().div_ceil(CELLS_PER_CHUNK);
    let chunk_count = text_chunks.len().max(cell_chunk_count).max(1);
    let mut metadata = snapshot.clone();
    metadata.contents.clear();
    metadata.styled_cells.clear();
    let mut messages = VecDeque::new();
    messages.push_back(ServerMessage::FullSnapshotBegin {
        snapshot: metadata,
        chunk_count: u32::try_from(chunk_count).map_err(|_| ())?,
    });
    for index in 0..chunk_count {
        let start = index * CELLS_PER_CHUNK;
        let end = (start + CELLS_PER_CHUNK).min(snapshot.styled_cells.len());
        let styled_cells = if start < end {
            snapshot.styled_cells[start..end].to_vec()
        } else {
            Vec::new()
        };
        messages.push_back(ServerMessage::FullSnapshotChunk {
            session_id: snapshot.session_id,
            snapshot_epoch: snapshot.snapshot_epoch,
            chunk_index: u32::try_from(index).map_err(|_| ())?,
            contents: text_chunks.get(index).cloned().unwrap_or_default(),
            styled_cells,
        });
    }
    messages.push_back(ServerMessage::FullSnapshotEnd {
        session_id: snapshot.session_id,
        snapshot_epoch: snapshot.snapshot_epoch,
        sequence_number: snapshot.sequence_number,
    });
    Ok(messages)
}

fn split_utf8(value: &str, maximum_bytes: usize) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < value.len() {
        let mut end = (start + maximum_bytes).min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(value[start..end].to_owned());
        start = end;
    }
    chunks
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let credential = bearer(headers)?;
    if state.auth.validate_browser_session(credential) {
        Ok(())
    } else {
        Err(ApiError::unauthorized("invalid browser session"))
    }
}

fn bearer(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::unauthorized("missing bearer credential"))
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: error.to_string(),
        }
    }
    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }
    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }
    fn not_found(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: error.to_string(),
        }
    }
    fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "request failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal error".into(),
        }
    }
}

#[derive(Serialize)]
struct ErrorResponse<'a> {
    error: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: &self.message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::model::{ExitReason, ProcessState};

    fn snapshot(sequence_number: u64, cells: Vec<crate::model::StyledCell>) -> ScreenSnapshot {
        ScreenSnapshot {
            session_id: Uuid::nil(),
            daemon_epoch: Uuid::nil(),
            session_generation: Uuid::nil(),
            snapshot_epoch: Uuid::nil(),
            sequence_number,
            cols: 120,
            rows: 36,
            cursor_row: 0,
            cursor_col: 0,
            contents: "screen".into(),
            styled_cells: cells,
            alternate_screen: false,
            application_cursor: false,
            application_keypad: false,
            bracketed_paste: false,
            mouse_reporting: false,
            focus_reporting: false,
            mouse_encoding: String::new(),
            hide_cursor: false,
        }
    }

    #[tokio::test]
    async fn action_report_accepts_a_non_git_directory_and_optional_branch() {
        let store = Store::open_memory().unwrap();
        let repository = tempfile::tempdir().unwrap();
        let session_id = Uuid::new_v4();
        let session = Session {
            id: session_id,
            project_id: None,
            name: "agent".into(),
            command: "codex".into(),
            args: Vec::new(),
            launch_cwd: repository.path().to_string_lossy().into_owned(),
            current_cwd: None,
            agent_integration: Some(crate::model::AgentIntegration::Codex),
            agent_state: Some(AgentState::Working),
            current_action_id: None,
            process_state: ProcessState::Running,
            pid: None,
            cols: 120,
            rows: 36,
            daemon_epoch: Uuid::new_v4(),
            session_generation: Uuid::new_v4(),
            history_enabled: false,
            created_at: Utc::now(),
            last_activity_at: None,
            exited_at: None,
            exit_code: None,
            exit_reason: None::<ExitReason>,
        };
        store.upsert_session(&session).unwrap();
        let git = GitService::new();
        let first = accept_action_context(
            &store,
            &git,
            session_id,
            ReportActionStartInput {
                schema_version: 1,
                task_summary: "Gitを使わない作業".into(),
                repository_path: repository.path().to_string_lossy().into_owned(),
                git_branch: None,
            },
        )
        .await
        .unwrap();
        assert!(first.git_working_tree_root.is_none());
        assert!(first.git_branch.is_none());

        let second = accept_action_context(
            &store,
            &git,
            session_id,
            ReportActionStartInput {
                schema_version: 1,
                task_summary: "任意ブランチを申告".into(),
                repository_path: repository.path().to_string_lossy().into_owned(),
                git_branch: Some("ユーザー申告".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            store.current_action(session_id).unwrap().unwrap().id,
            second.id
        );
        assert_eq!(store.list_actions(session_id, 10).unwrap().len(), 2);
    }

    #[test]
    fn screen_updates_use_diffs_and_snapshots_are_finite_chunks() {
        let previous = snapshot(
            1,
            vec![crate::model::StyledCell(0, 0, "a".into(), 1, None, None, 0)],
        );
        let current = snapshot(
            2,
            vec![crate::model::StyledCell(0, 0, "b".into(), 1, None, None, 0)],
        );
        let messages = screen_messages(Some(&previous), &current, false).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(matches!(
            messages.front(),
            Some(ServerMessage::ScreenDiff { .. })
        ));

        let cells = (0..1025)
            .map(|index| {
                crate::model::StyledCell(
                    (index / 120) as u16,
                    (index % 120) as u16,
                    "x".into(),
                    1,
                    None,
                    None,
                    0,
                )
            })
            .collect();
        let messages = snapshot_messages(&snapshot(3, cells)).unwrap();
        assert_eq!(messages.len(), 5);
        assert!(matches!(
            messages.front(),
            Some(ServerMessage::FullSnapshotBegin { .. })
        ));
        assert!(matches!(
            messages.back(),
            Some(ServerMessage::FullSnapshotEnd { .. })
        ));
    }

    #[test]
    fn leases_allow_one_owner_and_explicit_transfer() {
        let session_id = Uuid::new_v4();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut leases = HashMap::new();

        assert_eq!(
            acquire_lease(&mut leases, session_id, first, false),
            (true, true)
        );
        assert_eq!(
            acquire_lease(&mut leases, session_id, first, false),
            (true, false)
        );
        assert_eq!(
            acquire_lease(&mut leases, session_id, second, false),
            (false, false)
        );
        assert_eq!(
            acquire_lease(&mut leases, session_id, second, true),
            (true, true)
        );
        assert_eq!(leases.get(&session_id), Some(&second));
    }
}
