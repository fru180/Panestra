use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_SESSIONS: usize = 25;
pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;
pub const MIN_COLS: u16 = 40;
pub const MAX_COLS: u16 = 300;
pub const MIN_ROWS: u16 = 12;
pub const MAX_ROWS: u16 = 120;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentIntegration {
    Codex,
    Claude,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Starting,
    Running,
    Exited,
    Killed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Initializing,
    Working,
    WaitingApproval,
    AwaitingUser,
    IntegrationError,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    Normal,
    Signal,
    Forced,
    DaemonTerminated,
    LaunchFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    pub name: String,
    pub project_id: Option<Uuid>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: String,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    pub agent_integration: Option<AgentIntegration>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    #[serde(default = "default_history_enabled")]
    pub history_enabled: bool,
}

fn default_history_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub launch_cwd: String,
    pub current_cwd: Option<String>,
    pub agent_integration: Option<AgentIntegration>,
    pub agent_state: Option<AgentState>,
    pub current_action_id: Option<Uuid>,
    pub process_state: ProcessState,
    pub pid: Option<u32>,
    pub cols: u16,
    pub rows: u16,
    pub daemon_epoch: Uuid,
    pub session_generation: Uuid,
    pub history_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub exited_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub exit_reason: Option<ExitReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenSnapshot {
    pub session_id: Uuid,
    pub daemon_epoch: Uuid,
    pub session_generation: Uuid,
    pub snapshot_epoch: Uuid,
    pub sequence_number: u64,
    pub cols: u16,
    pub rows: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub contents: String,
    #[serde(default)]
    pub styled_cells: Vec<StyledCell>,
    #[serde(default)]
    pub alternate_screen: bool,
    #[serde(default)]
    pub application_cursor: bool,
    #[serde(default)]
    pub application_keypad: bool,
    #[serde(default)]
    pub bracketed_paste: bool,
    #[serde(default)]
    pub mouse_reporting: bool,
    #[serde(default)]
    pub focus_reporting: bool,
    #[serde(default)]
    pub mouse_encoding: String,
    #[serde(default)]
    pub hide_cursor: bool,
}

/// Compact tuple encoded as [row, col, text, width, foreground, background, attributes].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StyledCell(
    pub u16,
    pub u16,
    pub String,
    pub u8,
    pub Option<u32>,
    pub Option<u32>,
    pub u8,
);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub root_path: String,
    pub color: Option<String>,
    pub tags: Vec<String>,
    pub repository_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectRequest {
    pub name: String,
    pub root_path: String,
    pub color: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub repository_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionContext {
    pub id: Uuid,
    pub schema_version: u16,
    pub session_id: Uuid,
    pub task_summary: String,
    pub repository_path: String,
    pub git_branch: Option<String>,
    pub git_working_tree_root: Option<String>,
    pub accepted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportActionStartInput {
    pub schema_version: u16,
    pub task_summary: String,
    pub repository_path: String,
    pub git_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub working_tree_root: String,
    pub actual_branch: Option<String>,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicts: u32,
    pub changed_files: u32,
    pub additions: u64,
    pub deletions: u64,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub stale: bool,
    pub error: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryChunk {
    pub sequence: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub session_id: Uuid,
    pub chunks: Vec<HistoryChunk>,
    pub next_before: Option<u64>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySearchResult {
    pub session_id: Uuid,
    pub query: String,
    pub matches: Vec<String>,
    pub truncated: bool,
}
