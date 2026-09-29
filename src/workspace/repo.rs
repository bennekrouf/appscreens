//! Is the project's source code safe somewhere other than this Mac?

use std::path::Path;
use std::process::Command;

#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct RepoState {
    pub is_git: bool,
    pub remote: Option<String>,
    /// Commits not on the upstream branch; None when there is no upstream
    pub unpushed: Option<u32>,
    pub uncommitted: u32,
    /// "2026-04-18" — date of the last commit
    pub last_commit: String,
}

impl RepoState {
    /// Everything committed is also somewhere else.
    pub(super) fn safe(&self) -> bool {
        self.is_git && self.remote.is_some() && self.unpushed == Some(0)
    }

    pub(super) fn summary(&self) -> String {
        if !self.is_git {
            return "Not a git repository — nothing backs this code up".into();
        }
        let Some(remote) = &self.remote else {
            return "No remote — the code exists only on this Mac".into();
        };
        let pushed = match self.unpushed {
            None => "the branch has no upstream — it has never been pushed".to_string(),
            Some(0) => "everything committed is pushed".to_string(),
            Some(n) => format!("{n} commit(s) not pushed"),
        };
        let mut s = format!("{remote} · {pushed}");
        if self.uncommitted > 0 {
            s.push_str(&format!(" · {} uncommitted file(s)", self.uncommitted));
        }
        if !self.last_commit.is_empty() {
            s.push_str(&format!(" · last commit {}", self.last_commit));
        }
        s
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub(super) fn repo_state(dir: &Path) -> RepoState {
    if git(dir, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return RepoState::default();
    }
    RepoState {
        is_git: true,
        remote: git(dir, &["remote", "get-url", "origin"])
            .or_else(|| git(dir, &["remote"]).and_then(|r| r.lines().next().map(str::to_string)))
            .filter(|r| !r.is_empty()),
        unpushed: git(dir, &["rev-list", "--count", "@{u}..HEAD"]).and_then(|n| n.parse().ok()),
        uncommitted: git(dir, &["status", "--porcelain"]).map(|s| s.lines().count() as u32).unwrap_or(0),
        last_commit: git(dir, &["log", "-1", "--format=%cs"]).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_unsafe_repositories() {
        let dir = std::env::temp_dir().join("appscreens-test-repo");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!repo_state(&dir).is_git);

        let g = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(args).status().unwrap().success());
        g(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        g(&["add", "."]);
        g(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "a"]);
        let s = repo_state(&dir);
        assert!(s.is_git && s.remote.is_none() && !s.safe());
        assert!(s.summary().contains("only on this Mac"));

        std::fs::write(dir.join("b.txt"), "b").unwrap();
        assert_eq!(repo_state(&dir).uncommitted, 1);
    }
}
