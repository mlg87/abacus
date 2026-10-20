use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreePlan {
    pub path: PathBuf,
    /// `None` = detached scratch checkout (decompose mode).
    pub branch: Option<String>,
    /// Remote-tracking ref, e.g. `origin/main`.
    pub base: String,
    pub gh: Option<u32>,
}

fn git_output(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("failed to run git")
}

pub fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = git_output(repo, args)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        bail!("git {} failed: {}", args.join(" "), first.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_ok(repo: &Path, args: &[&str]) -> bool {
    git_output(repo, args).is_ok_and(|o| o.status.success())
}

pub fn default_base(repo: &Path) -> String {
    git(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "origin/main".into())
}

pub fn parse_origin(url: &str) -> Option<(String, String)> {
    let rest = url
        .trim()
        .strip_prefix("git@github.com:")
        .or_else(|| url.trim().strip_prefix("https://github.com/"))
        .or_else(|| url.trim().strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

pub fn origin_slug(repo: &Path) -> Option<(String, String)> {
    parse_origin(&git(repo, &["remote", "get-url", "origin"]).ok()?)
}

/// `(path, branch)` pairs from `git worktree list --porcelain`.
pub fn parse_worktree_list(s: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut out = Vec::new();
    for block in s.split("\n\n") {
        let mut path = None;
        let mut branch = None;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                branch = Some(b.to_string());
            }
        }
        if let Some(p) = path {
            out.push((p, branch));
        }
    }
    out
}

/// macOS hands out both `/tmp/x` and `/private/tmp/x` for the same directory.
fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    a == b || canon(a) == canon(b)
}

/// Creates or reuses the worktree for `plan`, adjusting `plan.path` when the
/// branch is already checked out elsewhere.
pub fn ensure_worktree(repo: &Path, plan: &mut WorktreePlan) -> Result<()> {
    git(repo, &["fetch", "origin"])?;
    let path_str = plan.path.to_string_lossy().into_owned();
    let Some(branch) = plan.branch.clone() else {
        git(
            repo,
            &["worktree", "add", "--detach", &path_str, &plan.base],
        )?;
        return Ok(());
    };

    let list = parse_worktree_list(&git(repo, &["worktree", "list", "--porcelain"])?);
    if let Some((_, b)) = list.iter().find(|(p, _)| same_path(p, &plan.path)) {
        if b.as_deref() == Some(branch.as_str()) {
            return Ok(());
        }
        bail!(
            "worktree {} is on branch {}",
            plan.path.display(),
            b.as_deref().unwrap_or("(detached)")
        );
    }
    if let Some((p, _)) = list
        .iter()
        .find(|(_, b)| b.as_deref() == Some(branch.as_str()))
    {
        plan.path = p.clone();
        return Ok(());
    }
    if plan.path.exists() {
        bail!("{}: path exists and is not a worktree", plan.path.display());
    }

    let local_ref = format!("refs/heads/{branch}");
    if git_ok(repo, &["rev-parse", "--verify", "--quiet", &local_ref]) {
        git(repo, &["worktree", "add", &path_str, &branch])?;
        return Ok(());
    }
    let remote_ref = format!("origin/{branch}");
    let track_remote = |repo: &Path| -> Result<()> {
        git(repo, &["fetch", "origin", &branch])?;
        git(
            repo,
            &[
                "worktree",
                "add",
                "--track",
                "-b",
                &branch,
                &path_str,
                &remote_ref,
            ],
        )
        .map(drop)
    };
    if git_ok(
        repo,
        &["ls-remote", "--exit-code", "--heads", "origin", &branch],
    ) {
        return track_remote(repo);
    }
    if let Some(n) = plan.gh {
        // Creating the branch through GitHub links it to the issue (user rule:
        // issue work lives on an issue-linked branch).
        let base_branch = plan.base.strip_prefix("origin/").unwrap_or(&plan.base);
        let out = Command::new("gh")
            .args([
                "issue",
                "develop",
                &n.to_string(),
                "--base",
                base_branch,
                "--name",
                &branch,
            ])
            .current_dir(repo)
            .output()
            .context("failed to run gh")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!(
                "gh issue develop {n} failed: {}",
                stderr.lines().next().unwrap_or("").trim()
            );
        }
        return track_remote(repo);
    }
    git(
        repo,
        &[
            "worktree",
            "add",
            "--no-track",
            "-b",
            &branch,
            &path_str,
            &plan.base,
        ],
    )
    .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_remote_forms() {
        let want = Some(("mlg87".to_string(), "dlb".to_string()));
        assert_eq!(parse_origin("git@github.com:mlg87/dlb.git"), want);
        assert_eq!(parse_origin("https://github.com/mlg87/dlb"), want);
        assert_eq!(parse_origin("https://github.com/mlg87/dlb.git\n"), want);
        assert_eq!(parse_origin("/tmp/abacus-smoke/origin.git"), None);
    }

    #[test]
    fn parses_porcelain_worktree_list() {
        let s = "worktree /a\nHEAD 1\nbranch refs/heads/main\n\nworktree /b\nHEAD 2\ndetached\n";
        assert_eq!(
            parse_worktree_list(s),
            vec![
                (PathBuf::from("/a"), Some("main".into())),
                (PathBuf::from("/b"), None)
            ]
        );
    }
}
