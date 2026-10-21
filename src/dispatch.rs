use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};

use crate::app::AppEvent;
use crate::bd;
use crate::git::{self, WorktreePlan};
use crate::model::{BlockedBy, Derived, Graph, Status, gh_number};
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
/// a bead their earlier attempt already claimed. `waits` marks a run queued behind
/// prerequisites: being blocked is then expected, unless by a manual `blocked` status.
pub fn plan_dispatch(
    g: &Graph,
    active: &[ActiveRun],
    idx: usize,
    req: Requested,
    retry_mode: Option<Mode>,
    waits: bool,
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
        (_, Derived::Blocked) if waits && g.blocked_by(idx) != Some(BlockedBy::Status) => {}
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

/// One bead of a dependency chain: queued now, started once every bead in `after`
/// has closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainStep {
    pub idx: usize,
    pub after: Vec<usize>,
}

/// Open beads outside `idx`'s own line (itself, ancestors, descendants) blocking the
/// bead, an ancestor (whose gate holds back everything below it), or an open
/// descendant (a resolve run has to work every child).
pub fn gating_beads(g: &Graph, idx: usize) -> Vec<usize> {
    let ancestors = g.ancestors(idx);
    let descendants = g.descendants(idx);
    let line: HashSet<usize> = std::iter::once(idx)
        .chain(ancestors.iter().copied())
        .chain(descendants.iter().copied())
        .collect();
    let open_descendants = descendants
        .iter()
        .copied()
        .filter(|&d| g.beads[d].status != Status::Closed);
    let mut seen = HashSet::new();
    std::iter::once(idx)
        .chain(ancestors.iter().copied())
        .chain(open_descendants)
        .flat_map(|n| g.blockers[n].iter().copied())
        .filter(|&b| g.beads[b].status != Status::Closed && !line.contains(&b) && seen.insert(b))
        .collect()
}

/// Everything that has to close before blocked bead `idx` can start, in dependency
/// order and ending with `idx`. Dispatching a blocked bead used to be refused, which
/// sent users hunting for the beads to dispatch first; now the chain is queued and
/// runs in order, independent steps in parallel. `covered` marks beads an existing
/// or already-planned run will deliver: they are waited on but not scheduled again.
pub fn prerequisite_chain(
    g: &Graph,
    idx: usize,
    covered: &dyn Fn(usize) -> bool,
) -> Result<Vec<ChainStep>, String> {
    let mut steps = Vec::new();
    let mut done: HashMap<usize, bool> = HashMap::new();
    visit_chain(g, idx, idx, covered, &mut done, &mut steps)?;

    // A step under another step's resolve run is that run's work, unless the
    // ancestor itself waits on it: then it must run first, on its own.
    let pos: HashMap<usize, usize> = steps.iter().enumerate().map(|(k, s)| (s.idx, k)).collect();
    let mut waits: Vec<HashSet<usize>> = Vec::with_capacity(steps.len());
    for s in &steps {
        let mut w = HashSet::new();
        for &a in &s.after {
            w.insert(a);
            if let Some(&k) = pos.get(&a) {
                w.extend(waits[k].iter().copied());
            }
        }
        waits.push(w);
    }
    let keep: Vec<bool> = steps
        .iter()
        .map(|s| {
            !g.ancestors(s.idx)
                .iter()
                .any(|a| pos.get(a).is_some_and(|&k| !waits[k].contains(&s.idx)))
        })
        .collect();
    Ok(steps
        .into_iter()
        .zip(keep)
        .filter_map(|(s, k)| k.then_some(s))
        .collect())
}

fn visit_chain(
    g: &Graph,
    n: usize,
    root: usize,
    covered: &dyn Fn(usize) -> bool,
    done: &mut HashMap<usize, bool>,
    steps: &mut Vec<ChainStep>,
) -> Result<(), String> {
    match done.get(&n) {
        Some(true) => return Ok(()),
        Some(false) => return Err(format!("dependency cycle through {}", g.beads[n].id)),
        None => {}
    }
    let blocked = g.derived(n) == Derived::Blocked;
    let after = if n == root || blocked {
        gating_beads(g, n)
    } else {
        Vec::new()
    };
    if blocked && (after.is_empty() || g.blocked_by(n) == Some(BlockedBy::Status)) {
        // Nothing outside the bead can unblock it: a manual hold or an internal knot.
        let reason = g.blocked_reason(n).unwrap_or_else(|| "blocked".into());
        return Err(if n == root {
            reason
        } else {
            format!("prerequisite {}: {reason}", g.beads[n].id)
        });
    }
    done.insert(n, false);
    for &p in &after {
        if !covered(p) {
            visit_chain(g, p, root, covered, done, steps)?;
        }
    }
    done.insert(n, true);
    steps.push(ChainStep { idx: n, after });
    Ok(())
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
                let _ = bd::release(&job.repo_root, &[&job.meta.bead_id], &note);
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
            plan_dispatch(&g, &[], i("e"), Requested::Default, None, false),
            Ok(Mode::Resolve)
        );
        assert!(
            plan_dispatch(&g, &[], i("bare"), Requested::Default, None, false)
                .unwrap_err()
                .starts_with("epic has no gh-N external ref")
        );
        assert_eq!(
            plan_dispatch(&g, &[], i("t2"), Requested::Default, None, false),
            Err("blocked by t1".into())
        );
        assert_eq!(
            plan_dispatch(&g, &[], i("t2"), Requested::Default, None, true),
            Ok(Mode::Implement)
        );
        assert_eq!(
            plan_dispatch(&g, &[], i("t1"), Requested::Decompose, None, false),
            Err("decompose applies to epics only".into())
        );

        let resolve = [ActiveRun {
            run_id: "r1",
            bead_id: "e",
            mode: Mode::Resolve,
        }];
        assert_eq!(
            plan_dispatch(&g, &resolve, i("t1"), Requested::Default, None, false),
            Err("inside active resolve run of e".into())
        );
        let child_run = [ActiveRun {
            run_id: "r2",
            bead_id: "t1",
            mode: Mode::Implement,
        }];
        assert_eq!(
            plan_dispatch(&g, &child_run, i("e"), Requested::Default, None, false),
            Err("descendant t1 has an active run".into())
        );
        assert_eq!(
            plan_dispatch(&g, &child_run, i("t1"), Requested::Default, None, false),
            Err("already has active run r2".into())
        );

        assert_eq!(
            plan_dispatch(&g, &[], i("busy"), Requested::Default, None, false),
            Err("in progress".into())
        );
        assert_eq!(
            plan_dispatch(
                &g,
                &[],
                i("busy"),
                Requested::Default,
                Some(Mode::Implement),
                false
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
            plan_dispatch(&g, &[], 0, Requested::Decompose, None, false),
            Ok(Mode::Decompose)
        );
    }

    fn blocked_on(mut b: crate::model::Bead, on: &str) -> crate::model::Bead {
        let id = b.id.clone();
        b.dependencies.push(dep(&id, on, "blocks"));
        b
    }

    /// Chain as (bead id, after ids), for readable assertions.
    fn chain(
        g: &Graph,
        id: &str,
        covered: &dyn Fn(usize) -> bool,
    ) -> Result<Vec<(String, Vec<String>)>, String> {
        let ids = |v: &[usize]| v.iter().map(|&j| g.beads[j].id.clone()).collect::<Vec<_>>();
        prerequisite_chain(g, g.get(id).unwrap(), covered).map(|steps| {
            steps
                .iter()
                .map(|s| (g.beads[s.idx].id.clone(), ids(&s.after)))
                .collect()
        })
    }

    fn step(id: &str, after: &[&str]) -> (String, Vec<String>) {
        (id.into(), after.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn chain_orders_transitive_blockers_first_and_leaves_independent_ones_parallel() {
        let g = Graph::build(vec![
            blocked_on(blocked_on(bead("t", "open"), "b"), "p"),
            blocked_on(bead("b", "open"), "c"),
            bead("c", "open"),
            bead("p", "open"),
        ]);
        assert_eq!(
            chain(&g, "t", &|_| false),
            Ok(vec![
                step("c", &[]),
                step("b", &["c"]),
                step("p", &[]),
                step("t", &["b", "p"]),
            ])
        );
    }

    #[test]
    fn chain_for_an_epic_waits_on_what_blocks_its_children() {
        let g = Graph::build(vec![
            epic("x", Some(1)),
            blocked_on(child("c1", "x"), "p1"),
            blocked_on(child("c2", "x"), "p2"),
            // Internal ordering is the resolve run's business, not the chain's.
            blocked_on(child("c3", "x"), "c1"),
            bead("p1", "open"),
            bead("p2", "open"),
        ]);
        assert_eq!(
            chain(&g, "x", &|_| false),
            Ok(vec![
                step("p1", &[]),
                step("p2", &[]),
                step("x", &["p1", "p2"])
            ])
        );
    }

    #[test]
    fn chain_waits_on_covered_beads_without_scheduling_them() {
        let g = Graph::build(vec![blocked_on(bead("t", "open"), "b"), bead("b", "open")]);
        let b = g.get("b").unwrap();
        assert_eq!(chain(&g, "t", &|j| j == b), Ok(vec![step("t", &["b"])]));
    }

    #[test]
    fn chain_leaves_a_step_to_its_ancestors_resolve_run() {
        let g = Graph::build(vec![
            blocked_on(blocked_on(bead("t", "open"), "a"), "a1"),
            epic("a", Some(7)),
            child("a1", "a"),
        ]);
        assert_eq!(
            chain(&g, "t", &|_| false),
            Ok(vec![step("a", &[]), step("t", &["a", "a1"])])
        );
    }

    #[test]
    fn chain_refuses_cycles_and_manual_holds() {
        let g = Graph::build(vec![
            blocked_on(bead("t", "open"), "a"),
            blocked_on(bead("a", "open"), "b"),
            blocked_on(bead("b", "open"), "a"),
        ]);
        assert!(
            chain(&g, "t", &|_| false)
                .unwrap_err()
                .starts_with("dependency cycle")
        );

        let g = Graph::build(vec![
            blocked_on(bead("t", "open"), "h"),
            bead("h", "blocked"),
        ]);
        assert_eq!(
            chain(&g, "t", &|_| false),
            Err("prerequisite h: blocked (status)".into())
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
