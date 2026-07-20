use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};
use parking_lot::RwLock;
use semver::{Version, VersionReq};
use serde_json::json;
use uuid::Uuid;

use crate::{auth::random_token, model::AgentIntegration};

const CODEX_SUPPORTED: &str = ">=0.144.0, <0.145.0";
const CLAUDE_SUPPORTED: &str = ">=2.1.211, <2.2.0";
const MCP_TOKEN_ENV: &str = "PANESTRA_MCP_CREDENTIAL";
const HOOK_TOKEN_ENV: &str = "PANESTRA_HOOK_CREDENTIAL";
const HOOK_ENDPOINT_ENV: &str = "PANESTRA_HOOK_ENDPOINT";
const SESSION_ID_ENV: &str = "PANESTRA_SESSION_ID";
const INTEGRATION_BASE_URL_ENV: &str = "PANESTRA_INTEGRATION_BASE_URL";
const ORIGINAL_PATH_ENV: &str = "PANESTRA_ORIGINAL_PATH";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialScope {
    Fixed(AgentIntegration),
    Terminal,
}

#[derive(Debug, Clone)]
struct Credentials {
    scope: CredentialScope,
    hook_token: String,
    mcp_token: String,
}

#[derive(Debug)]
pub struct PreparedLaunch {
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

#[derive(Clone)]
pub struct IntegrationManager {
    base_url: String,
    hook_executable: String,
    shim_dir: PathBuf,
    credentials: Arc<RwLock<HashMap<Uuid, Credentials>>>,
}

impl IntegrationManager {
    pub fn new(base_url: String, data_dir: &Path) -> Result<Self> {
        let executable = std::env::current_exe().context("cannot locate Panestra executable")?;
        let shim_dir = data_dir.join("agent-shims");
        install_agent_shims(&shim_dir, &executable)?;
        Ok(Self {
            base_url,
            hook_executable: executable.to_string_lossy().into_owned(),
            shim_dir,
            credentials: Arc::default(),
        })
    }

    #[cfg(test)]
    pub fn for_tests() -> Self {
        Self {
            base_url: "http://127.0.0.1:8371".into(),
            hook_executable: "/tmp/panestra-daemon".into(),
            shim_dir: PathBuf::from("/tmp/panestra-agent-shims"),
            credentials: Arc::default(),
        }
    }

    pub fn prepare(
        &self,
        session_id: Uuid,
        provider: AgentIntegration,
        command: &str,
        user_args: &[String],
    ) -> Result<PreparedLaunch> {
        verify_version(provider, command)?;
        let credentials = self.issue_credentials(CredentialScope::Fixed(provider));
        let hook_url = format!("{}/api/integrations/{session_id}/hook", self.base_url);
        let mcp_url = format!("{}/api/integrations/{session_id}/mcp", self.base_url);
        let args = self.integration_args(provider, &hook_url, &mcp_url, user_args)?;
        let env = HashMap::from([
            (HOOK_TOKEN_ENV.into(), credentials.hook_token.clone()),
            (HOOK_ENDPOINT_ENV.into(), hook_url),
            (MCP_TOKEN_ENV.into(), credentials.mcp_token.clone()),
        ]);
        self.credentials.write().insert(session_id, credentials);
        Ok(PreparedLaunch { args, env })
    }

    pub fn prepare_terminal(
        &self,
        session_id: Uuid,
        original_path: &str,
    ) -> Result<PreparedLaunch> {
        let credentials = self.issue_credentials(CredentialScope::Terminal);
        let mut paths = vec![self.shim_dir.clone()];
        paths.extend(std::env::split_paths(OsStr::new(original_path)));
        let path = std::env::join_paths(paths)
            .context("terminal PATH cannot contain the Panestra agent shim directory")?
            .to_string_lossy()
            .into_owned();
        let env = HashMap::from([
            ("PATH".into(), path),
            (ORIGINAL_PATH_ENV.into(), original_path.to_owned()),
            (SESSION_ID_ENV.into(), session_id.to_string()),
            (
                INTEGRATION_BASE_URL_ENV.into(),
                format!("{}/api/integrations/{session_id}", self.base_url),
            ),
            (HOOK_TOKEN_ENV.into(), credentials.hook_token.clone()),
            (MCP_TOKEN_ENV.into(), credentials.mcp_token.clone()),
        ]);
        self.credentials.write().insert(session_id, credentials);
        Ok(PreparedLaunch {
            args: Vec::new(),
            env,
        })
    }

    pub fn revoke(&self, session_id: Uuid) {
        self.credentials.write().remove(&session_id);
    }

    pub fn authorize_hook(&self, session_id: Uuid, token: &str) -> Option<AgentIntegration> {
        self.credentials
            .read()
            .get(&session_id)
            .filter(|credentials| credentials.hook_token == token)
            .and_then(|credentials| match credentials.scope {
                CredentialScope::Fixed(provider) => Some(provider),
                CredentialScope::Terminal => None,
            })
    }

    pub fn authorize_mcp(&self, session_id: Uuid, token: &str) -> Option<AgentIntegration> {
        self.credentials
            .read()
            .get(&session_id)
            .filter(|credentials| credentials.mcp_token == token)
            .and_then(|credentials| match credentials.scope {
                CredentialScope::Fixed(provider) => Some(provider),
                CredentialScope::Terminal => None,
            })
    }

    pub fn authorize_terminal_hook(&self, session_id: Uuid, token: &str) -> bool {
        self.credentials
            .read()
            .get(&session_id)
            .is_some_and(|credentials| {
                credentials.hook_token == token && credentials.scope == CredentialScope::Terminal
            })
    }

    pub fn authorize_terminal_mcp(&self, session_id: Uuid, token: &str) -> bool {
        self.credentials
            .read()
            .get(&session_id)
            .is_some_and(|credentials| {
                credentials.mcp_token == token && credentials.scope == CredentialScope::Terminal
            })
    }

    fn issue_credentials(&self, scope: CredentialScope) -> Credentials {
        Credentials {
            scope,
            hook_token: random_token(),
            mcp_token: random_token(),
        }
    }

    fn integration_args(
        &self,
        provider: AgentIntegration,
        hook_url: &str,
        mcp_url: &str,
        user_args: &[String],
    ) -> Result<Vec<String>> {
        match provider {
            AgentIntegration::Codex => self.codex_args(mcp_url, user_args),
            AgentIntegration::Claude => self.claude_args(hook_url, mcp_url, user_args),
        }
    }

    fn codex_args(&self, mcp_url: &str, user_args: &[String]) -> Result<Vec<String>> {
        codex_args(&self.hook_executable, mcp_url, user_args)
    }

    fn claude_args(
        &self,
        hook_url: &str,
        mcp_url: &str,
        user_args: &[String],
    ) -> Result<Vec<String>> {
        claude_args(&self.hook_executable, hook_url, mcp_url, user_args)
    }
}

pub fn shim_provider() -> Option<AgentIntegration> {
    let executable = std::env::args_os().next()?;
    match Path::new(&executable).file_name()?.to_str()? {
        "codex" => Some(AgentIntegration::Codex),
        "claude" => Some(AgentIntegration::Claude),
        _ => None,
    }
}

pub fn run_agent_shim(provider: AgentIntegration) -> Result<()> {
    let original_path = std::env::var_os(ORIGINAL_PATH_ENV)
        .context("Panestra agent shim was started without its original PATH")?;
    let executable = resolve_executable(provider_name(provider), &original_path)
        .with_context(|| format!("{}: command not found", provider_name(provider)))?;
    std::env::var(SESSION_ID_ENV)
        .context("missing Panestra session id")?
        .parse::<Uuid>()
        .context("invalid Panestra session id")?;
    let base_url =
        std::env::var(INTEGRATION_BASE_URL_ENV).context("missing Panestra integration endpoint")?;
    let hook_token = std::env::var(HOOK_TOKEN_ENV).context("missing hook credential")?;
    let invocation_id = Uuid::new_v4();
    let provider_name = provider_name(provider);
    let hook_url = format!("{base_url}/hook/{provider_name}/{invocation_id}");
    let mcp_url = format!("{base_url}/mcp/{provider_name}/{invocation_id}");
    let user_args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let integration = verify_version(provider, &executable.to_string_lossy()).and_then(|_| {
        let string_args = user_args
            .iter()
            .map(|argument| {
                argument
                    .to_str()
                    .map(ToOwned::to_owned)
                    .context("agent arguments must be valid UTF-8 for state integration")
            })
            .collect::<Result<Vec<_>>>()?;
        match provider {
            AgentIntegration::Codex => codex_args(
                &std::env::current_exe()?.to_string_lossy(),
                &mcp_url,
                &string_args,
            ),
            AgentIntegration::Claude => claude_args(
                &std::env::current_exe()?.to_string_lossy(),
                &hook_url,
                &mcp_url,
                &string_args,
            ),
        }
    });
    let integration_supported = integration.is_ok();
    let runtime_url = format!("{base_url}/runtime/{provider_name}/{invocation_id}");
    let notification = ureq::post(&runtime_url)
        .header("Authorization", &format!("Bearer {hook_token}"))
        .send_json(json!({
            "pid": std::process::id(),
            "integrationSupported": integration_supported,
        }));

    let mut command = Command::new(&executable);
    if notification.is_ok() {
        if let Ok(args) = integration {
            command.args(args);
            command.env(HOOK_ENDPOINT_ENV, hook_url);
        } else {
            eprintln!(
                "Panestra: {}の状態連携に対応していないため、通常モードで起動します。",
                provider_display_name(provider)
            );
            command.args(&user_args);
        }
    } else {
        command.args(&user_args);
    }

    exec_command(command)
}

pub fn forward_hook_event() -> Result<()> {
    use std::io::Read;
    const MAX_HOOK_INPUT: usize = 1024 * 1024;
    let endpoint = std::env::var(HOOK_ENDPOINT_ENV).context("missing hook endpoint")?;
    let credential = std::env::var(HOOK_TOKEN_ENV).context("missing hook credential")?;
    let mut input = Vec::new();
    std::io::stdin()
        .take((MAX_HOOK_INPUT + 1) as u64)
        .read_to_end(&mut input)?;
    if input.len() > MAX_HOOK_INPUT {
        bail!("hook input exceeds allowed size");
    }
    let payload: serde_json::Value = serde_json::from_slice(&input)?;
    ureq::post(&endpoint)
        .header("Authorization", &format!("Bearer {credential}"))
        .send_json(&payload)
        .context("failed to forward hook event")?;
    Ok(())
}

fn install_agent_shims(directory: &Path, executable: &Path) -> Result<()> {
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        for name in ["codex", "claude"] {
            let path = directory.join(name);
            if path.exists() || path.symlink_metadata().is_ok() {
                let metadata = path.symlink_metadata()?;
                if metadata.file_type().is_dir() {
                    bail!("agent shim path is a directory: {}", path.display());
                }
                fs::remove_file(&path)?;
            }
            symlink(executable, path)?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (directory, executable);
        bail!("automatic terminal agent detection requires Unix")
    }
}

fn resolve_executable(name: &str, path: &OsStr) -> Result<PathBuf> {
    for directory in std::env::split_paths(path) {
        let candidate = directory.join(name);
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(anyhow!("executable was not found in the original PATH"))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(unix)]
fn exec_command(mut command: Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    Err(command.exec()).context("failed to execute the agent CLI")
}

#[cfg(not(unix))]
fn exec_command(mut command: Command) -> Result<()> {
    let status = command.status()?;
    std::process::exit(status.code().unwrap_or(1));
}

fn verify_version(provider: AgentIntegration, command: &str) -> Result<()> {
    let output = Command::new(command)
        .arg("--version")
        .output()
        .with_context(|| format!("failed to run {command} --version"))?;
    if !output.status.success() {
        bail!("{command} --version failed");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text
        .split_whitespace()
        .find_map(|part| Version::parse(part.trim_start_matches('v')).ok())
        .with_context(|| format!("could not parse provider version from {text:?}"))?;
    let requirement = VersionReq::parse(match provider {
        AgentIntegration::Codex => CODEX_SUPPORTED,
        AgentIntegration::Claude => CLAUDE_SUPPORTED,
    })?;
    if !requirement.matches(&version) {
        bail!(
            "unsupported {} version {version}; supported range is {requirement}",
            provider_display_name(provider)
        );
    }
    Ok(())
}

fn codex_args(hook_executable: &str, mcp_url: &str, user_args: &[String]) -> Result<Vec<String>> {
    let command = toml_string(&format!("{hook_executable} hook-event"))?;
    let handler = format!("[{{hooks=[{{type=\"command\",command={command},timeout=5}}]}}]");
    let mcp_url = toml_string(mcp_url)?;
    let mcp = format!(
        "{{url={mcp_url},bearer_token_env_var=\"{MCP_TOKEN_ENV}\",required=false,enabled_tools=[\"report_action_start\"],default_tools_approval_mode=\"auto\"}}"
    );
    let mut args = Vec::new();
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PermissionRequest",
        "Stop",
    ] {
        args.extend(["-c".into(), format!("hooks.{event}={handler}")]);
    }
    args.extend(["-c".into(), format!("mcp_servers.panestra={mcp}")]);
    args.extend_from_slice(user_args);
    Ok(args)
}

fn claude_args(
    hook_executable: &str,
    hook_url: &str,
    mcp_url: &str,
    user_args: &[String],
) -> Result<Vec<String>> {
    let command_hook = json!({
        "type": "command",
        "command": hook_executable,
        "args": ["hook-event"],
        "timeout": 5
    });
    let settings = json!({
        "hooks": {
            "SessionStart": [{ "hooks": [command_hook.clone()] }],
            "UserPromptSubmit": [{ "hooks": [command_hook.clone()] }],
            "PermissionRequest": [{ "hooks": [command_hook.clone()] }],
            "Stop": [{ "hooks": [command_hook] }]
        }
    });
    let mcp = json!({
        "mcpServers": {
            "panestra": {
                "type": "http",
                "url": mcp_url,
                "headers": { "Authorization": format!("Bearer ${{{MCP_TOKEN_ENV}}}") }
            }
        }
    });
    let mut args = vec![
        "--settings".into(),
        serde_json::to_string(&settings)?,
        "--mcp-config".into(),
        serde_json::to_string(&mcp)?,
    ];
    args.extend_from_slice(user_args);
    let _ = hook_url;
    Ok(args)
}

fn provider_name(provider: AgentIntegration) -> &'static str {
    match provider {
        AgentIntegration::Codex => "codex",
        AgentIntegration::Claude => "claude",
    }
}

fn provider_display_name(provider: AgentIntegration) -> &'static str {
    match provider {
        AgentIntegration::Codex => "Codex CLI",
        AgentIntegration::Claude => "Claude Code",
    }
}

fn toml_string(value: &str) -> Result<String> {
    serde_json::to_string(value).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_configuration_is_session_only_and_contains_all_state_hooks() {
        let manager = IntegrationManager::for_tests();
        let args = manager
            .claude_args("http://localhost/hook", "http://localhost/mcp", &[])
            .unwrap();
        let settings: serde_json::Value = serde_json::from_str(&args[1]).unwrap();
        for event in [
            "SessionStart",
            "UserPromptSubmit",
            "PermissionRequest",
            "Stop",
        ] {
            assert!(settings["hooks"][event].is_array());
        }
        assert_eq!(args[0], "--settings");
        assert!(args.contains(&"--mcp-config".to_owned()));
    }

    #[test]
    fn codex_configuration_does_not_bypass_hook_trust() {
        let manager = IntegrationManager::for_tests();
        let args = manager.codex_args("http://localhost/mcp", &[]).unwrap();
        assert!(!args.iter().any(|arg| arg.contains("bypass-hook-trust")));
        for event in [
            "SessionStart",
            "UserPromptSubmit",
            "PermissionRequest",
            "Stop",
        ] {
            assert!(
                args.iter()
                    .any(|arg| arg.starts_with(&format!("hooks.{event}=")))
            );
        }
    }

    #[test]
    fn terminal_credentials_do_not_authorize_fixed_routes() {
        let manager = IntegrationManager::for_tests();
        let session_id = Uuid::new_v4();
        let prepared = manager
            .prepare_terminal(session_id, "/usr/bin:/bin")
            .unwrap();
        let hook_token = &prepared.env[HOOK_TOKEN_ENV];
        let mcp_token = &prepared.env[MCP_TOKEN_ENV];
        assert!(manager.authorize_hook(session_id, hook_token).is_none());
        assert!(manager.authorize_mcp(session_id, mcp_token).is_none());
        assert!(manager.authorize_terminal_hook(session_id, hook_token));
        assert!(manager.authorize_terminal_mcp(session_id, mcp_token));
        assert!(prepared.env["PATH"].starts_with("/tmp/panestra-agent-shims:"));
    }

    #[cfg(unix)]
    #[test]
    fn resolves_only_executable_files_from_the_original_path() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let command = directory.path().join("codex");
        fs::write(&command, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            resolve_executable("codex", directory.path().as_os_str()).unwrap(),
            command
        );
        assert!(resolve_executable("claude", directory.path().as_os_str()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn installs_both_agent_shims_for_the_daemon_executable() {
        let directory = tempfile::tempdir().unwrap();
        let executable = std::env::current_exe().unwrap();
        install_agent_shims(directory.path(), &executable).unwrap();
        assert_eq!(
            fs::read_link(directory.path().join("codex")).unwrap(),
            executable
        );
        assert_eq!(
            fs::read_link(directory.path().join("claude")).unwrap(),
            executable
        );
    }
}
