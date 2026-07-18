mod api;
mod auth;
mod config;
mod git;
mod integration;
mod model;
mod persistence;
mod protocol;
mod session;

use std::{fs, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::{
    api::{AppState, OpenResponse, router},
    auth::{AuthState, random_token},
    config::{Arguments, Config, default_data_dir},
    git::GitService,
    integration::IntegrationManager,
    persistence::Store,
    session::SessionManager,
};

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("hook-event") {
        integration::forward_hook_event()?;
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("session-launcher") {
        session::run_session_launcher()?;
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("open") {
        open_existing(OpenArguments::parse_from(std::env::args().skip(1)))?;
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("panestra=info")),
        )
        .with_target(false)
        .init();

    let config = Config::from_args(Arguments::parse())?;
    fs::create_dir_all(&config.data_dir)?;
    set_owner_only(&config.data_dir)?;
    let store = Store::open(&config.database_path())?;
    let interrupted = store.mark_interrupted_sessions()?;
    if interrupted > 0 {
        tracing::warn!(
            count = interrupted,
            "finalized sessions left by a previous daemon"
        );
    }

    let daemon_epoch = Uuid::new_v4();
    let integrations = IntegrationManager::new(format!("http://{}", config.listen))?;
    let sessions = SessionManager::new(
        store.clone(),
        daemon_epoch,
        integrations.clone(),
        config.data_dir.join("checkpoints"),
    );
    let auth = Arc::new(AuthState::default());
    let control_credential: Arc<str> = random_token().into();
    let bootstrap = auth.issue_bootstrap();
    write_bootstrap_file(&config, &bootstrap)?;

    let (lease_events, _) = tokio::sync::broadcast::channel(256);
    let state = AppState {
        auth,
        sessions: sessions.clone(),
        integrations,
        git: GitService::new(),
        store,
        config: config.clone(),
        input_leases: Arc::default(),
        resize_leases: Arc::default(),
        lease_events,
        control_credential: Arc::clone(&control_credential),
    };
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("failed to listen on {}", config.listen))?;
    tracing::info!(address = %config.listen, "Panestra daemon is ready");
    write_runtime_file(&config, &control_credential)?;

    if !config.no_open {
        open_browser(&config, &bootstrap);
    }

    let checkpoint_sessions = sessions.clone();
    let checkpoint_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.tick().await;
        loop {
            interval.tick().await;
            checkpoint_sessions.checkpoint_all();
        }
    });

    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down managed sessions");
            sessions.shutdown().await;
        })
        .await?;
    checkpoint_task.abort();
    let _ = fs::remove_file(config.bootstrap_path());
    let _ = fs::remove_file(config.runtime_path());
    Ok(())
}

#[derive(Debug, Parser)]
#[command(
    name = "panestra open",
    about = "Open a new browser tab for a running Panestra daemon"
)]
struct OpenArguments {
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeInfo {
    origin: String,
    control_credential: String,
}

fn write_runtime_file(config: &Config, control_credential: &str) -> Result<()> {
    let runtime = RuntimeInfo {
        origin: config.browser_origin.clone(),
        control_credential: control_credential.to_owned(),
    };
    fs::write(config.runtime_path(), serde_json::to_vec(&runtime)?)?;
    set_owner_only(&config.runtime_path())?;
    Ok(())
}

fn open_existing(arguments: OpenArguments) -> Result<()> {
    let data_dir = arguments.data_dir.unwrap_or(default_data_dir()?);
    let runtime_path = data_dir.join("runtime.json");
    ensure_owner_only_file(&runtime_path)?;
    let runtime: RuntimeInfo = serde_json::from_slice(&fs::read(&runtime_path)?)?;
    let endpoint = format!("{}/api/control/open", runtime.origin);
    let mut response = ureq::post(&endpoint)
        .header(
            "Authorization",
            &format!("Bearer {}", runtime.control_credential),
        )
        .send_empty()
        .context("failed to contact the running Panestra daemon")?;
    let open_response: OpenResponse = response.body_mut().read_json()?;
    std::process::Command::new("open")
        .arg(&open_response.url)
        .spawn()
        .context("failed to open the supported browser")?;
    println!("Opened Panestra in a new browser tab.");
    Ok(())
}

fn write_bootstrap_file(config: &Config, token: &str) -> Result<()> {
    fs::write(config.bootstrap_path(), token)?;
    set_owner_only(&config.bootstrap_path())?;
    Ok(())
}

fn open_browser(config: &Config, token: &str) {
    let url = format!("{}/#bootstrap={token}", config.browser_origin);
    if let Err(error) = std::process::Command::new("open").arg(&url).spawn() {
        tracing::warn!(%error, "could not open the browser; use the bootstrap file with the open command");
    }
}

#[cfg(unix)]
fn set_owner_only(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if path.is_dir() { 0o700 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(unix)]
fn ensure_owner_only_file(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::metadata(path)?;
    anyhow::ensure!(metadata.is_file(), "runtime metadata is not a regular file");
    anyhow::ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "runtime metadata has a different owner"
    );
    anyhow::ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "runtime metadata is accessible by another user"
    );
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only_file(_path: &std::path::Path) -> Result<()> {
    Ok(())
}
