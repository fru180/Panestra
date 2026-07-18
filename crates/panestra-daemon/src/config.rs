use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "panestra",
    version,
    about = "Local multi-terminal command center"
)]
pub struct Arguments {
    #[arg(long, default_value = "127.0.0.1:8371")]
    pub listen: SocketAddr,
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    #[arg(long)]
    pub web_dir: Option<PathBuf>,
    #[arg(long)]
    pub dev_web_origin: Option<String>,
    #[arg(long)]
    pub no_open: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub web_dir: PathBuf,
    pub browser_origin: String,
    pub no_open: bool,
}

impl Config {
    pub fn from_args(arguments: Arguments) -> Result<Self> {
        if !arguments.listen.ip().is_loopback() {
            anyhow::bail!("Panestra may only listen on an explicit loopback address");
        }
        let data_dir = arguments.data_dir.unwrap_or(default_data_dir()?);
        let web_dir = arguments
            .web_dir
            .unwrap_or_else(|| PathBuf::from("web/dist"));
        let browser_origin = arguments
            .dev_web_origin
            .unwrap_or_else(|| format!("http://{}", arguments.listen));
        Ok(Self {
            listen: arguments.listen,
            data_dir,
            web_dir,
            browser_origin,
            no_open: arguments.no_open,
        })
    }

    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("panestra.db")
    }

    pub fn bootstrap_path(&self) -> PathBuf {
        self.data_dir.join("bootstrap")
    }

    pub fn runtime_path(&self) -> PathBuf {
        self.data_dir.join("runtime.json")
    }
}

pub fn default_data_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not defined")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Panestra"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_the_panestra_daemon_port() {
        let arguments = Arguments::parse_from([
            "panestra",
            "--data-dir",
            "/tmp/panestra-test",
            "--web-dir",
            "/tmp/panestra-web",
        ]);
        let config = Config::from_args(arguments).unwrap();

        assert_eq!(config.listen, "127.0.0.1:8371".parse().unwrap());
        assert_eq!(config.browser_origin, "http://127.0.0.1:8371");
    }

    #[test]
    fn development_origin_does_not_change_the_daemon_port() {
        let arguments = Arguments::parse_from([
            "panestra",
            "--data-dir",
            "/tmp/panestra-test",
            "--web-dir",
            "/tmp/panestra-web",
            "--dev-web-origin",
            "http://127.0.0.1:8372",
        ]);
        let config = Config::from_args(arguments).unwrap();

        assert_eq!(config.listen, "127.0.0.1:8371".parse().unwrap());
        assert_eq!(config.browser_origin, "http://127.0.0.1:8372");
    }
}
