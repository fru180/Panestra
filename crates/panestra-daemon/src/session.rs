use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use parking_lot::{Mutex, RwLock};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    integration::IntegrationManager,
    model::{
        ActionContext, AgentState, CreateSessionRequest, ExitReason, MAX_COLS, MAX_ROWS,
        MAX_SESSIONS, MIN_COLS, MIN_ROWS, ProcessState, ScreenSnapshot, Session, StyledCell,
    },
    persistence::{HistoryChunkRef, Store},
};

const READ_BUFFER_SIZE: usize = 16 * 1024;
const SCROLLBACK_ROWS: usize = 10_000;
const MAX_INPUT_SIZE: usize = 64 * 1024;
const HISTORY_CHUNK_SIZE: usize = 256 * 1024;
const HISTORY_PENDING_LIMIT: usize = 1024 * 1024;
const HISTORY_DISK_LIMIT: u64 = 64 * 1024 * 1024;
const HISTORY_READ_LIMIT: usize = 8 * 1024 * 1024;
const INTEGRATION_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Default)]
struct HistoryBuffer {
    pending: Vec<u8>,
    next_sequence: u64,
}

#[derive(Debug, Clone)]
pub enum DaemonEvent {
    ScreenChanged(Uuid),
    ScreenResized(Uuid),
    SessionState(Session),
    AgentState { session_id: Uuid, state: AgentState },
    ActionContext(ActionContext),
    ActionContextCleared(Uuid),
    SessionDeleted(Uuid),
}

struct TerminalCallbacks {
    metadata: Arc<RwLock<Session>>,
    store: Store,
    events: broadcast::Sender<DaemonEvent>,
}

impl vt100::Callbacks for TerminalCallbacks {
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        let [b"7", value] = params else { return };
        let Ok(value) = std::str::from_utf8(value) else {
            return;
        };
        let Ok(url) = url::Url::parse(value) else {
            return;
        };
        let Ok(path) = url.to_file_path() else { return };
        let Ok(path) = std::fs::canonicalize(path) else {
            return;
        };
        if !path.is_dir() {
            return;
        }
        let mut metadata = self.metadata.write();
        metadata.current_cwd = Some(path.to_string_lossy().into_owned());
        let updated = metadata.clone();
        drop(metadata);
        if self.store.upsert_session(&updated).is_ok() {
            let _ = self.events.send(DaemonEvent::SessionState(updated));
        }
    }
}

pub struct SessionHandle {
    metadata: Arc<RwLock<Session>>,
    writer: Mutex<Box<dyn Write + Send>>,
    pty_master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    process_group_id: Option<i32>,
    terminal: Arc<Mutex<vt100::Parser<TerminalCallbacks>>>,
    snapshot_epoch: Uuid,
    sequence: AtomicU64,
    history: Mutex<HistoryBuffer>,
    history_commit: Mutex<()>,
    forced_termination: AtomicBool,
    integration_event_received: AtomicBool,
    focus_reporting: AtomicBool,
    terminal_mode_tail: Mutex<Vec<u8>>,
    #[cfg(unix)]
    _watchdog: Option<std::os::unix::net::UnixStream>,
}

impl SessionHandle {
    pub fn metadata(&self) -> Session {
        self.metadata.read().clone()
    }

    pub fn snapshot(&self) -> ScreenSnapshot {
        let sequence_number = self.sequence.load(Ordering::Acquire);
        let terminal = self.terminal.lock();
        let metadata = self.metadata.read().clone();
        let screen = terminal.screen();
        let (cursor_row, cursor_col) = screen.cursor_position();
        let mut styled_cells = Vec::new();
        for row in 0..metadata.rows {
            for col in 0..metadata.cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                let foreground = terminal_color(cell.fgcolor());
                let background = terminal_color(cell.bgcolor());
                let attributes = u8::from(cell.bold())
                    | (u8::from(cell.dim()) << 1)
                    | (u8::from(cell.italic()) << 2)
                    | (u8::from(cell.underline()) << 3)
                    | (u8::from(cell.inverse()) << 4);
                if cell.has_contents() || background.is_some() || attributes != 0 {
                    styled_cells.push(StyledCell(
                        row,
                        col,
                        cell.contents().to_owned(),
                        if cell.is_wide_continuation() {
                            0
                        } else if cell.is_wide() {
                            2
                        } else {
                            1
                        },
                        foreground,
                        background,
                        attributes,
                    ));
                }
            }
        }
        ScreenSnapshot {
            session_id: metadata.id,
            daemon_epoch: metadata.daemon_epoch,
            session_generation: metadata.session_generation,
            snapshot_epoch: self.snapshot_epoch,
            sequence_number,
            cols: metadata.cols,
            rows: metadata.rows,
            cursor_row,
            cursor_col,
            contents: screen.contents(),
            styled_cells,
            alternate_screen: screen.alternate_screen(),
            application_cursor: screen.application_cursor(),
            application_keypad: screen.application_keypad(),
            bracketed_paste: screen.bracketed_paste(),
            mouse_reporting: screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None,
            focus_reporting: self.focus_reporting.load(Ordering::Acquire),
            mouse_encoding: match screen.mouse_protocol_encoding() {
                vt100::MouseProtocolEncoding::Sgr => "sgr",
                vt100::MouseProtocolEncoding::Utf8 => "utf8",
                vt100::MouseProtocolEncoding::Default => "default",
            }
            .to_owned(),
            hide_cursor: screen.hide_cursor(),
        }
    }

    pub fn write_input(&self, data: &[u8]) -> Result<()> {
        if data.len() > MAX_INPUT_SIZE {
            bail!("input exceeds {MAX_INPUT_SIZE} bytes");
        }
        if self.metadata.read().process_state != ProcessState::Running {
            bail!("session is not running");
        }
        let mut writer = self.writer.lock();
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }

    pub fn terminate_gracefully(&self) -> Result<()> {
        if self.metadata.read().process_state != ProcessState::Running {
            return Ok(());
        }
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id {
            // Negative pid addresses the whole process group created for this PTY.
            let result = unsafe { libc::kill(-process_group_id, libc::SIGTERM) };
            if result == 0 {
                return Ok(());
            }
        }
        self.killer.lock().kill().map_err(Into::into)
    }

    pub fn kill(&self) -> Result<()> {
        self.forced_termination.store(true, Ordering::Release);
        self.killer.lock().kill().map_err(Into::into)
    }

    fn update_terminal_modes(&self, data: &[u8]) {
        const ENABLE: &[u8] = b"\x1b[?1004h";
        const DISABLE: &[u8] = b"\x1b[?1004l";
        let mut tail = self.terminal_mode_tail.lock();
        let mut combined = Vec::with_capacity(tail.len() + data.len());
        combined.extend_from_slice(&tail);
        combined.extend_from_slice(data);
        for window in combined.windows(ENABLE.len()) {
            if window == ENABLE {
                self.focus_reporting.store(true, Ordering::Release);
            } else if window == DISABLE {
                self.focus_reporting.store(false, Ordering::Release);
            }
        }
        let keep = combined.len().min(ENABLE.len() - 1);
        tail.clear();
        tail.extend_from_slice(&combined[combined.len() - keep..]);
    }
}

fn terminal_color(color: vt100::Color) -> Option<u32> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Rgb(red, green, blue) => {
            Some((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue))
        }
        vt100::Color::Idx(index) => Some(xterm_color(index)),
    }
}

fn xterm_color(index: u8) -> u32 {
    const BASIC: [u32; 16] = [
        0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666,
        0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
    ];
    if index < 16 {
        return BASIC[usize::from(index)];
    }
    if index < 232 {
        let value = index - 16;
        let component = |part: u8| {
            if part == 0 {
                0
            } else {
                55 + 40 * u32::from(part)
            }
        };
        let red = component(value / 36);
        let green = component((value % 36) / 6);
        let blue = component(value % 6);
        return (red << 16) | (green << 8) | blue;
    }
    let gray = 8 + 10 * u32::from(index - 232);
    (gray << 16) | (gray << 8) | gray
}

#[derive(Clone)]
pub struct SessionManager {
    daemon_epoch: Uuid,
    sessions: Arc<RwLock<HashMap<Uuid, Arc<SessionHandle>>>>,
    store: Store,
    events: broadcast::Sender<DaemonEvent>,
    integrations: IntegrationManager,
    checkpoint_dir: Arc<PathBuf>,
    history_dir: Arc<PathBuf>,
    daemon_stopping: Arc<AtomicBool>,
}

impl SessionManager {
    pub fn new(
        store: Store,
        daemon_epoch: Uuid,
        integrations: IntegrationManager,
        checkpoint_dir: PathBuf,
    ) -> Self {
        let (events, _) = broadcast::channel(1024);
        let history_dir = checkpoint_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("history");
        Self {
            daemon_epoch,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            store,
            events,
            integrations,
            checkpoint_dir: Arc::new(checkpoint_dir),
            history_dir: Arc::new(history_dir),
            daemon_stopping: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.events.subscribe()
    }

    pub fn list(&self) -> Result<Vec<Session>> {
        let mut by_id: HashMap<Uuid, Session> = self
            .store
            .list_sessions()?
            .into_iter()
            .map(|session| (session.id, session))
            .collect();
        for (id, handle) in self.sessions.read().iter() {
            by_id.insert(*id, handle.metadata());
        }
        let mut sessions: Vec<_> = by_id.into_values().collect();
        sessions.sort_by_key(|session| session.created_at);
        Ok(sessions)
    }

    pub fn get(&self, id: Uuid) -> Option<Arc<SessionHandle>> {
        self.sessions.read().get(&id).cloned()
    }

    pub fn create(&self, request: CreateSessionRequest) -> Result<Session> {
        validate_request(&request)?;
        if let Some(project_id) = request.project_id
            && !self.store.project_exists(project_id)?
        {
            bail!("project does not exist");
        }
        let running_count = self
            .sessions
            .read()
            .values()
            .filter(|handle| handle.metadata().process_state == ProcessState::Running)
            .count();
        if running_count >= MAX_SESSIONS {
            bail!("at most {MAX_SESSIONS} sessions can run at once");
        }
        let cwd = std::fs::canonicalize(&request.cwd)
            .with_context(|| format!("invalid working directory: {}", request.cwd))?;
        if !cwd.is_dir() {
            bail!("working directory is not a directory");
        }
        let cols = request.cols.unwrap_or(crate::model::DEFAULT_COLS);
        let rows = request.rows.unwrap_or(crate::model::DEFAULT_ROWS);
        let id = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let created_at = Utc::now();
        let prepared = request
            .agent_integration
            .map(|provider| {
                self.integrations
                    .prepare(id, provider, &request.command, &request.args)
            })
            .transpose()?;
        let launch_args = prepared
            .as_ref()
            .map_or_else(|| request.args.clone(), |launch| launch.args.clone());

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to allocate PTY")?;
        #[cfg(not(test))]
        let (watch_socket_path, watchdog_listener) = {
            let directory = self
                .checkpoint_dir
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("watchdogs");
            std::fs::create_dir_all(&directory)?;
            #[cfg(unix)]
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
            let path = directory.join(format!("{id}.sock"));
            let listener = std::os::unix::net::UnixListener::bind(&path)?;
            listener.set_nonblocking(true)?;
            let executable = std::env::current_exe().context("cannot locate Panestra launcher")?;
            let mut command = CommandBuilder::new(executable);
            command.args([
                "session-launcher",
                "--watch-socket",
                &path.to_string_lossy(),
                "--",
                &request.command,
            ]);
            command.args(&launch_args);
            (Some(path), (Some(listener), command))
        };
        #[cfg(not(test))]
        let (watchdog_listener, mut command) = watchdog_listener;
        #[cfg(test)]
        let (watch_socket_path, _watchdog_listener, mut command) = {
            let mut command = CommandBuilder::new(&request.command);
            command.args(&launch_args);
            (None::<PathBuf>, None::<()>, command)
        };
        command.cwd(&cwd);
        for (key, value) in &request.env {
            command.env(key, value);
        }
        if let Some(prepared) = &prepared {
            for (key, value) in &prepared.env {
                command.env(key, value);
            }
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");

        let mut child = match pair.slave.spawn_command(command) {
            Ok(child) => child,
            Err(error) => {
                if let Some(path) = &watch_socket_path {
                    let _ = std::fs::remove_file(path);
                }
                self.integrations.revoke(id);
                return Err(error).with_context(|| format!("failed to launch {}", request.command));
            }
        };
        #[cfg(not(test))]
        let watchdog = accept_watchdog(
            watchdog_listener.context("missing watchdog listener")?,
            watch_socket_path
                .as_deref()
                .context("missing watchdog path")?,
        )
        .inspect_err(|_| {
            let _ = child.kill();
        })?;
        #[cfg(test)]
        let watchdog: Option<std::os::unix::net::UnixStream> = None;
        drop(pair.slave);
        let pid = child.process_id();
        let killer = child.clone_killer();
        #[cfg(unix)]
        let process_group_id = pair.master.process_group_leader();
        #[cfg(not(unix))]
        let process_group_id = None;
        let mut reader = pair
            .master
            .try_clone_reader()
            .context("failed to open PTY reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("failed to open PTY writer")?;

        let metadata = Session {
            id,
            project_id: request.project_id,
            name: request.name.trim().to_owned(),
            command: request.command,
            args: launch_args,
            launch_cwd: cwd.to_string_lossy().into_owned(),
            current_cwd: None,
            agent_integration: request.agent_integration,
            agent_state: request.agent_integration.map(|_| AgentState::Initializing),
            current_action_id: None,
            process_state: ProcessState::Running,
            pid,
            cols,
            rows,
            daemon_epoch: self.daemon_epoch,
            session_generation: generation,
            history_enabled: request.history_enabled,
            created_at,
            last_activity_at: None,
            exited_at: None,
            exit_code: None,
            exit_reason: None,
        };
        self.store.upsert_session(&metadata)?;

        let metadata_shared = Arc::new(RwLock::new(metadata.clone()));
        let terminal = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            rows,
            cols,
            SCROLLBACK_ROWS,
            TerminalCallbacks {
                metadata: Arc::clone(&metadata_shared),
                store: self.store.clone(),
                events: self.events.clone(),
            },
        )));
        let handle = Arc::new(SessionHandle {
            metadata: Arc::clone(&metadata_shared),
            writer: Mutex::new(writer),
            pty_master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
            process_group_id,
            terminal: Arc::clone(&terminal),
            snapshot_epoch: Uuid::new_v4(),
            sequence: AtomicU64::new(0),
            history: Mutex::new(HistoryBuffer::default()),
            history_commit: Mutex::new(()),
            forced_termination: AtomicBool::new(false),
            integration_event_received: AtomicBool::new(false),
            focus_reporting: AtomicBool::new(false),
            terminal_mode_tail: Mutex::new(Vec::new()),
            _watchdog: watchdog,
        });
        self.sessions.write().insert(id, Arc::clone(&handle));

        let reader_metadata = Arc::clone(&metadata_shared);
        let reader_events = self.events.clone();
        let reader_handle = Arc::clone(&handle);
        let reader_store = self.store.clone();
        let reader_history_dir = Arc::clone(&self.history_dir);
        let reader_done = Arc::new(AtomicBool::new(false));
        let reader_done_signal = Arc::clone(&reader_done);
        thread::Builder::new()
            .name(format!("panestra-pty-{id}"))
            .spawn(move || {
                let mut buffer = vec![0_u8; READ_BUFFER_SIZE];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            if reader_handle.metadata().history_enabled
                                && let Err(error) = append_history(
                                    &reader_store,
                                    &reader_history_dir,
                                    &reader_handle,
                                    &buffer[..read],
                                )
                            {
                                tracing::error!(session_id = %id, %error, "failed to append terminal history");
                            }
                            reader_handle.update_terminal_modes(&buffer[..read]);
                            terminal.lock().process(&buffer[..read]);
                            reader_handle.sequence.fetch_add(1, Ordering::AcqRel);
                            reader_metadata.write().last_activity_at = Some(Utc::now());
                            let _ = reader_events.send(DaemonEvent::ScreenChanged(id));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                reader_done_signal.store(true, Ordering::Release);
            })
            .context("failed to start PTY reader")?;

        let wait_metadata = Arc::clone(&metadata_shared);
        let wait_store = self.store.clone();
        let wait_events = self.events.clone();
        let wait_integrations = self.integrations.clone();
        let wait_handle = Arc::clone(&handle);
        let wait_checkpoint_dir = Arc::clone(&self.checkpoint_dir);
        let wait_history_dir = Arc::clone(&self.history_dir);
        let wait_daemon_stopping = Arc::clone(&self.daemon_stopping);
        let wait_reader_done = Arc::clone(&reader_done);
        thread::Builder::new()
            .name(format!("panestra-wait-{id}"))
            .spawn(move || {
                let status = child.wait();
                for _ in 0..200 {
                    if wait_reader_done.load(Ordering::Acquire) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                let mut session = wait_metadata.write();
                session.pid = None;
                session.exited_at = Some(Utc::now());
                match status {
                    Ok(status) => {
                        session.exit_code = Some(status.exit_code() as i32);
                        if wait_daemon_stopping.load(Ordering::Acquire) {
                            session.process_state = ProcessState::Exited;
                            session.exit_reason = Some(ExitReason::DaemonTerminated);
                        } else if wait_handle.forced_termination.load(Ordering::Acquire) {
                            session.process_state = ProcessState::Killed;
                            session.exit_reason = Some(ExitReason::Forced);
                        } else {
                            session.process_state = ProcessState::Exited;
                            session.exit_reason = Some(if status.signal().is_some() {
                                ExitReason::Signal
                            } else {
                                ExitReason::Normal
                            });
                        }
                    }
                    Err(_) => {
                        session.process_state = ProcessState::Exited;
                        session.exit_reason = Some(ExitReason::Signal);
                    }
                }
                let final_session = session.clone();
                drop(session);
                if final_session.history_enabled {
                    let _ = commit_history(&wait_store, &wait_history_dir, &wait_handle);
                    let _ = commit_checkpoint(&wait_store, &wait_checkpoint_dir, &wait_handle);
                }
                let _ = wait_store.upsert_session(&final_session);
                wait_integrations.revoke(id);
                let _ = wait_events.send(DaemonEvent::SessionState(final_session));
            })
            .context("failed to start PTY wait thread")?;

        let _ = self
            .events
            .send(DaemonEvent::SessionState(metadata.clone()));
        if metadata.agent_integration.is_some() {
            let handshake_metadata = Arc::clone(&metadata_shared);
            let handshake_handle = Arc::clone(&handle);
            let handshake_store = self.store.clone();
            let handshake_events = self.events.clone();
            thread::Builder::new()
                .name(format!("panestra-integration-handshake-{id}"))
                .spawn(move || {
                    thread::sleep(INTEGRATION_HANDSHAKE_TIMEOUT);
                    let mut session = handshake_metadata.write();
                    if session.process_state == ProcessState::Running
                        && session.agent_state == Some(AgentState::Initializing)
                        && !handshake_handle
                            .integration_event_received
                            .load(Ordering::Acquire)
                    {
                        session.agent_state = Some(AgentState::IntegrationError);
                        let updated = session.clone();
                        drop(session);
                        let _ = handshake_store.upsert_session(&updated);
                        let _ = handshake_events.send(DaemonEvent::AgentState {
                            session_id: id,
                            state: AgentState::IntegrationError,
                        });
                    }
                })?;
        }
        Ok(metadata)
    }

    pub fn snapshot(&self, id: Uuid) -> Result<ScreenSnapshot> {
        if let Some(handle) = self.get(id) {
            return Ok(handle.snapshot());
        }
        self.store
            .load_checkpoint(id)?
            .ok_or_else(|| anyhow!("session has no committed screen checkpoint"))
    }

    pub fn input(&self, id: Uuid, data: &[u8]) -> Result<()> {
        let handle = self.get(id).ok_or_else(|| anyhow!("session not found"))?;
        handle.write_input(data)?;
        if handle.metadata().agent_state == Some(AgentState::WaitingApproval) {
            self.update_agent_state(id, AgentState::Working)?;
        }
        Ok(())
    }

    pub fn resize(
        &self,
        id: Uuid,
        cols: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Result<bool> {
        if !(MIN_COLS..=MAX_COLS).contains(&cols) || !(MIN_ROWS..=MAX_ROWS).contains(&rows) {
            bail!("terminal dimensions are outside the allowed range");
        }
        let handle = self.get(id).ok_or_else(|| anyhow!("session not found"))?;
        let current = handle.metadata();
        if current.process_state != ProcessState::Running {
            bail!("session is not running");
        }
        if current.cols == cols && current.rows == rows {
            return Ok(false);
        }

        handle.pty_master.lock().resize(PtySize {
            rows,
            cols,
            pixel_width,
            pixel_height,
        })?;
        handle.terminal.lock().screen_mut().set_size(rows, cols);
        let updated = {
            let mut metadata = handle.metadata.write();
            metadata.cols = cols;
            metadata.rows = rows;
            let updated = metadata.clone();
            self.store.upsert_session(&updated)?;
            updated
        };
        handle.sequence.fetch_add(1, Ordering::AcqRel);
        let _ = self.events.send(DaemonEvent::SessionState(updated));
        let _ = self.events.send(DaemonEvent::ScreenResized(id));
        Ok(true)
    }

    pub fn update_agent_state(&self, id: Uuid, state: AgentState) -> Result<()> {
        let handle = self.get(id).ok_or_else(|| anyhow!("session not found"))?;
        handle
            .integration_event_received
            .store(true, Ordering::Release);
        {
            let mut metadata = handle.metadata.write();
            if metadata.agent_integration.is_none() {
                bail!("session has no agent integration");
            }
            metadata.agent_state = Some(state);
            self.store.upsert_session(&metadata)?;
        }
        let _ = self.events.send(DaemonEvent::AgentState {
            session_id: id,
            state,
        });
        Ok(())
    }

    pub fn publish_action(&self, action: ActionContext) {
        if let Some(handle) = self.get(action.session_id) {
            handle.metadata.write().current_action_id = Some(action.id);
        }
        let _ = self.events.send(DaemonEvent::ActionContext(action));
    }

    pub fn clear_current_action(&self, id: Uuid) -> Result<()> {
        self.store.clear_current_action(id)?;
        if let Some(handle) = self.get(id) {
            handle.metadata.write().current_action_id = None;
        }
        let _ = self.events.send(DaemonEvent::ActionContextCleared(id));
        Ok(())
    }

    pub fn terminate(&self, id: Uuid, force: bool) -> Result<()> {
        let handle = self.get(id).ok_or_else(|| anyhow!("session not found"))?;
        if force {
            handle.kill()
        } else {
            handle.terminate_gracefully()
        }
    }

    pub fn delete_record(&self, id: Uuid, include_history: bool) -> Result<()> {
        if let Some(handle) = self.get(id)
            && matches!(
                handle.metadata().process_state,
                ProcessState::Starting | ProcessState::Running
            )
        {
            bail!("running sessions cannot be deleted");
        }
        let paths = self.store.delete_session(id, include_history)?;
        self.sessions.write().remove(&id);
        for path in paths {
            let _ = std::fs::remove_file(path);
        }
        let _ = self.events.send(DaemonEvent::SessionDeleted(id));
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.daemon_stopping.store(true, Ordering::Release);
        self.checkpoint_all();
        let handles: Vec<_> = self.sessions.read().values().cloned().collect();
        for handle in &handles {
            let _ = handle.terminate_gracefully();
        }
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        for handle in handles {
            if handle.metadata().process_state == ProcessState::Running {
                let _ = handle.kill();
            }
        }
    }

    pub fn checkpoint_all(&self) {
        for handle in self.sessions.read().values() {
            let metadata = handle.metadata();
            if metadata.history_enabled && metadata.process_state == ProcessState::Running {
                if let Err(error) = commit_history(&self.store, &self.history_dir, handle) {
                    tracing::error!(session_id = %metadata.id, %error, "failed to commit terminal history");
                }
                if let Err(error) = commit_checkpoint(&self.store, &self.checkpoint_dir, handle) {
                    tracing::error!(session_id = %metadata.id, %error, "failed to commit screen checkpoint");
                }
            }
        }
    }

    pub fn history_page(
        &self,
        id: Uuid,
        before: Option<u64>,
        limit: usize,
    ) -> Result<crate::model::HistoryPage> {
        if let Some(handle) = self.get(id)
            && handle.metadata().history_enabled
        {
            commit_history(&self.store, &self.history_dir, &handle)?;
        }
        let requested = limit.clamp(1, 50);
        let references = self.store.list_history_chunks(id, before, requested + 1)?;
        let truncated = references.len() > requested;
        let mut chunks = Vec::new();
        let mut total = 0;
        for reference in references.iter().take(requested) {
            let bytes = read_history_chunk(reference, HISTORY_READ_LIMIT.saturating_sub(total))?;
            total += bytes.len();
            let text = String::from_utf8_lossy(&strip_ansi_escapes::strip(&bytes)).into_owned();
            chunks.push(crate::model::HistoryChunk {
                sequence: reference.sequence,
                text,
            });
            if total >= HISTORY_READ_LIMIT {
                break;
            }
        }
        let next_before = chunks.last().map(|chunk| chunk.sequence);
        Ok(crate::model::HistoryPage {
            session_id: id,
            chunks,
            next_before,
            truncated: truncated || total >= HISTORY_READ_LIMIT,
        })
    }

    pub fn search_history(
        &self,
        id: Uuid,
        query: &str,
    ) -> Result<crate::model::HistorySearchResult> {
        if query.is_empty() || query.len() > 256 {
            bail!("history query must contain 1 to 256 bytes");
        }
        let references = self.store.list_history_chunks(id, None, 256)?;
        let mut matches = Vec::new();
        let mut total = 0;
        let mut truncated = false;
        for reference in references {
            let remaining = HISTORY_READ_LIMIT.saturating_sub(total);
            if remaining == 0 {
                truncated = true;
                break;
            }
            let bytes = read_history_chunk(&reference, remaining)?;
            total += bytes.len();
            let plain = String::from_utf8_lossy(&strip_ansi_escapes::strip(&bytes)).into_owned();
            for line in plain.lines().filter(|line| line.contains(query)) {
                matches.push(line.chars().take(1000).collect());
                if matches.len() >= 200 {
                    truncated = true;
                    break;
                }
            }
            if truncated {
                break;
            }
        }
        Ok(crate::model::HistorySearchResult {
            session_id: id,
            query: query.to_owned(),
            matches,
            truncated,
        })
    }

    pub fn export_history(&self, id: Uuid) -> Result<Vec<u8>> {
        let references = self.store.list_history_chunks(id, None, 512)?;
        let mut output = Vec::new();
        for reference in references.into_iter().rev() {
            let remaining = HISTORY_READ_LIMIT.saturating_sub(output.len());
            if remaining == 0 {
                break;
            }
            let bytes = read_history_chunk(&reference, remaining)?;
            output.extend_from_slice(&strip_ansi_escapes::strip(&bytes));
        }
        Ok(output)
    }
}

fn append_history(
    store: &Store,
    directory: &Path,
    handle: &SessionHandle,
    data: &[u8],
) -> Result<()> {
    let commit = {
        let mut history = handle.history.lock();
        if history.pending.len().saturating_add(data.len()) > HISTORY_PENDING_LIMIT {
            bail!("terminal history pending buffer reached its limit");
        }
        history.pending.extend_from_slice(data);
        history.pending.len() >= HISTORY_CHUNK_SIZE
    };
    if commit {
        commit_history(store, directory, handle)?;
    }
    Ok(())
}

fn commit_history(store: &Store, directory: &Path, handle: &SessionHandle) -> Result<()> {
    let _commit_guard = handle.history_commit.lock();
    let (sequence, mut pending) = {
        let mut history = handle.history.lock();
        if history.pending.is_empty() {
            return Ok(());
        }
        let sequence = history.next_sequence;
        let pending = std::mem::take(&mut history.pending);
        (sequence, pending)
    };
    let session_id = handle.metadata().id;
    let session_dir = directory.join(session_id.to_string());
    std::fs::create_dir_all(&session_dir)?;
    #[cfg(unix)]
    std::fs::set_permissions(&session_dir, std::fs::Permissions::from_mode(0o700))?;
    let result = (|| -> Result<()> {
        let compressed = zstd::stream::encode_all(pending.as_slice(), 3)?;
        let final_path = session_dir.join(format!("{sequence:020}.zst"));
        let temporary_path = session_dir.join(format!(".{sequence}.{}.tmp", Uuid::new_v4()));
        write_atomic_file(&temporary_path, &final_path, &compressed)?;
        store.insert_history_chunk(&HistoryChunkRef {
            session_id,
            sequence,
            path: final_path,
            compressed_bytes: compressed.len() as u64,
            uncompressed_bytes: pending.len() as u64,
            committed_at: Utc::now(),
        })?;
        for path in store.prune_history(session_id, HISTORY_DISK_LIMIT)? {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            handle.history.lock().next_sequence = sequence + 1;
            Ok(())
        }
        Err(error) => {
            let mut history = handle.history.lock();
            pending.extend(std::mem::take(&mut history.pending));
            history.pending = pending;
            Err(error)
        }
    }
}

fn write_atomic_file(temporary_path: &Path, final_path: &Path, bytes: &[u8]) -> Result<()> {
    use std::fs::OpenOptions;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(temporary_path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    std::fs::rename(temporary_path, final_path)?;
    Ok(())
}

fn read_history_chunk(reference: &HistoryChunkRef, limit: usize) -> Result<Vec<u8>> {
    if reference.uncompressed_bytes as usize > limit {
        bail!("history response size limit reached");
    }
    let file = std::fs::File::open(&reference.path)?;
    let decoder = zstd::Decoder::new(file)?;
    let mut bytes = Vec::with_capacity(reference.uncompressed_bytes as usize);
    decoder.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!("history chunk exceeds declared limit");
    }
    Ok(bytes)
}

#[cfg(all(unix, not(test)))]
fn accept_watchdog(
    listener: std::os::unix::net::UnixListener,
    path: &Path,
) -> Result<Option<std::os::unix::net::UnixStream>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let result = loop {
        match listener.accept() {
            Ok((stream, _)) => break Ok(Some(stream)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    break Err(anyhow!("session launcher did not establish its watchdog"));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => break Err(error.into()),
        }
    };
    let _ = std::fs::remove_file(path);
    result
}

pub fn run_session_launcher() -> Result<()> {
    #[cfg(not(unix))]
    bail!("session launcher is only available on macOS");

    #[cfg(unix)]
    {
        let mut arguments = std::env::args().skip(2);
        anyhow::ensure!(
            arguments.next().as_deref() == Some("--watch-socket"),
            "missing --watch-socket"
        );
        let watch_path = arguments.next().context("missing watchdog socket path")?;
        anyhow::ensure!(
            arguments.next().as_deref() == Some("--"),
            "missing launcher separator"
        );
        let command = arguments.next().context("missing session command")?;
        let command_arguments: Vec<_> = arguments.collect();
        let mut watch = std::os::unix::net::UnixStream::connect(watch_path)
            .context("failed to connect session watchdog")?;
        let mut child = std::process::Command::new(command)
            .args(command_arguments)
            .spawn()
            .context("failed to launch session command")?;
        let (disconnected_tx, disconnected_rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let mut byte = [0_u8; 1];
            let _ = watch.read(&mut byte);
            let _ = disconnected_tx.send(());
        });
        loop {
            if let Some(status) = child.try_wait()? {
                std::process::exit(status.code().unwrap_or(128));
            }
            if disconnected_rx.try_recv().is_ok() {
                unsafe {
                    libc::signal(libc::SIGHUP, libc::SIG_IGN);
                    libc::kill(0, libc::SIGHUP);
                }
                std::thread::sleep(std::time::Duration::from_millis(750));
                unsafe { libc::kill(0, libc::SIGKILL) };
                std::process::exit(137);
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

fn commit_checkpoint(store: &Store, directory: &Path, handle: &SessionHandle) -> Result<()> {
    use std::fs::OpenOptions;
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    let snapshot = handle.snapshot();
    let final_path = directory.join(format!("{}.json", snapshot.session_id));
    let temporary_path = directory.join(format!(".{}.{}.tmp", snapshot.session_id, Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary_path)?;
    serde_json::to_writer(&mut file, &snapshot)?;
    file.flush()?;
    file.sync_all()?;
    std::fs::rename(&temporary_path, &final_path)?;
    store.save_checkpoint(&snapshot, &final_path)?;
    Ok(())
}

fn validate_request(request: &CreateSessionRequest) -> Result<()> {
    if request.name.trim().is_empty() || request.name.len() > 120 {
        bail!("name must contain 1 to 120 bytes");
    }
    if request.command.is_empty() || request.command.len() > 4096 {
        bail!("command must contain 1 to 4096 bytes");
    }
    if request.args.len() > 256
        || request
            .args
            .iter()
            .any(|argument| argument.len() > 64 * 1024)
    {
        bail!("command arguments exceed the allowed size");
    }
    if request.env.len() > 256
        || request
            .env
            .iter()
            .any(|(key, value)| key.is_empty() || key.len() > 1024 || value.len() > 64 * 1024)
    {
        bail!("environment exceeds the allowed size");
    }
    let cols = request.cols.unwrap_or(crate::model::DEFAULT_COLS);
    let rows = request.rows.unwrap_or(crate::model::DEFAULT_ROWS);
    if !(MIN_COLS..=MAX_COLS).contains(&cols) || !(MIN_ROWS..=MAX_ROWS).contains(&rows) {
        bail!("terminal size is outside the allowed range");
    }
    let cwd = PathBuf::from(&request.cwd);
    if !cwd.exists() || !cwd.is_dir() {
        bail!("working directory does not exist or is not a directory");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn rejects_an_invalid_terminal_size() {
        let request = CreateSessionRequest {
            name: "test".into(),
            project_id: None,
            command: "/bin/echo".into(),
            args: vec![],
            cwd: "/tmp".into(),
            env: HashMap::new(),
            agent_integration: None,
            cols: Some(1),
            rows: Some(1),
            history_enabled: true,
        };
        assert!(validate_request(&request).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resize_updates_the_real_pty_parser_metadata_and_store() {
        let store = Store::open_memory().unwrap();
        let manager = SessionManager::new(
            store.clone(),
            Uuid::new_v4(),
            IntegrationManager::for_tests(),
            std::env::temp_dir().join(format!("panestra-test-{}", Uuid::new_v4())),
        );
        let session = manager
            .create(CreateSessionRequest {
                name: "resize".into(),
                project_id: None,
                command: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "trap 'stty size; echo RESIZED; exit 0' WINCH; echo READY; while :; do sleep 0.1; done"
                        .into(),
                ],
                cwd: "/tmp".into(),
                env: HashMap::new(),
                agent_integration: None,
                cols: Some(80),
                rows: Some(24),
                history_enabled: false,
            })
            .unwrap();

        let ready = (0..100).any(|_| {
            if manager
                .snapshot(session.id)
                .is_ok_and(|snapshot| snapshot.contents.contains("READY"))
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            false
        });
        assert!(ready, "test shell did not become ready");

        assert!(manager.resize(session.id, 100, 41, 600, 492).unwrap());
        assert!(!manager.resize(session.id, 100, 41, 600, 492).unwrap());

        let resized = (0..200).any(|_| {
            if manager.snapshot(session.id).is_ok_and(|snapshot| {
                snapshot.cols == 100
                    && snapshot.rows == 41
                    && snapshot.contents.contains("41 100")
                    && snapshot.contents.contains("RESIZED")
            }) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            false
        });
        assert!(resized, "PTY did not report SIGWINCH at its new size");

        let handle = manager.get(session.id).unwrap();
        assert_eq!(
            handle.pty_master.lock().get_size().unwrap(),
            PtySize {
                rows: 41,
                cols: 100,
                pixel_width: 600,
                pixel_height: 492,
            }
        );
        let stored = store
            .list_sessions()
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == session.id)
            .unwrap();
        assert_eq!((stored.cols, stored.rows), (100, 41));
    }

    #[test]
    fn real_pty_captures_japanese_output_without_shell_reinterpretation() {
        let store = Store::open_memory().unwrap();
        let manager = SessionManager::new(
            store,
            Uuid::new_v4(),
            IntegrationManager::for_tests(),
            std::env::temp_dir().join(format!("panestra-test-{}", Uuid::new_v4())),
        );
        let session = manager
            .create(CreateSessionRequest {
                name: "日本語".into(),
                project_id: None,
                command: "/bin/echo".into(),
                args: vec!["hello; printf 改竄されない".into()],
                cwd: "/tmp".into(),
                env: HashMap::new(),
                agent_integration: None,
                cols: None,
                rows: None,
                history_enabled: false,
            })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let snapshot = manager.snapshot(session.id).unwrap();
        assert!(snapshot.contents.contains("hello; printf 改竄されない"));
    }

    #[test]
    fn captures_cell_style_osc7_and_committed_history() {
        let store = Store::open_memory().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(
            store,
            Uuid::new_v4(),
            IntegrationManager::for_tests(),
            data_dir.path().join("checkpoints"),
        );
        let session = manager
            .create(CreateSessionRequest {
                name: "styled".into(),
                project_id: None,
                command: "/usr/bin/printf".into(),
                args: vec!["\x1b]7;file://localhost/tmp\x07\x1b[31m赤\x1b[0m\n".into()],
                cwd: "/tmp".into(),
                env: HashMap::new(),
                agent_integration: None,
                cols: None,
                rows: None,
                history_enabled: true,
            })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(250));
        let snapshot = manager.snapshot(session.id).unwrap();
        assert!(
            snapshot
                .styled_cells
                .iter()
                .any(|cell| cell.2 == "赤" && cell.4.is_some())
        );
        let expected_cwd = std::fs::canonicalize("/tmp").unwrap();
        assert_eq!(
            manager
                .get(session.id)
                .unwrap()
                .metadata()
                .current_cwd
                .as_deref(),
            Some(expected_cwd.to_string_lossy().as_ref())
        );
        let history = manager.history_page(session.id, None, 10).unwrap();
        assert!(history.chunks.iter().any(|chunk| chunk.text.contains('赤')));
    }

    #[test]
    fn forced_termination_is_recorded_as_killed() {
        let store = Store::open_memory().unwrap();
        let manager = SessionManager::new(
            store,
            Uuid::new_v4(),
            IntegrationManager::for_tests(),
            std::env::temp_dir().join(format!("panestra-test-{}", Uuid::new_v4())),
        );
        let session = manager
            .create(CreateSessionRequest {
                name: "force".into(),
                project_id: None,
                command: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "trap '' TERM; while :; do sleep 1; done".into(),
                ],
                cwd: "/tmp".into(),
                env: HashMap::new(),
                agent_integration: None,
                cols: None,
                rows: None,
                history_enabled: false,
            })
            .unwrap();
        manager.terminate(session.id, true).unwrap();
        for _ in 0..100 {
            let metadata = manager.get(session.id).unwrap().metadata();
            if metadata.process_state == ProcessState::Killed {
                assert_eq!(metadata.exit_reason, Some(ExitReason::Forced));
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("forced process did not reach killed state");
    }
}
