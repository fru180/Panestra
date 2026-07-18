use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use parking_lot::RwLock;
use tokio::{
    process::Command,
    sync::{Mutex, Semaphore},
};

use crate::model::GitStatus;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const CACHE_TTL: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct GitService {
    concurrency: Arc<Semaphore>,
    cache: Arc<RwLock<HashMap<PathBuf, CacheEntry>>>,
    locks: Arc<RwLock<HashMap<PathBuf, Arc<Mutex<()>>>>>,
}

#[derive(Clone)]
struct CacheEntry {
    created: Instant,
    status: GitStatus,
}

impl GitService {
    pub fn new() -> Self {
        Self {
            concurrency: Arc::new(Semaphore::new(2)),
            cache: Arc::default(),
            locks: Arc::default(),
        }
    }

    pub async fn resolve_working_tree(&self, directory: &Path) -> Result<Option<PathBuf>> {
        let output = self
            .run(directory, &["rev-parse", "--show-toplevel"])
            .await?;
        if !output.status.success() {
            return Ok(None);
        }
        let root = String::from_utf8(output.stdout).context("Git returned a non-UTF-8 root")?;
        Ok(Some(std::fs::canonicalize(root.trim())?))
    }

    pub async fn status(&self, root: &Path) -> Result<GitStatus> {
        let root = std::fs::canonicalize(root)?;
        if let Some(entry) = self.cache.read().get(&root)
            && entry.created.elapsed() < CACHE_TTL
        {
            return Ok(entry.status.clone());
        }
        let lock = self
            .locks
            .write()
            .entry(root.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        if let Some(entry) = self.cache.read().get(&root)
            && entry.created.elapsed() < CACHE_TTL
        {
            return Ok(entry.status.clone());
        }

        match self.collect_status(&root).await {
            Ok(status) => {
                self.cache.write().insert(
                    root,
                    CacheEntry {
                        created: Instant::now(),
                        status: status.clone(),
                    },
                );
                Ok(status)
            }
            Err(error) => {
                if let Some(previous) = self.cache.read().get(&root) {
                    let mut stale = previous.status.clone();
                    stale.stale = true;
                    stale.error = Some(error.to_string());
                    return Ok(stale);
                }
                Err(error)
            }
        }
    }

    async fn collect_status(&self, root: &Path) -> Result<GitStatus> {
        let status_output = self
            .run(
                root,
                &[
                    "status",
                    "--porcelain=v1",
                    "--branch",
                    "--untracked-files=all",
                ],
            )
            .await?;
        if !status_output.status.success() {
            bail!(
                "git status failed: {}",
                concise_stderr(&status_output.stderr)
            );
        }
        let status_text =
            String::from_utf8(status_output.stdout).context("git status was not UTF-8")?;
        let mut status = GitStatus {
            working_tree_root: root.to_string_lossy().into_owned(),
            actual_branch: None,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicts: 0,
            changed_files: 0,
            additions: 0,
            deletions: 0,
            ahead: None,
            behind: None,
            stale: false,
            error: None,
            updated_at: chrono::Utc::now(),
        };
        for line in status_text.lines() {
            if let Some(header) = line.strip_prefix("## ") {
                parse_branch_header(header, &mut status);
                continue;
            }
            let bytes = line.as_bytes();
            if bytes.len() < 2 {
                continue;
            }
            status.changed_files += 1;
            let code = &line[..2];
            if code == "??" {
                status.untracked += 1;
            } else if matches!(code, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU") {
                status.conflicts += 1;
            } else {
                if bytes[0] != b' ' {
                    status.staged += 1;
                }
                if bytes[1] != b' ' {
                    status.unstaged += 1;
                }
            }
        }

        let diff_output = self.run(root, &["diff", "--numstat", "HEAD"]).await?;
        if diff_output.status.success() {
            let text = String::from_utf8_lossy(&diff_output.stdout);
            for line in text.lines() {
                let mut fields = line.split('\t');
                status.additions += fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                status.deletions += fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
        }
        Ok(status)
    }

    async fn run(&self, directory: &Path, arguments: &[&str]) -> Result<std::process::Output> {
        let _permit = self
            .concurrency
            .acquire()
            .await
            .map_err(|_| anyhow!("Git service stopped"))?;
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .kill_on_drop(true);
        tokio::time::timeout(COMMAND_TIMEOUT, command.output())
            .await
            .map_err(|_| anyhow!("Git command timed out"))?
            .context("failed to execute git")
    }
}

fn parse_branch_header(header: &str, status: &mut GitStatus) {
    let header = header.strip_prefix("No commits yet on ").unwrap_or(header);
    let name = header.split(['.', '[']).next().unwrap_or(header).trim();
    if !name.is_empty() && name != "HEAD (no branch)" {
        status.actual_branch = Some(name.to_owned());
    }
    if let Some(detail) = header
        .split_once('[')
        .and_then(|(_, tail)| tail.strip_suffix(']'))
    {
        for part in detail.split(',').map(str::trim) {
            if let Some(value) = part.strip_prefix("ahead ") {
                status.ahead = value.parse().ok();
            }
            if let Some(value) = part.strip_prefix("behind ") {
                status.behind = value.parse().ok();
            }
        }
    }
}

fn concise_stderr(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim()
        .chars()
        .take(240)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_header_keeps_actual_branch_separate_from_agent_report() {
        let mut status = GitStatus {
            working_tree_root: String::new(),
            actual_branch: None,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicts: 0,
            changed_files: 0,
            additions: 0,
            deletions: 0,
            ahead: None,
            behind: None,
            stale: false,
            error: None,
            updated_at: chrono::Utc::now(),
        };
        parse_branch_header("feature...origin/feature [ahead 2, behind 1]", &mut status);
        assert_eq!(status.actual_branch.as_deref(), Some("feature"));
        assert_eq!(status.ahead, Some(2));
        assert_eq!(status.behind, Some(1));
    }

    #[tokio::test]
    async fn resolves_each_worktree_and_collects_repository_wide_changes() {
        fn git(directory: &Path, arguments: &[&str]) {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(directory)
                    .args(arguments)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-b", "main"]);
        git(
            &repository,
            &["config", "user.email", "panestra@example.invalid"],
        );
        git(&repository, &["config", "user.name", "Panestra Test"]);
        std::fs::write(repository.join("tracked.txt"), "initial\n").unwrap();
        git(&repository, &["add", "tracked.txt"]);
        git(&repository, &["commit", "-m", "initial"]);
        let worktree = temporary.path().join("worktree");
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        );
        std::fs::write(worktree.join("tracked.txt"), "changed\n").unwrap();

        let service = GitService::new();
        let resolved = service
            .resolve_working_tree(&worktree)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved, std::fs::canonicalize(&worktree).unwrap());
        let status = service.status(&resolved).await.unwrap();
        assert_eq!(status.actual_branch.as_deref(), Some("feature"));
        assert_eq!(status.changed_files, 1);
        assert_eq!(status.unstaged, 1);
    }
}
