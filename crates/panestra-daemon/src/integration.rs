use std::{collections::HashMap, process::Command, sync::Arc};

use anyhow::{Context, Result, bail};
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

#[derive(Debug, Clone)]
struct Credentials {
    provider: AgentIntegration,
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
    credentials: Arc<RwLock<HashMap<Uuid, Credentials>>>,
}

impl IntegrationManager {
    pub fn new(base_url: String) -> Result<Self> {
        let executable = std::env::current_exe().context("cannot locate Panestra executable")?;
        Ok(Self {
            base_url,
            hook_executable: executable.to_string_lossy().into_owned(),
            credentials: Arc::default(),
        })
    }

    #[cfg(test)]
    pub fn for_tests() -> Self {
        Self {
            base_url: "http://127.0.0.1:8371".into(),
            hook_executable: "/tmp/panestra-daemon".into(),
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
        let credentials = Credentials {
            provider,
            hook_token: random_token(),
            mcp_token: random_token(),
        };
        let hook_url = format!("{}/api/integrations/{session_id}/hook", self.base_url);
        let mcp_url = format!("{}/api/integrations/{session_id}/mcp", self.base_url);
        let args = match provider {
            AgentIntegration::Codex => self.codex_args(&mcp_url, user_args)?,
            AgentIntegration::Claude => self.claude_args(&hook_url, &mcp_url, user_args)?,
        };
        let env = HashMap::from([
            (HOOK_TOKEN_ENV.into(), credentials.hook_token.clone()),
            (HOOK_ENDPOINT_ENV.into(), hook_url),
            (MCP_TOKEN_ENV.into(), credentials.mcp_token.clone()),
        ]);
        self.credentials.write().insert(session_id, credentials);
        Ok(PreparedLaunch { args, env })
    }

    pub fn revoke(&self, session_id: Uuid) {
        self.credentials.write().remove(&session_id);
    }

    pub fn authorize_hook(&self, session_id: Uuid, token: &str) -> Option<AgentIntegration> {
        self.credentials
            .read()
            .get(&session_id)
            .filter(|credentials| credentials.hook_token == token)
            .map(|credentials| credentials.provider)
    }

    pub fn authorize_mcp(&self, session_id: Uuid, token: &str) -> Option<AgentIntegration> {
        self.credentials
            .read()
            .get(&session_id)
            .filter(|credentials| credentials.mcp_token == token)
            .map(|credentials| credentials.provider)
    }

    fn codex_args(&self, mcp_url: &str, user_args: &[String]) -> Result<Vec<String>> {
        let command = toml_string(&format!("{} hook-event", self.hook_executable))?;
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
        &self,
        hook_url: &str,
        mcp_url: &str,
        user_args: &[String],
    ) -> Result<Vec<String>> {
        let command_hook = json!({
            "type": "command",
            "command": self.hook_executable,
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
        // hook_url is carried through the environment for the shared hook forwarder.
        let _ = hook_url;
        Ok(args)
    }
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
            match provider {
                AgentIntegration::Codex => "Codex CLI",
                AgentIntegration::Claude => "Claude Code",
            }
        );
    }
    Ok(())
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
}
