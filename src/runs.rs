use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Resolve,
    Implement,
    Decompose,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Resolve => "resolve",
            Mode::Implement => "implement",
            Mode::Decompose => "decompose",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    Queued,
    Starting,
    Running,
    Succeeded,
    Failed,
    Killed,
}

impl RunState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            RunState::Queued | RunState::Starting | RunState::Running
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            RunState::Queued => "queued",
            RunState::Starting => "starting",
            RunState::Running => "running",
            RunState::Succeeded => "succeeded",
            RunState::Failed => "failed",
            RunState::Killed => "killed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMeta {
    pub run_id: String,
    pub repo_root: PathBuf,
    pub bead_id: String,
    pub bead_title: String,
    pub mode: Mode,
    pub state: RunState,
    pub worktree: Option<PathBuf>,
    pub branch: Option<String>,
    pub pid: Option<u32>,
    pub queued_at: u64,
    pub started_at: Option<u64>,
    pub ended_at: Option<u64>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub retry_of: Option<String>,
    #[serde(default)]
    pub hidden: bool,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

pub fn run_dir(state: &Path, run_id: &str) -> PathBuf {
    state.join("runs").join(run_id)
}

pub fn persist(state: &Path, meta: &RunMeta) -> Result<()> {
    let dir = run_dir(state, &meta.run_id);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let tmp = dir.join("meta.json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
    fs::rename(&tmp, dir.join("meta.json"))?;
    Ok(())
}

pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 only checks for existence/permission.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn read_exit_code(dir: &Path) -> Option<i32> {
    let s = fs::read_to_string(dir.join("exit_code")).ok()?;
    s.trim().parse().ok()
}

const LOG_CAP: usize = 2000;

static PR_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https://github\.com/[\w.-]+/[\w.-]+/pull/\d+").unwrap());

/// Live view of an agent's stdout JSONL; rebuilt from `events.jsonl` on load.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    pub session_id: Option<String>,
    pub activity: Option<String>,
    pub cost_usd: f64,
    pub result_line: Option<String>,
    pub pr_url: Option<String>,
    pub log: VecDeque<String>,
    pub agent_ended: bool,
    pub offset: u64,
    /// Raw bytes, so a read that splits a multi-byte UTF-8 char decodes correctly later.
    pub partial: Vec<u8>,
}

impl Progress {
    fn push_log(&mut self, line: String) {
        if self.log.len() == LOG_CAP {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    pub fn ingest(&mut self, bytes: &[u8]) {
        self.offset += bytes.len() as u64;
        self.partial.extend_from_slice(bytes);
        let Some(last_nl) = self.partial.iter().rposition(|&b| b == b'\n') else {
            return;
        };
        let rest = self.partial.split_off(last_nl + 1);
        let complete = std::mem::replace(&mut self.partial, rest);
        for line in String::from_utf8_lossy(&complete).lines() {
            self.ingest_line(line);
        }
    }

    /// Ingests an unterminated last line once the writer is gone (e.g. an agent
    /// killed mid-write); while the run is live such bytes are an incomplete read.
    pub fn finish(&mut self) {
        let rest = std::mem::take(&mut self.partial);
        let line = String::from_utf8_lossy(&rest);
        self.ingest_line(line.trim_end_matches('\r'));
    }

    fn ingest_line(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            self.push_log(line.chars().take(300).collect());
            return;
        };
        let str_of =
            |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        match v.get("type").and_then(|t| t.as_str()) {
            Some("session") => self.session_id = str_of(&v, "id"),
            Some("tool_execution_start") => {
                let tool = str_of(&v, "toolName").unwrap_or_else(|| "tool".into());
                let activity = match str_of(&v, "intent").filter(|s| !s.is_empty()) {
                    Some(intent) => format!("{tool}: {intent}"),
                    None => tool,
                };
                self.push_log(format!("→ {activity}"));
                self.activity = Some(activity);
            }
            Some("message_end") => {
                let msg = &v["message"];
                if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
                    return;
                }
                if let Some(c) = msg.pointer("/usage/cost/total").and_then(|c| c.as_f64()) {
                    self.cost_usd += c;
                }
                let Some(content) = msg.get("content").and_then(|c| c.as_array()) else {
                    return;
                };
                for item in content {
                    if item.get("type").and_then(|t| t.as_str()) != Some("text") {
                        continue;
                    }
                    let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    for l in text.lines() {
                        if l.trim_start().starts_with("RESULT:") {
                            self.result_line = Some(l.trim().to_string());
                        }
                        self.push_log(format!("  {l}"));
                    }
                    if let Some(m) = PR_URL.find_iter(text).last() {
                        self.pr_url = Some(m.as_str().to_string());
                    }
                }
            }
            Some("agent_end") => {
                self.agent_ended = true;
                self.push_log("■ agent finished".into());
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
pub struct Run {
    pub meta: RunMeta,
    pub progress: Progress,
}

pub struct Registry {
    pub state_dir: PathBuf,
    pub runs: Vec<Run>,
}

impl Registry {
    pub fn load(state_dir: &Path, repo_root: &Path) -> Result<Registry> {
        let mut reg = Registry {
            state_dir: state_dir.to_path_buf(),
            runs: Vec::new(),
        };
        let runs_dir = state_dir.join("runs");
        let entries = match fs::read_dir(&runs_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(reg),
            Err(e) => return Err(e).context("read runs dir"),
        };
        for entry in entries.flatten() {
            let Ok(bytes) = fs::read(entry.path().join("meta.json")) else {
                continue;
            };
            let Ok(meta) = serde_json::from_slice::<RunMeta>(&bytes) else {
                continue;
            };
            if meta.repo_root != repo_root {
                continue;
            }
            let mut run = Run {
                meta,
                progress: Progress::default(),
            };
            reg.catch_up(&mut run);
            if run.meta.state == RunState::Starting && run.meta.pid.is_none() {
                run.meta.state = RunState::Failed;
                run.meta.error = Some("abacus exited during dispatch".into());
                run.meta.ended_at = Some(now_secs());
                persist(state_dir, &run.meta)?;
            } else if matches!(run.meta.state, RunState::Starting | RunState::Running) {
                reg.reconcile(&mut run);
                // The agent may have exited between the first read and the reconcile.
                reg.catch_up(&mut run);
            }
            if !run.meta.state.is_active() {
                run.progress.finish();
            }
            if run.meta.state == RunState::Failed {
                reg.append_stderr_tail(&mut run);
            }
            reg.runs.push(run);
        }
        reg.runs.sort_by(|a, b| {
            a.meta
                .queued_at
                .cmp(&b.meta.queued_at)
                .then(a.meta.run_id.cmp(&b.meta.run_id))
        });
        Ok(reg)
    }

    pub fn dir(&self, run: &RunMeta) -> PathBuf {
        run_dir(&self.state_dir, &run.run_id)
    }

    /// Reads any new bytes of `events.jsonl` into the run's progress.
    fn catch_up(&self, run: &mut Run) {
        let path = self.dir(&run.meta).join("events.jsonl");
        let Ok(mut f) = fs::File::open(&path) else {
            return;
        };
        if f.seek(SeekFrom::Start(run.progress.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
            run.progress.ingest(&buf);
        }
    }

    /// Settles a Starting/Running run from on-disk evidence. Returns true if it ended.
    fn reconcile(&self, run: &mut Run) -> bool {
        let dir = self.dir(&run.meta);
        if let Some(code) = read_exit_code(&dir) {
            run.meta.exit_code = Some(code);
            run.meta.state = if code == 0 {
                RunState::Succeeded
            } else {
                RunState::Failed
            };
        } else if run.meta.pid.is_some_and(pid_alive) {
            if run.meta.state == RunState::Starting {
                run.meta.state = RunState::Running;
                let _ = persist(&self.state_dir, &run.meta);
            }
            return false;
        } else {
            run.meta.state = RunState::Failed;
            run.meta.error = Some("agent process ended without exit status".into());
        }
        run.meta.ended_at = Some(now_secs());
        let _ = persist(&self.state_dir, &run.meta);
        true
    }

    fn append_stderr_tail(&self, run: &mut Run) {
        let Ok(s) = fs::read_to_string(self.dir(&run.meta).join("stderr.log")) else {
            return;
        };
        let lines: Vec<&str> = s.lines().collect();
        for l in &lines[lines.len().saturating_sub(20)..] {
            run.progress.push_log(format!("stderr: {l}"));
        }
    }

    /// Advances every Running run; returns how many ended.
    pub fn poll(&mut self) -> usize {
        let mut runs = std::mem::take(&mut self.runs);
        let mut ended = 0;
        for run in runs
            .iter_mut()
            .filter(|r| r.meta.state == RunState::Running)
        {
            self.catch_up(run);
            if self.reconcile(run) {
                // Drain whatever the agent wrote between our read and its exit.
                self.catch_up(run);
                run.progress.finish();
                if run.meta.state == RunState::Failed {
                    self.append_stderr_tail(run);
                }
                ended += 1;
            }
        }
        self.runs = runs;
        ended
    }

    pub fn find(&self, run_id: &str) -> Option<&Run> {
        self.runs.iter().find(|r| r.meta.run_id == run_id)
    }

    pub fn find_mut(&mut self, run_id: &str) -> Option<&mut Run> {
        self.runs.iter_mut().find(|r| r.meta.run_id == run_id)
    }

    pub fn new_run_id(&self, bead_id: &str) -> String {
        let base = format!("{}-{}", now_secs(), bead_id.replace('.', "-"));
        let taken = |id: &str| self.find(id).is_some() || run_dir(&self.state_dir, id).exists();
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|n| format!("{base}-{n}"))
            .find(|id| !taken(id))
            .expect("unbounded")
    }

    pub fn kill(&mut self, run_id: &str) -> Result<()> {
        let state_dir = self.state_dir.clone();
        let run = self.find_mut(run_id).context("no such run")?;
        let pid = run.meta.pid.context("run has no pid")?;
        // SAFETY: negative pid targets the process group the wrapper shell leads.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
        }
        run.meta.state = RunState::Killed;
        run.meta.ended_at = Some(now_secs());
        run.progress.push_log("■ killed by abacus".into());
        persist(&state_dir, &run.meta)?;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(10));
            if pid_alive(pid) {
                // SAFETY: as above; escalation for agents that ignore SIGTERM.
                unsafe {
                    libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
                }
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        r#"{"type":"session","version":3,"id":"0199e7a2-5b1c-7000-8000-000000000001","timestamp":"2026-10-20T12:00:00.000Z","cwd":"/tmp"}"#,
        "\n",
        r#"{"type":"tool_execution_start","toolCallId":"toolu_1","toolName":"read","args":{"path":"/etc/hosts"},"intent":"Reading hosts file"}"#,
        "\n",
        r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"toolCall","id":"toolu_1","name":"read","intent":"Reading hosts file"}],"usage":{"input":10,"output":20,"cost":{"input":0.04,"output":0.005668,"total":0.045668}}}}"#,
        "\n",
        r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"OK"}}"#,
        "\n",
        r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Opened https://github.com/mlg87/dlb/pull/7 then https://github.com/mlg87/dlb/pull/1400\nRESULT: https://github.com/mlg87/dlb/pull/1400 — done"}],"usage":{"cost":{"total":0.0025178}}}}"#,
        "\n",
        r#"{"type":"agent_end","messages":[]}"#,
        "\n",
    );

    fn check(p: &Progress) {
        assert_eq!(
            p.session_id.as_deref(),
            Some("0199e7a2-5b1c-7000-8000-000000000001")
        );
        assert_eq!(p.activity.as_deref(), Some("read: Reading hosts file"));
        assert!((p.cost_usd - 0.0481858).abs() < 1e-9);
        assert_eq!(
            p.result_line.as_deref(),
            Some("RESULT: https://github.com/mlg87/dlb/pull/1400 — done")
        );
        assert_eq!(
            p.pr_url.as_deref(),
            Some("https://github.com/mlg87/dlb/pull/1400")
        );
        assert!(p.agent_ended);
        assert_eq!(p.log.back().map(String::as_str), Some("■ agent finished"));
    }

    #[test]
    fn ingest_extracts_session_activity_cost_result_and_pr() {
        let mut p = Progress::default();
        p.ingest(FIXTURE.as_bytes());
        check(&p);
        assert!(p.partial.is_empty());
    }

    #[test]
    fn ended_run_ingests_unterminated_final_line() {
        let state = tempfile::tempdir().unwrap();
        let repo = Path::new("/repo/a");
        let m = meta("1-tail", repo, None);
        persist(state.path(), &m).unwrap();
        let dir = run_dir(state.path(), "1-tail");
        fs::write(dir.join("events.jsonl"), FIXTURE.trim_end_matches('\n')).unwrap();
        fs::write(dir.join("exit_code"), "0\n").unwrap();

        let reg = Registry::load(state.path(), repo).unwrap();
        let run = reg.find("1-tail").unwrap();
        assert_eq!(run.meta.state, RunState::Succeeded);
        check(&run.progress);
    }

    #[test]
    fn ingest_split_mid_line_matches_single_ingest() {
        let mut whole = Progress::default();
        whole.ingest(FIXTURE.as_bytes());
        let mid_char = FIXTURE.find('—').unwrap() + 1;
        for cut in [1, 57, 200, mid_char, FIXTURE.len() - 3] {
            let mut split = Progress::default();
            split.ingest(&FIXTURE.as_bytes()[..cut]);
            split.ingest(&FIXTURE.as_bytes()[cut..]);
            assert_eq!(split, whole, "cut at {cut}");
        }
    }

    fn meta(run_id: &str, repo: &Path, pid: Option<u32>) -> RunMeta {
        RunMeta {
            run_id: run_id.into(),
            repo_root: repo.into(),
            bead_id: "x-1".into(),
            bead_title: "t".into(),
            mode: Mode::Implement,
            state: RunState::Running,
            worktree: None,
            branch: None,
            pid,
            queued_at: 1,
            started_at: Some(1),
            ended_at: None,
            exit_code: None,
            error: None,
            retry_of: None,
            hidden: false,
        }
    }

    #[test]
    fn load_reconciles_running_runs_from_disk() {
        let state = tempfile::tempdir().unwrap();
        let repo = Path::new("/repo/a");

        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();

        let ok = meta("1-ok", repo, Some(dead_pid));
        persist(state.path(), &ok).unwrap();
        fs::write(run_dir(state.path(), "1-ok").join("exit_code"), "0\n").unwrap();

        let dead = meta("2-dead", repo, Some(dead_pid));
        persist(state.path(), &dead).unwrap();

        let other_repo = meta("3-other", Path::new("/repo/b"), None);
        persist(state.path(), &other_repo).unwrap();

        let reg = Registry::load(state.path(), repo).unwrap();
        assert_eq!(reg.runs.len(), 2);
        let ok = reg.find("1-ok").unwrap();
        assert_eq!(ok.meta.state, RunState::Succeeded);
        assert_eq!(ok.meta.exit_code, Some(0));
        let dead = reg.find("2-dead").unwrap();
        assert_eq!(dead.meta.state, RunState::Failed);
        assert_eq!(
            dead.meta.error.as_deref(),
            Some("agent process ended without exit status")
        );

        // Reconciliation is persisted, not just in memory.
        let reloaded = Registry::load(state.path(), repo).unwrap();
        assert_eq!(
            reloaded.find("2-dead").unwrap().meta.state,
            RunState::Failed
        );
    }
}
