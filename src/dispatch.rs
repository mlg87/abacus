use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};

use crate::app::AppEvent;
use crate::bd;
use crate::git::{self, WorktreePlan};
use crate::model::{Derived, Graph, gh_number};
use crate::prompt::{self, BeadRef, PreviousRun, PromptCtx};
use crate::runs::{self, Mode, RunMeta, RunState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requested {
    Default,
    Decompose,
}

#[derive(Debug, Clone, Copy)]
pub struct ActiveRun<'a> {
    pub run_id: &'a str,
    pub bead_id: &'a str,
    pub mode: Mode,
}

/// Decides which mode a dispatch of bead `idx` runs in, or why it must not run.
/// `retry_mode` carries the mode of the run being retried; retries may pick up
/// a bead their earlier attempt already claimed.
pub fn plan_dispatch(
    g: &Graph,
    active: &[ActiveRun],
    idx: usize,
    req: Requested,
    retry_mode: Option<Mode>,
) -> Result<Mode, String> {
    let epic = g.is_epic(idx);
    let mode = match (retry_mode, req) {
        (Some(m), _) => m,
        (None, Requested::Decompose) if !epic => {
            return Err("decompose applies to epics only".into());
        }
        (None, Requested::Decompose) => Mode::Decompose,
        (None, Requested::Default) if epic => {
            if gh_number(&g.beads[idx]).is_none() {
                return Err(
                    "epic has no gh-N external ref — D decomposes it, or dispatch its children"
                        .into(),
                );
            }
            Mode::Resolve
        }
        (None, Requested::Default) => Mode::Implement,
    };

    let derived = g.derived(idx);
    match (mode, derived) {
        (Mode::Decompose, Derived::Closed) => return Err("closed".into()),
        (Mode::Decompose, _) => {}
        (_, Derived::Ready) => {}
        (_, Derived::InProgress) if retry_mode.is_some() => {}
        (_, Derived::InProgress) => return Err("in progress".into()),
        (_, Derived::Closed) => return Err("closed".into()),
        (_, Derived::Deferred) => return Err("deferred".into()),
        (_, Derived::Blocked) => {
            return Err(g.blocked_reason(idx).unwrap_or_else(|| "blocked".into()));
        }
        (_, Derived::Other) => return Err("unrecognised status".into()),
    }

    let id = g.beads[idx].id.as_str();
    if let Some(a) = active.iter().find(|a| a.bead_id == id) {
        return Err(format!("already has active run {}", a.run_id));
    }
    if mode != Mode::Decompose {
        for anc in g.ancestors(idx) {
            let anc_id = g.beads[anc].id.as_str();
            if active
                .iter()
                .any(|a| a.bead_id == anc_id && a.mode == Mode::Resolve)
            {
                return Err(format!("inside active resolve run of {anc_id}"));
            }
        }
        for d in g.descendants(idx) {
            let d_id = g.beads[d].id.as_str();
            if active
                .iter()
                .any(|a| a.bead_id == d_id && a.mode != Mode::Decompose)
            {
                return Err(format!("descendant {d_id} has an active run"));
            }
        }
    }
    Ok(mode)
}

pub fn slugify(title: &str) -> String {
    let mut t = title.trim();
    loop {
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("epic:") {
            t = t[5..].trim_start();
        } else {
            break;
        }
    }
    let t = match t.rfind("(GH#") {
        Some(pos) if t.trim_end().ends_with(')') => t[..pos].trim_end(),
        _ => t,
    };
    let mut out = String::new();
    let mut dash = false;
    for c in t.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let trimmed: String = out.trim_matches('-').chars().take(40).collect();
    let trimmed = trimmed.trim_matches('-');
    if trimmed.is_empty() {
        "work".into()
    } else {
        trimmed.to_string()
    }
}

pub fn id_slug(id: &str) -> String {
    id.replace('.', "-")
}

pub fn plan_worktree(
    repo_root: &Path,
    state: &Path,
    g: &Graph,
    idx: usize,
    mode: Mode,
    run_id: &str,
    base: &str,
) -> WorktreePlan {
    let repo_name = repo_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let parent = repo_root.parent().unwrap_or(repo_root);
    let bead = &g.beads[idx];
    let slug = slugify(&bead.title);
    let ids = id_slug(&bead.id);
    let beside_repo = |suffix: &str| parent.join(format!("{repo_name}-{suffix}"));
    let (path, branch, gh) = match mode {
        Mode::Decompose => (state.join("scratch").join(run_id), None, None),
        Mode::Resolve => {
            let n = gh_number(bead);
            let branch = match n {
                Some(n) => format!("{n}-{slug}"),
                None => format!("bead/{ids}-{slug}"),
            };
            (beside_repo(&branch.replace('/', "-")), Some(branch), n)
        }
        Mode::Implement => match g.gh_ancestor(idx) {
            Some((_, n)) => {
                let branch = format!("{n}-{ids}-{slug}");
                (beside_repo(&branch), Some(branch), Some(n))
            }
            None => (
                beside_repo(&format!("bead-{ids}-{slug}")),
                Some(format!("bead/{ids}-{slug}")),
                None,
            ),
        },
    };
    WorktreePlan {
        path,
        branch,
        base: base.to_string(),
        gh,
    }
}

pub fn issue_url(origin: Option<&(String, String)>, n: u32) -> Option<String> {
    origin.map(|(o, r)| format!("https://github.com/{o}/{r}/issues/{n}"))
}

fn bead_ref(g: &Graph, j: usize, status: String) -> BeadRef {
    BeadRef {
        id: g.beads[j].id.clone(),
        title: g.beads[j].title.clone(),
        status,
    }
}

/// Snapshot of everything the prompt needs, taken on the main thread so the
/// worker never touches the live graph. `worktree`/`branch` are filled in by
/// `start_run` once the worktree exists.
pub fn prompt_ctx(
    g: &Graph,
    idx: usize,
    mode: Mode,
    repo_root: &Path,
    origin: Option<&(String, String)>,
    plan: &WorktreePlan,
    previous: Option<&runs::Run>,
) -> PromptCtx {
    let b = &g.beads[idx];
    let gh = match mode {
        Mode::Implement => g.gh_ancestor(idx),
        _ => gh_number(b).map(|n| (idx, n)),
    };
    PromptCtx {
        repo_name: repo_root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        repo_root: repo_root.to_path_buf(),
        id: b.id.clone(),
        issue_type: b.issue_type.clone(),
        priority: b.priority,
        title: b.title.clone(),
        labels: b.labels.clone(),
        description: b.description.clone(),
        acceptance_criteria: b.acceptance_criteria.clone(),
        notes: b.notes.clone(),
        parent: g.parent[idx].map(|p| bead_ref(g, p, g.derived(p).label().into())),
        blockers: g.blockers[idx]
            .iter()
            .map(|&j| bead_ref(g, j, g.derived(j).label().into()))
            .collect(),
        children: g.children[idx]
            .iter()
            .map(|&j| bead_ref(g, j, g.derived(j).glyph().into()))
            .collect(),
        mode,
        worktree: plan.path.clone(),
        branch: plan.branch.clone(),
        base: plan.base.clone(),
        gh: gh.map(|(_, n)| (n, issue_url(origin, n))),
        gh_bead_id: gh.map(|(j, _)| g.beads[j].id.clone()),
        previous: previous.map(|r| PreviousRun {
            run_id: r.meta.run_id.clone(),
            state: r.meta.state.label().into(),
            exit_code: r.meta.exit_code,
            result_line: r.progress.result_line.clone(),
        }),
    }
}

pub struct StartJob {
    pub repo_root: PathBuf,
    pub state_dir: PathBuf,
    pub agent_cmd: String,
    pub model: Option<String>,
    pub meta: RunMeta,
    pub plan: WorktreePlan,
    pub prompt: PromptCtx,
}

/// Runs on a worker thread: worktree → claim → prompt → detached agent process.
pub fn start_run(job: StartJob, tx: &Sender<AppEvent>) {
    let run_id = job.meta.run_id.clone();
    let event = match try_start(job) {
        Ok((pid, worktree, branch)) => AppEvent::DispatchStarted {
            run_id,
            pid,
            worktree,
            branch,
        },
        Err(e) => AppEvent::DispatchFailed {
            run_id,
            error: format!("{e:#}"),
        },
    };
    let _ = tx.send(event);
}

fn try_start(mut job: StartJob) -> Result<(u32, PathBuf, Option<String>)> {
    git::ensure_worktree(&job.repo_root, &mut job.plan)?;
    let wt = job.plan.path.clone();
    let branch = job.plan.branch.clone();
    job.meta.worktree = Some(wt.clone());
    job.meta.branch = branch.clone();
    runs::persist(&job.state_dir, &job.meta)?;

    let claimed = job.meta.mode != Mode::Decompose;
    if claimed {
        let note = format!(
            "abacus: run {} mode={} worktree={} branch={}",
            job.meta.run_id,
            job.meta.mode.label(),
            wt.display(),
            branch.as_deref().unwrap_or("-"),
        );
        bd::claim(&job.repo_root, &job.meta.bead_id, &note)?;
    }

    match spawn_agent(&mut job) {
        Ok(pid) => Ok((pid, wt, branch)),
        Err(e) => {
            if claimed {
                let note = format!("abacus: spawn failed for run {}: {e:#}", job.meta.run_id);
                let _ = bd::release(&job.repo_root, &job.meta.bead_id, &note);
            }
            Err(e)
        }
    }
}

fn spawn_agent(job: &mut StartJob) -> Result<u32> {
    job.prompt.worktree = job.plan.path.clone();
    job.prompt.branch = job.plan.branch.clone();
    let prompt = prompt::render(&job.prompt);
    let dir = runs::run_dir(&job.state_dir, &job.meta.run_id);
    std::fs::write(dir.join("prompt.md"), &prompt)?;
    let session_dir = dir.join("session");
    std::fs::create_dir_all(&session_dir)?;
    let stdout = File::create(dir.join("events.jsonl"))?;
    let stderr = File::create(dir.join("stderr.log"))?;
    let wt = &job.plan.path;

    let mut cmd = Command::new("/bin/sh");
    // The shell records the agent's exit status so a later abacus can tell
    // success from a crash after this process (and its Child handle) is gone.
    cmd.arg("-c")
        .arg(r#""$0" "$@"; echo $? > "$ABACUS_EXIT_FILE""#)
        .arg(&job.agent_cmd)
        .args(["-p", "--mode", "json", "--cwd"])
        .arg(wt)
        .arg("--session-dir")
        .arg(&session_dir)
        .arg("--auto-approve");
    if let Some(m) = &job.model {
        cmd.args(["--model", m]);
    }
    cmd.arg(&prompt)
        .current_dir(wt)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        // Own process group: survives abacus exit and lets kill target the whole tree.
        .process_group(0)
        .env("ABACUS_EXIT_FILE", dir.join("exit_code"))
        .env("ABACUS_RUN_ID", &job.meta.run_id)
        .env("ABACUS_BEAD_ID", &job.meta.bead_id);
    let mut child = cmd.spawn().context("failed to spawn agent")?;
    let pid = child.id();

    job.meta.pid = Some(pid);
    job.meta.state = RunState::Running;
    job.meta.started_at = Some(runs::now_secs());
    runs::persist(&job.state_dir, &job.meta)?;

    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::{bead, dep};

    fn epic(id: &str, gh: Option<u32>) -> crate::model::Bead {
        let mut b = bead(id, "open");
        b.issue_type = "epic".into();
        b.external_ref = gh.map(|n| format!("gh-{n}"));
        b
    }

    fn child(id: &str, parent: &str) -> crate::model::Bead {
        let mut b = bead(id, "open");
        b.parent = Some(parent.into());
        b
    }

    #[test]
    fn plan_dispatch_branches() {
        let mut blocked = child("t2", "e");
        blocked.dependencies.push(dep("t2", "t1", "blocks"));
        let mut claimed = bead("busy", "in_progress");
        claimed.issue_type = "task".into();
        let g = Graph::build(vec![
            epic("e", Some(1329)),
            epic("bare", None),
            child("t1", "e"),
            blocked,
            claimed,
        ]);
        let i = |id| g.get(id).unwrap();

        assert_eq!(
            plan_dispatch(&g, &[], i("e"), Requested::Default, None),
            Ok(Mode::Resolve)
        );
        assert!(
            plan_dispatch(&g, &[], i("bare"), Requested::Default, None)
                .unwrap_err()
                .starts_with("epic has no gh-N external ref")
        );
        assert_eq!(
            plan_dispatch(&g, &[], i("t2"), Requested::Default, None),
            Err("blocked by t1".into())
        );
        assert_eq!(
            plan_dispatch(&g, &[], i("t1"), Requested::Decompose, None),
            Err("decompose applies to epics only".into())
        );

        let resolve = [ActiveRun {
            run_id: "r1",
            bead_id: "e",
            mode: Mode::Resolve,
        }];
        assert_eq!(
            plan_dispatch(&g, &resolve, i("t1"), Requested::Default, None),
            Err("inside active resolve run of e".into())
        );
        let child_run = [ActiveRun {
            run_id: "r2",
            bead_id: "t1",
            mode: Mode::Implement,
        }];
        assert_eq!(
            plan_dispatch(&g, &child_run, i("e"), Requested::Default, None),
            Err("descendant t1 has an active run".into())
        );
        assert_eq!(
            plan_dispatch(&g, &child_run, i("t1"), Requested::Default, None),
            Err("already has active run r2".into())
        );

        assert_eq!(
            plan_dispatch(&g, &[], i("busy"), Requested::Default, None),
            Err("in progress".into())
        );
        assert_eq!(
            plan_dispatch(
                &g,
                &[],
                i("busy"),
                Requested::Default,
                Some(Mode::Implement)
            ),
            Ok(Mode::Implement)
        );
    }

    #[test]
    fn decompose_is_allowed_on_a_blocked_epic() {
        let mut e = epic("e", None);
        e.dependencies.push(dep("e", "x", "blocks"));
        let g = Graph::build(vec![e, bead("x", "open")]);
        assert_eq!(g.derived(0), Derived::Blocked);
        assert_eq!(
            plan_dispatch(&g, &[], 0, Requested::Decompose, None),
            Ok(Mode::Decompose)
        );
    }

    #[test]
    fn slugify_strips_epic_prefixes_and_gh_suffix() {
        assert_eq!(
            slugify("Epic: Epic: Native scoring engine (GH#1351)"),
            "native-scoring-engine"
        );
        assert_eq!(slugify(""), "work");
        assert_eq!(slugify("!!! ??? ---"), "work");
        assert_eq!(
            slugify("A very long title that keeps going well past forty characters"),
            "a-very-long-title-that-keeps-going-well"
        );
    }

    #[test]
    fn plan_worktree_table() {
        let mut e = epic("dlb-x9", Some(1351));
        e.title = "Epic: Native scoring engine (GH#1351)".into();
        let mut c = child("dlb-x9.2", "dlb-x9");
        c.title = "Score calc".into();
        let mut lone = bead("dlb-q.1", "open");
        lone.title = "Fix thing".into();
        let g = Graph::build(vec![e, c, lone]);
        let repo = Path::new("/w/dlb");
        let state = Path::new("/s");
        let plan = |id: &str, mode| {
            plan_worktree(
                repo,
                state,
                &g,
                g.get(id).unwrap(),
                mode,
                "9-r",
                "origin/main",
            )
        };

        let p = plan("dlb-x9", Mode::Resolve);
        assert_eq!(p.branch.as_deref(), Some("1351-native-scoring-engine"));
        assert_eq!(p.path, Path::new("/w/dlb-1351-native-scoring-engine"));
        assert_eq!(p.gh, Some(1351));

        let p = plan("dlb-x9.2", Mode::Implement);
        assert_eq!(p.branch.as_deref(), Some("1351-dlb-x9-2-score-calc"));
        assert_eq!(p.path, Path::new("/w/dlb-1351-dlb-x9-2-score-calc"));
        assert_eq!(p.gh, Some(1351));

        let p = plan("dlb-q.1", Mode::Implement);
        assert_eq!(p.branch.as_deref(), Some("bead/dlb-q-1-fix-thing"));
        assert_eq!(p.path, Path::new("/w/dlb-bead-dlb-q-1-fix-thing"));
        assert_eq!(p.gh, None);

        let p = plan("dlb-x9", Mode::Decompose);
        assert_eq!(p.branch, None);
        assert_eq!(p.path, Path::new("/s/scratch/9-r"));
        assert_eq!(p.base, "origin/main");
    }
}
