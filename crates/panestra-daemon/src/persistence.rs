use std::{fs, path::Path, sync::Arc};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::model::{
    ActionContext, AgentIntegration, AgentState, ExitReason, ProcessState, Project, ScreenSnapshot,
    Session,
};

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
            set_owner_only(parent)?;
        }
        let connection =
            Connection::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        let store = Self {
            connection: Arc::new(Mutex::new(connection)),
        };
        store.migrate()?;
        set_owner_only(path)?;
        set_sqlite_sidecars_owner_only(path)?;
        Ok(store)
    }

    #[cfg(test)]
    pub fn open_memory() -> Result<Self> {
        let store = Self {
            connection: Arc::new(Mutex::new(Connection::open_in_memory()?)),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.connection.lock().execute_batch(
            r#"
            PRAGMA user_version = 1;
            CREATE TABLE IF NOT EXISTS sessions (
              id TEXT PRIMARY KEY,
              project_id TEXT,
              name TEXT NOT NULL,
              command TEXT NOT NULL,
              args_json TEXT NOT NULL,
              launch_cwd TEXT NOT NULL,
              current_cwd TEXT,
              agent_integration TEXT,
              agent_state TEXT,
              process_state TEXT NOT NULL,
              pid INTEGER,
              cols INTEGER NOT NULL,
              rows INTEGER NOT NULL,
              daemon_epoch TEXT NOT NULL,
              session_generation TEXT NOT NULL,
              history_enabled INTEGER NOT NULL,
              created_at TEXT NOT NULL,
              last_activity_at TEXT,
              exited_at TEXT,
              exit_code INTEGER,
              exit_reason TEXT,
              current_action_id TEXT
            );
            CREATE TABLE IF NOT EXISTS action_contexts (
              id TEXT PRIMARY KEY,
              schema_version INTEGER NOT NULL,
              session_id TEXT NOT NULL,
              task_summary TEXT NOT NULL,
              repository_path TEXT NOT NULL,
              git_branch TEXT,
              git_working_tree_root TEXT,
              accepted_at TEXT NOT NULL,
              FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_actions_session_time
              ON action_contexts(session_id, accepted_at DESC);
            CREATE TABLE IF NOT EXISTS projects (
              id TEXT PRIMARY KEY,
              name TEXT NOT NULL,
              root_path TEXT NOT NULL,
              color TEXT,
              tags_json TEXT NOT NULL,
              repository_url TEXT,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS checkpoints (
              session_id TEXT PRIMARY KEY,
              snapshot_epoch TEXT NOT NULL,
              sequence_number INTEGER NOT NULL,
              path TEXT NOT NULL,
              committed_at TEXT NOT NULL,
              FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS history_chunks (
              session_id TEXT NOT NULL,
              sequence_number INTEGER NOT NULL,
              path TEXT NOT NULL,
              compressed_bytes INTEGER NOT NULL,
              uncompressed_bytes INTEGER NOT NULL,
              committed_at TEXT NOT NULL,
              PRIMARY KEY(session_id, sequence_number),
              FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_history_session_sequence
              ON history_chunks(session_id, sequence_number DESC);
            "#,
        )?;
        Ok(())
    }

    pub fn mark_interrupted_sessions(&self) -> Result<usize> {
        Ok(self.connection.lock().execute(
            "UPDATE sessions SET process_state='exited', exit_reason='daemon_terminated', exited_at=?1, pid=NULL WHERE process_state IN ('starting','running')",
            [chrono::Utc::now().to_rfc3339()],
        )?)
    }

    pub fn upsert_session(&self, session: &Session) -> Result<()> {
        self.connection.lock().execute(
            r#"INSERT INTO sessions (
              id, project_id, name, command, args_json, launch_cwd, current_cwd,
              agent_integration, agent_state, process_state, pid, cols, rows,
              daemon_epoch, session_generation, history_enabled, created_at,
              last_activity_at, exited_at, exit_code, exit_reason, current_action_id
            ) VALUES (
              ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22
            ) ON CONFLICT(id) DO UPDATE SET
              current_cwd=excluded.current_cwd,
              agent_state=excluded.agent_state,
              process_state=excluded.process_state,
              pid=excluded.pid,
              cols=excluded.cols,
              rows=excluded.rows,
              last_activity_at=excluded.last_activity_at,
              exited_at=excluded.exited_at,
              exit_code=excluded.exit_code,
              exit_reason=excluded.exit_reason,
              current_action_id=excluded.current_action_id"#,
            params![
                session.id.to_string(),
                session.project_id.map(|value| value.to_string()),
                session.name,
                session.command,
                serde_json::to_string(&session.args)?,
                session.launch_cwd,
                session.current_cwd,
                enum_json(&session.agent_integration)?,
                enum_json(&session.agent_state)?,
                enum_value(session.process_state)?,
                session.pid,
                session.cols,
                session.rows,
                session.daemon_epoch.to_string(),
                session.session_generation.to_string(),
                session.history_enabled,
                session.created_at.to_rfc3339(),
                session.last_activity_at.map(|value| value.to_rfc3339()),
                session.exited_at.map(|value| value.to_rfc3339()),
                session.exit_code,
                enum_json(&session.exit_reason)?,
                session.current_action_id.map(|value| value.to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT id,project_id,name,command,args_json,launch_cwd,current_cwd,agent_integration,agent_state,current_action_id,process_state,pid,cols,rows,daemon_epoch,session_generation,history_enabled,created_at,last_activity_at,exited_at,exit_code,exit_reason FROM sessions ORDER BY created_at",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Session {
                id: parse_uuid(row.get::<_, String>(0)?)?,
                project_id: parse_optional_uuid(row.get(1)?)?,
                name: row.get(2)?,
                command: row.get(3)?,
                args: parse_json(row.get::<_, String>(4)?)?,
                launch_cwd: row.get(5)?,
                current_cwd: row.get(6)?,
                agent_integration: parse_optional_enum::<AgentIntegration>(row.get(7)?)?,
                agent_state: parse_optional_enum::<AgentState>(row.get(8)?)?,
                current_action_id: parse_optional_uuid(row.get(9)?)?,
                process_state: parse_enum::<ProcessState>(row.get(10)?)?,
                pid: row.get(11)?,
                cols: row.get(12)?,
                rows: row.get(13)?,
                daemon_epoch: parse_uuid(row.get::<_, String>(14)?)?,
                session_generation: parse_uuid(row.get::<_, String>(15)?)?,
                history_enabled: row.get(16)?,
                created_at: parse_time(row.get::<_, String>(17)?)?,
                last_activity_at: parse_optional_time(row.get(18)?)?,
                exited_at: parse_optional_time(row.get(19)?)?,
                exit_code: row.get(20)?,
                exit_reason: parse_optional_enum::<ExitReason>(row.get(21)?)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn insert_action(&self, action: &ActionContext) -> Result<()> {
        let connection = self.connection.lock();
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO action_contexts(id,schema_version,session_id,task_summary,repository_path,git_branch,git_working_tree_root,accepted_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![action.id.to_string(), action.schema_version, action.session_id.to_string(), action.task_summary, action.repository_path, action.git_branch, action.git_working_tree_root, action.accepted_at.to_rfc3339()],
        )?;
        transaction.execute(
            "UPDATE sessions SET current_action_id=?1 WHERE id=?2",
            params![action.id.to_string(), action.session_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn list_actions(&self, session_id: Uuid, limit: usize) -> Result<Vec<ActionContext>> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT id,schema_version,session_id,task_summary,repository_path,git_branch,git_working_tree_root,accepted_at FROM action_contexts WHERE session_id=?1 ORDER BY accepted_at DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![
                session_id.to_string(),
                i64::try_from(limit.clamp(1, 100)).unwrap_or(100)
            ],
            |row| {
                Ok(ActionContext {
                    id: parse_uuid(row.get::<_, String>(0)?)?,
                    schema_version: row.get(1)?,
                    session_id: parse_uuid(row.get::<_, String>(2)?)?,
                    task_summary: row.get(3)?,
                    repository_path: row.get(4)?,
                    git_branch: row.get(5)?,
                    git_working_tree_root: row.get(6)?,
                    accepted_at: parse_time(row.get::<_, String>(7)?)?,
                })
            },
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn current_action(&self, session_id: Uuid) -> Result<Option<ActionContext>> {
        let current: Option<String> = self
            .connection
            .lock()
            .query_row(
                "SELECT current_action_id FROM sessions WHERE id=?1",
                [session_id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let Some(id) = current else { return Ok(None) };
        self.action_by_id(parse_uuid(id)?)
    }

    fn action_by_id(&self, id: Uuid) -> Result<Option<ActionContext>> {
        self.connection
            .lock()
            .query_row(
                "SELECT id,schema_version,session_id,task_summary,repository_path,git_branch,git_working_tree_root,accepted_at FROM action_contexts WHERE id=?1",
                [id.to_string()],
                |row| {
                    Ok(ActionContext {
                        id: parse_uuid(row.get(0)?)?,
                        schema_version: row.get(1)?,
                        session_id: parse_uuid(row.get(2)?)?,
                        task_summary: row.get(3)?,
                        repository_path: row.get(4)?,
                        git_branch: row.get(5)?,
                        git_working_tree_root: row.get(6)?,
                        accepted_at: parse_time(row.get(7)?)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn clear_current_action(&self, session_id: Uuid) -> Result<()> {
        self.connection.lock().execute(
            "UPDATE sessions SET current_action_id=NULL WHERE id=?1",
            [session_id.to_string()],
        )?;
        Ok(())
    }

    pub fn delete_session(
        &self,
        session_id: Uuid,
        include_history: bool,
    ) -> Result<Vec<std::path::PathBuf>> {
        let connection = self.connection.lock();
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
            [session_id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        if include_history {
            if let Some(path) = connection
                .query_row(
                    "SELECT path FROM checkpoints WHERE session_id=?1",
                    [session_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                paths.push(path.into());
            }
            let mut statement =
                connection.prepare("SELECT path FROM history_chunks WHERE session_id=?1")?;
            paths.extend(
                statement
                    .query_map([session_id.to_string()], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .into_iter()
                    .map(Into::into),
            );
        }
        connection.execute("DELETE FROM sessions WHERE id=?1", [session_id.to_string()])?;
        Ok(paths)
    }

    pub fn save_checkpoint(&self, snapshot: &ScreenSnapshot, path: &Path) -> Result<()> {
        self.connection.lock().execute(
            r#"INSERT INTO checkpoints(session_id,snapshot_epoch,sequence_number,path,committed_at)
               VALUES (?1,?2,?3,?4,?5)
               ON CONFLICT(session_id) DO UPDATE SET
                 snapshot_epoch=excluded.snapshot_epoch,
                 sequence_number=excluded.sequence_number,
                 path=excluded.path,
                 committed_at=excluded.committed_at"#,
            params![
                snapshot.session_id.to_string(),
                snapshot.snapshot_epoch.to_string(),
                i64::try_from(snapshot.sequence_number).unwrap_or(i64::MAX),
                path.to_string_lossy(),
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn load_checkpoint(&self, session_id: Uuid) -> Result<Option<ScreenSnapshot>> {
        let path: Option<String> = self
            .connection
            .lock()
            .query_row(
                "SELECT path FROM checkpoints WHERE session_id=?1",
                [session_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(path) = path else { return Ok(None) };
        let bytes = fs::read(path)?;
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    pub fn upsert_project(&self, project: &Project) -> Result<()> {
        self.connection.lock().execute(
            r#"INSERT INTO projects(id,name,root_path,color,tags_json,repository_url,created_at,updated_at)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
               ON CONFLICT(id) DO UPDATE SET name=excluded.name,root_path=excluded.root_path,
                 color=excluded.color,tags_json=excluded.tags_json,repository_url=excluded.repository_url,
                 updated_at=excluded.updated_at"#,
            params![project.id.to_string(), project.name, project.root_path, project.color,
                serde_json::to_string(&project.tags)?, project.repository_url,
                project.created_at.to_rfc3339(), project.updated_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT id,name,root_path,color,tags_json,repository_url,created_at,updated_at FROM projects ORDER BY name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Project {
                id: parse_uuid(row.get(0)?)?,
                name: row.get(1)?,
                root_path: row.get(2)?,
                color: row.get(3)?,
                tags: parse_json(row.get(4)?)?,
                repository_url: row.get(5)?,
                created_at: parse_time(row.get(6)?)?,
                updated_at: parse_time(row.get(7)?)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn project_exists(&self, id: Uuid) -> Result<bool> {
        Ok(self.connection.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [id.to_string()],
            |row| row.get(0),
        )?)
    }

    pub fn delete_project(&self, id: Uuid) -> Result<bool> {
        Ok(self
            .connection
            .lock()
            .execute("DELETE FROM projects WHERE id=?1", [id.to_string()])?
            > 0)
    }

    pub fn insert_history_chunk(&self, chunk: &HistoryChunkRef) -> Result<()> {
        self.connection.lock().execute(
            "INSERT INTO history_chunks(session_id,sequence_number,path,compressed_bytes,uncompressed_bytes,committed_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                chunk.session_id.to_string(),
                i64::try_from(chunk.sequence).unwrap_or(i64::MAX),
                chunk.path.to_string_lossy(),
                i64::try_from(chunk.compressed_bytes).unwrap_or(i64::MAX),
                i64::try_from(chunk.uncompressed_bytes).unwrap_or(i64::MAX),
                chunk.committed_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_history_chunks(
        &self,
        session_id: Uuid,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<HistoryChunkRef>> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT sequence_number,path,compressed_bytes,uncompressed_bytes,committed_at FROM history_chunks WHERE session_id=?1 AND sequence_number < ?2 ORDER BY sequence_number DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                session_id.to_string(),
                i64::try_from(before.unwrap_or(u64::MAX)).unwrap_or(i64::MAX),
                i64::try_from(limit).unwrap_or(i64::MAX),
            ],
            |row| {
                Ok(HistoryChunkRef {
                    session_id,
                    sequence: row.get::<_, i64>(0)?.try_into().unwrap_or(0),
                    path: row.get::<_, String>(1)?.into(),
                    compressed_bytes: row.get::<_, i64>(2)?.try_into().unwrap_or(0),
                    uncompressed_bytes: row.get::<_, i64>(3)?.try_into().unwrap_or(0),
                    committed_at: parse_time(row.get(4)?)?,
                })
            },
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn prune_history(
        &self,
        session_id: Uuid,
        max_compressed_bytes: u64,
    ) -> Result<Vec<std::path::PathBuf>> {
        let connection = self.connection.lock();
        let total: i64 = connection.query_row(
            "SELECT COALESCE(SUM(compressed_bytes),0) FROM history_chunks WHERE session_id=?1",
            [session_id.to_string()],
            |row| row.get(0),
        )?;
        let mut excess = u64::try_from(total)
            .unwrap_or(0)
            .saturating_sub(max_compressed_bytes);
        if excess == 0 {
            return Ok(Vec::new());
        }
        let mut statement = connection.prepare(
            "SELECT sequence_number,path,compressed_bytes FROM history_chunks WHERE session_id=?1 ORDER BY sequence_number ASC",
        )?;
        let candidates = statement
            .query_map([session_id.to_string()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let transaction = connection.unchecked_transaction()?;
        let mut paths = Vec::new();
        for (sequence, path, size) in candidates {
            if excess == 0 {
                break;
            }
            transaction.execute(
                "DELETE FROM history_chunks WHERE session_id=?1 AND sequence_number=?2",
                params![session_id.to_string(), sequence],
            )?;
            paths.push(path.into());
            excess = excess.saturating_sub(u64::try_from(size).unwrap_or(0));
        }
        transaction.commit()?;
        Ok(paths)
    }
}

#[derive(Debug, Clone)]
pub struct HistoryChunkRef {
    pub session_id: Uuid,
    pub sequence: u64,
    pub path: std::path::PathBuf,
    pub compressed_bytes: u64,
    pub uncompressed_bytes: u64,
    pub committed_at: chrono::DateTime<chrono::Utc>,
}

fn enum_value<T: serde::Serialize>(value: T) -> Result<String> {
    let json = serde_json::to_value(value)?;
    Ok(json.as_str().unwrap_or_default().to_owned())
}

fn enum_json<T: serde::Serialize>(value: &Option<T>) -> Result<Option<String>> {
    value.as_ref().map(enum_value).transpose()
}

fn parse_enum<T: serde::de::DeserializeOwned>(value: String) -> rusqlite::Result<T> {
    parse_json(format!("\"{value}\""))
}

fn parse_optional_enum<T: serde::de::DeserializeOwned>(
    value: Option<String>,
) -> rusqlite::Result<Option<T>> {
    value.map(parse_enum).transpose()
}

fn parse_json<T: serde::de::DeserializeOwned>(value: String) -> rusqlite::Result<T> {
    serde_json::from_str(&value).map_err(to_sql_error)
}

fn parse_uuid(value: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&value).map_err(to_sql_error)
}

fn parse_optional_uuid(value: Option<String>) -> rusqlite::Result<Option<Uuid>> {
    value.map(parse_uuid).transpose()
}

fn parse_time(value: String) -> rusqlite::Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&chrono::Utc))
        .map_err(to_sql_error)
}

fn parse_optional_time(
    value: Option<String>,
) -> rusqlite::Result<Option<chrono::DateTime<chrono::Utc>>> {
    value.map(parse_time).transpose()
}

fn to_sql_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if path.is_dir() { 0o700 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(unix)]
fn set_sqlite_sidecars_owner_only(path: &Path) -> Result<()> {
    use std::ffi::OsString;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar: OsString = path.as_os_str().to_owned();
        sidecar.push(suffix);
        let sidecar = Path::new(&sidecar);
        if sidecar.exists() {
            set_owner_only(sidecar)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn set_sqlite_sidecars_owner_only(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn session() -> Session {
        Session {
            id: Uuid::new_v4(),
            project_id: None,
            name: "test".into(),
            command: "/bin/zsh".into(),
            args: vec!["-l".into()],
            launch_cwd: "/tmp".into(),
            current_cwd: None,
            agent_integration: None,
            agent_state: None,
            current_action_id: None,
            process_state: ProcessState::Running,
            pid: Some(42),
            cols: 120,
            rows: 36,
            daemon_epoch: Uuid::new_v4(),
            session_generation: Uuid::new_v4(),
            history_enabled: true,
            created_at: Utc::now(),
            last_activity_at: None,
            exited_at: None,
            exit_code: None,
            exit_reason: None,
        }
    }

    #[test]
    fn sessions_round_trip_and_interrupted_are_finalized() {
        let store = Store::open_memory().unwrap();
        let expected = session();
        store.upsert_session(&expected).unwrap();
        assert_eq!(store.mark_interrupted_sessions().unwrap(), 1);
        let actual = store.list_sessions().unwrap().pop().unwrap();
        assert_eq!(actual.id, expected.id);
        assert_eq!(actual.process_state, ProcessState::Exited);
        assert_eq!(actual.exit_reason, Some(ExitReason::DaemonTerminated));
        assert_eq!(actual.pid, None);
    }
}
