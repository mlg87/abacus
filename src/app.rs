use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::bd;
use crate::dispatch::{self, ActiveRun, Requested, StartJob};
use crate::git;
use crate::model::{Bead, Graph, gh_number};
use crate::runs::{self, Mode, Registry, Run, RunMeta, RunState};
use crate::session;
use crate::view_model::{self, Filter, GraphScope, Layers, Row};

pub enum AppEvent {
    Key(KeyEvent),
    Resize,
    BeadsChanged,
    BeadsLoaded(Result<Vec<Bead>, String>),
    DispatchStarted {
        run_id: String,
        pid: u32,
        worktree: PathBuf,
        branch: Option<String>,
    },
    DispatchFailed {
        run_id: String,
        error: String,
    },
    BdWriteDone {
        message: String,
    },
}

pub struct Config {
    pub repo_root: PathBuf,
    pub state_dir: PathBuf,
    pub max_concurrent: usize,
    pub model: Option<String>,
    pub agent_cmd: String,
    pub repo_name: String,
    pub origin: Option<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Tree,
    Graph,
    Agents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Main,
    Detail,
}

pub struct PlanLine {
    pub bead_id: String,
    pub title: String,
    pub gh: Option<u32>,
    pub outcome: Result<Mode, String>,
    pub retry_of: Option<String>,
}

pub enum Modal {
    Dispatch(Vec<PlanLine>),
    Kill(String),
    Release(String),
    Quit(usize),
    Help,
}

const DEBOUNCE: Duration = Duration::from_millis(300);
const FALLBACK_POLL: Duration = Duration::from_secs(30);
const RUN_POLL: Duration = Duration::from_millis(500);
const MESSAGE_TTL: Duration = Duration::from_secs(5);

pub struct App {
    pub cfg: Config,
    tx: Sender<AppEvent>,

    pub graph: Graph,
    pub loaded_at: Option<Instant>,
    load_started: Option<Instant>,
    loading: bool,
    reload_due: Option<Instant>,
    reload_pending: bool,
    pub bd_error: Option<String>,

    pub filter: Filter,
    pub expanded: HashSet<String>,
    pub marks: HashSet<String>,
    pub rows: Vec<Row>,
    rows_dirty: bool,
    pub sel: Option<String>,
    sel_row: usize,
    pub tree_state: ListState,
    /// Body height of the last frame, for PgUp/PgDn.
    pub page: usize,

    pub view: View,
    pub focus: Focus,
    pub detail_sel: usize,
    pub detail_scroll: u16,

    pub anchor: Option<String>,
    pub scope: GraphScope,
    pub layers: Layers,
    pub members: HashSet<usize>,
    layers_dirty: bool,
    pub graph_hscroll: usize,

    pub registry: Registry,
    queue: VecDeque<String>,
    pub agents_sel: usize,
    /// `None` follows the tail; `Some(n)` pins the first visible log line.
    pub log_scroll: Option<usize>,
    last_poll: Instant,

    pub message: Option<(String, Instant)>,
    pub modal: Option<Modal>,
    pub searching: bool,
    pub quit: bool,
    pub frame: u64,
}

impl App {
    pub fn new(cfg: Config, registry: Registry, tx: Sender<AppEvent>) -> App {
        App {
            cfg,
            tx,
            graph: Graph::default(),
            loaded_at: None,
            load_started: None,
            loading: false,
            reload_due: None,
            reload_pending: false,
            bd_error: None,
            filter: Filter::default(),
            expanded: HashSet::new(),
            marks: HashSet::new(),
            rows: Vec::new(),
            rows_dirty: true,
            sel: None,
            sel_row: 0,
            tree_state: ListState::default(),
            page: 20,
            view: View::Tree,
            focus: Focus::Main,
            detail_sel: 0,
            detail_scroll: 0,
            anchor: None,
            scope: GraphScope::Local,
            layers: Layers::default(),
            members: HashSet::new(),
            layers_dirty: true,
            graph_hscroll: 0,
            registry,
            queue: VecDeque::new(),
            agents_sel: 0,
            log_scroll: None,
            last_poll: Instant::now(),
            message: None,
            modal: None,
            searching: false,
            quit: false,
            frame: 0,
        }
    }

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
    }

    pub fn sel_idx(&self) -> Option<usize> {
        self.graph.get(self.sel.as_deref()?)
    }

    fn select_idx(&mut self, i: usize) {
        if self.sel.as_deref() != Some(self.graph.beads[i].id.as_str()) {
            self.sel = Some(self.graph.beads[i].id.clone());
            self.detail_sel = 0;
            self.detail_scroll = 0;
        }
        if let Some(pos) = self.rows.iter().position(|r| r.idx == i) {
            self.sel_row = pos;
        }
    }

    pub fn sel_row(&self) -> Option<usize> {
        (!self.rows.is_empty()).then_some(self.sel_row.min(self.rows.len() - 1))
    }

    pub fn matching_count(&self) -> usize {
        (0..self.graph.beads.len())
            .filter(|&i| self.filter.matches(&self.graph, i))
            .count()
    }

    pub fn running_count(&self) -> usize {
        self.registry
            .runs
            .iter()
            .filter(|r| matches!(r.meta.state, RunState::Starting | RunState::Running))
            .count()
    }

    pub fn queued_count(&self) -> usize {
        self.queue.len()
    }

    /// Latest visible run per bead, for tree badges and the detail summary.
    pub fn latest_runs(&self) -> HashMap<&str, &Run> {
        let mut m: HashMap<&str, &Run> = HashMap::new();
        for r in self.registry.runs.iter().filter(|r| !r.meta.hidden) {
            m.insert(r.meta.bead_id.as_str(), r);
        }
        m
    }

    // ---------------------------------------------------------------- events

    pub fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Key(k) => self.on_key(k),
            AppEvent::Resize => {}
            AppEvent::BeadsChanged => self.reload_due = Some(Instant::now() + DEBOUNCE),
            AppEvent::BeadsLoaded(res) => self.on_loaded(res),
            AppEvent::DispatchStarted {
                run_id,
                pid,
                worktree,
                branch,
            } => {
                if let Some(r) = self.registry.find_mut(&run_id) {
                    // A kill can land between spawn and this event; keep it.
                    if r.meta.state == RunState::Starting {
                        r.meta.state = RunState::Running;
                    }
                    r.meta.pid = Some(pid);
                    r.meta.started_at = Some(runs::now_secs());
                    r.meta.worktree = Some(worktree);
                    r.meta.branch = branch;
                    let _ = runs::persist(&self.cfg.state_dir, &r.meta);
                }
                self.request_reload();
            }
            AppEvent::DispatchFailed { run_id, error } => {
                if let Some(r) = self.registry.find_mut(&run_id) {
                    r.meta.state = RunState::Failed;
                    r.meta.error = Some(error.clone());
                    r.meta.ended_at = Some(runs::now_secs());
                    let _ = runs::persist(&self.cfg.state_dir, &r.meta);
                    let bead = r.meta.bead_id.clone();
                    self.flash(format!("dispatch of {bead} failed: {error}"));
                }
            }
            AppEvent::BdWriteDone { message } => {
                self.flash(message);
                self.request_reload();
            }
        }
    }

    fn on_loaded(&mut self, res: Result<Vec<Bead>, String>) {
        self.loading = false;
        match res {
            Ok(beads) => {
                self.graph = Graph::build(beads);
                self.loaded_at = Some(Instant::now());
                self.bd_error = None;
                self.rows_dirty = true;
                self.layers_dirty = true;
            }
            Err(e) => self.bd_error = Some(e),
        }
        if std::mem::take(&mut self.reload_pending) {
            self.start_reload();
        }
    }

    pub fn request_reload(&mut self) {
        if self.loading {
            self.reload_pending = true;
        } else {
            self.start_reload();
        }
    }

    fn start_reload(&mut self) {
        self.loading = true;
        self.load_started = Some(Instant::now());
        self.reload_due = None;
        let tx = self.tx.clone();
        let repo = self.cfg.repo_root.clone();
        std::thread::spawn(move || {
            let res = bd::list_all(&repo).map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::BeadsLoaded(res));
        });
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        if !self.loading {
            let due = self.reload_due.is_some_and(|d| now >= d);
            let stale = self
                .load_started
                .is_none_or(|s| now.duration_since(s) >= FALLBACK_POLL);
            if due || stale {
                self.start_reload();
            }
        }
        if now.duration_since(self.last_poll) >= RUN_POLL {
            self.last_poll = now;
            self.registry.poll();
        }
        self.drain_queue();
        if self
            .message
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) >= MESSAGE_TTL)
        {
            self.message = None;
        }
        self.refresh_views();
    }

    pub fn refresh_views(&mut self) {
        if self.rows_dirty {
            self.rows_dirty = false;
            self.rows = view_model::flatten(&self.graph, &self.filter, &self.expanded);
            let found = self
                .sel_idx()
                .and_then(|i| self.rows.iter().position(|r| r.idx == i));
            match found {
                Some(pos) => self.sel_row = pos,
                None if self.rows.is_empty() => {}
                None => {
                    // Selected bead vanished (closed under filter, or deleted): clamp.
                    self.sel_row = self.sel_row.min(self.rows.len() - 1);
                    let i = self.rows[self.sel_row].idx;
                    self.sel = Some(self.graph.beads[i].id.clone());
                }
            }
        }
        if self.layers_dirty {
            self.layers_dirty = false;
            let anchor = self
                .anchor
                .as_deref()
                .and_then(|a| self.graph.get(a))
                .or_else(|| self.sel_idx());
            let members = match anchor {
                Some(a) => view_model::scope_members(&self.graph, a, self.scope, &self.filter),
                None if self.scope == GraphScope::AllOpen => (0..self.graph.beads.len())
                    .filter(|&i| self.filter.matches(&self.graph, i))
                    .collect(),
                None => Vec::new(),
            };
            self.layers = view_model::layers(&self.graph, &members);
            self.members = members.into_iter().collect();
        }
    }

    fn invalidate(&mut self) {
        self.rows_dirty = true;
        self.layers_dirty = true;
    }

    // ---------------------------------------------------------------- queue

    fn active_runs(&self, except: Option<&str>) -> Vec<ActiveRun<'_>> {
        self.registry
            .runs
            .iter()
            .filter(|r| r.meta.state.is_active() && Some(r.meta.run_id.as_str()) != except)
            .map(|r| ActiveRun {
                run_id: &r.meta.run_id,
                bead_id: &r.meta.bead_id,
                mode: r.meta.mode,
            })
            .collect()
    }

    fn enqueue(&mut self, bead_id: &str, mode: Mode, retry_of: Option<String>) {
        let Some(i) = self.graph.get(bead_id) else {
            return;
        };
        let meta = RunMeta {
            run_id: self.registry.new_run_id(bead_id),
            repo_root: self.cfg.repo_root.clone(),
            bead_id: bead_id.to_string(),
            bead_title: self.graph.beads[i].title.clone(),
            mode,
            state: RunState::Queued,
            worktree: None,
            branch: None,
            pid: None,
            queued_at: runs::now_secs(),
            started_at: None,
            ended_at: None,
            exit_code: None,
            error: None,
            retry_of,
            hidden: false,
        };
        self.queue.push_back(meta.run_id.clone());
        self.registry.runs.push(Run {
            meta,
            progress: Default::default(),
        });
    }

    fn drain_queue(&mut self) {
        while self.running_count() < self.cfg.max_concurrent {
            let Some(run_id) = self.queue.pop_front() else {
                return;
            };
            let Some(run) = self.registry.find(&run_id) else {
                continue;
            };
            if run.meta.state != RunState::Queued {
                continue;
            }
            let meta = run.meta.clone();
            let outcome = match self.graph.get(&meta.bead_id) {
                None => Err("bead no longer exists".to_string()),
                Some(i) => {
                    let req = if meta.mode == Mode::Decompose {
                        Requested::Decompose
                    } else {
                        Requested::Default
                    };
                    let retry_mode = meta.retry_of.as_ref().map(|_| meta.mode);
                    dispatch::plan_dispatch(
                        &self.graph,
                        &self.active_runs(Some(&run_id)),
                        i,
                        req,
                        retry_mode,
                    )
                    .map(|m| (i, m))
                }
            };
            match outcome {
                Err(reason) => {
                    let r = self.registry.find_mut(&run_id).expect("found above");
                    r.meta.state = RunState::Failed;
                    r.meta.error = Some(format!("skipped at start: {reason}"));
                    r.meta.ended_at = Some(runs::now_secs());
                    let _ = runs::persist(&self.cfg.state_dir, &r.meta);
                }
                Ok((i, mode)) => self.start(i, mode, meta),
            }
        }
    }

    fn start(&mut self, i: usize, mode: Mode, mut meta: RunMeta) {
        meta.mode = mode;
        meta.state = RunState::Starting;
        let base = git::default_base(&self.cfg.repo_root);
        let plan = dispatch::plan_worktree(
            &self.cfg.repo_root,
            &self.cfg.state_dir,
            &self.graph,
            i,
            mode,
            &meta.run_id,
            &base,
        );
        let previous = meta
            .retry_of
            .as_deref()
            .and_then(|id| self.registry.find(id));
        let prompt = dispatch::prompt_ctx(
            &self.graph,
            i,
            mode,
            &self.cfg.repo_root,
            self.cfg.origin.as_ref(),
            &plan,
            previous,
        );
        if let Some(r) = self.registry.find_mut(&meta.run_id) {
            r.meta = meta.clone();
        }
        if let Err(e) = runs::persist(&self.cfg.state_dir, &meta) {
            self.handle(AppEvent::DispatchFailed {
                run_id: meta.run_id,
                error: format!("{e:#}"),
            });
            return;
        }
        let job = StartJob {
            repo_root: self.cfg.repo_root.clone(),
            state_dir: self.cfg.state_dir.clone(),
            agent_cmd: self.cfg.agent_cmd.clone(),
            model: self.cfg.model.clone(),
            meta,
            plan,
            prompt,
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || dispatch::start_run(job, &tx));
    }

    // ---------------------------------------------------------------- keys

    fn on_key(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            if self.modal.is_none() || matches!(self.modal, Some(Modal::Help)) {
                self.modal = None;
                self.request_quit();
            } else {
                self.quit = true;
            }
            return;
        }
        if self.modal.is_some() {
            self.on_modal_key(k);
            return;
        }
        if self.searching {
            self.on_search_key(k);
            return;
        }
        if self.focus == Focus::Detail && self.on_detail_key(k) {
            return;
        }
        match k.code {
            KeyCode::Char('q') => return self.request_quit(),
            KeyCode::Char('?') => return self.modal = Some(Modal::Help),
            KeyCode::Char('1') => return self.set_view(View::Tree),
            KeyCode::Char('2') => {
                self.anchor = self.sel.clone();
                self.layers_dirty = true;
                return self.set_view(View::Graph);
            }
            KeyCode::Char('3') => return self.set_view(View::Agents),
            KeyCode::Char('r') => {
                self.request_reload();
                return self.flash("reloading");
            }
            KeyCode::Char('f') => {
                self.filter.status = self.filter.status.next();
                return self.invalidate();
            }
            KeyCode::Char('L') => return self.cycle_label(),
            KeyCode::Char('/') => {
                self.searching = true;
                return;
            }
            _ => {}
        }
        match self.view {
            View::Tree => self.on_tree_key(k),
            View::Graph => self.on_graph_key(k),
            View::Agents => self.on_agents_key(k),
        }
    }

    fn set_view(&mut self, v: View) {
        self.view = v;
        self.focus = Focus::Main;
    }

    fn request_quit(&mut self) {
        let n = self.queued_count();
        if n > 0 {
            self.modal = Some(Modal::Quit(n));
        } else {
            self.quit = true;
        }
    }

    fn cycle_label(&mut self) {
        let mut labels: Vec<&str> = self
            .graph
            .beads
            .iter()
            .flat_map(|b| b.labels.iter().map(String::as_str))
            .collect();
        labels.sort_unstable();
        labels.dedup();
        let next = match &self.filter.label {
            None => labels.first().map(|s| s.to_string()),
            Some(cur) => labels
                .iter()
                .position(|l| l == cur)
                .and_then(|p| labels.get(p + 1))
                .map(|s| s.to_string()),
        };
        self.filter.label = next;
        self.invalidate();
    }

    fn on_modal_key(&mut self, k: KeyEvent) {
        let yes = matches!(k.code, KeyCode::Char('y') | KeyCode::Enter);
        let no = matches!(k.code, KeyCode::Char('n') | KeyCode::Esc);
        match self.modal.take() {
            Some(Modal::Help) => {
                if !matches!(
                    k.code,
                    KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q')
                ) {
                    self.modal = Some(Modal::Help);
                }
            }
            Some(m) if no => drop(m),
            Some(Modal::Dispatch(lines)) if yes => {
                let mut n = 0;
                for l in &lines {
                    if let Ok(mode) = l.outcome {
                        self.enqueue(&l.bead_id, mode, l.retry_of.clone());
                        n += 1;
                    }
                }
                self.marks.clear();
                self.flash(format!("queued {n} run(s)"));
            }
            Some(Modal::Kill(run_id)) if yes => self.kill(&run_id),
            Some(Modal::Release(run_id)) if yes => self.release(&run_id),
            Some(Modal::Quit(_)) if yes => self.quit = true,
            other => self.modal = other,
        }
    }

    fn on_search_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => {
                self.searching = false;
                self.filter.query.clear();
            }
            KeyCode::Enter => self.searching = false,
            KeyCode::Backspace => {
                self.filter.query.pop();
            }
            KeyCode::Char(c) => self.filter.query.extend(c.to_lowercase()),
            _ => return,
        }
        self.invalidate();
        if self.filter.query.is_empty() {
            return;
        }
        // Search auto-expands to matches; land on the first one unless the
        // current selection already matches.
        self.refresh_views();
        let on_match = self.sel_row().is_some_and(|p| !self.rows[p].context_only);
        if !on_match && let Some(pos) = self.rows.iter().position(|r| !r.context_only) {
            self.sel_row = pos;
            self.select_idx(self.rows[pos].idx);
        }
    }

    // ---------------------------------------------------------------- tree

    fn move_rows(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let cur = self.sel_row().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, self.rows.len() as isize - 1) as usize;
        self.sel_row = next;
        self.select_idx(self.rows[next].idx);
    }

    fn on_tree_key(&mut self, k: KeyEvent) {
        let page = self.page.max(1) as isize;
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_rows(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_rows(1),
            KeyCode::PageUp => self.move_rows(-page),
            KeyCode::PageDown => self.move_rows(page),
            KeyCode::Home | KeyCode::Char('g') => self.move_rows(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_rows(isize::MAX / 2),
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some(r) = self.sel_row().map(|p| self.rows[p].clone())
                    && r.has_visible_children
                {
                    self.expanded.insert(self.graph.beads[r.idx].id.clone());
                    self.rows_dirty = true;
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                let Some(r) = self.sel_row().map(|p| self.rows[p].clone()) else {
                    return;
                };
                let id = self.graph.beads[r.idx].id.clone();
                if r.expanded && self.expanded.remove(&id) {
                    self.rows_dirty = true;
                } else if let Some(p) = self.graph.parent[r.idx] {
                    self.select_idx(p);
                }
            }
            KeyCode::Char('e') => {
                for (i, c) in self.graph.children.iter().enumerate() {
                    if !c.is_empty() {
                        self.expanded.insert(self.graph.beads[i].id.clone());
                    }
                }
                self.rows_dirty = true;
            }
            KeyCode::Char('E') => {
                self.expanded.clear();
                self.rows_dirty = true;
            }
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Char('d') => self.open_dispatch(Requested::Default),
            KeyCode::Char('D') => self.open_dispatch(Requested::Decompose),
            KeyCode::Tab => {
                if self.sel.is_some() {
                    self.focus = Focus::Detail;
                }
            }
            KeyCode::Esc => {
                if !self.filter.query.is_empty() {
                    self.filter.query.clear();
                    self.invalidate();
                } else {
                    self.marks.clear();
                }
            }
            _ => {}
        }
    }

    fn toggle_mark(&mut self) {
        let Some(id) = self.sel.clone() else { return };
        if !self.marks.remove(&id) {
            self.marks.insert(id);
        }
    }

    /// Relationship entries of the selected bead as `(section, bead idx)`.
    pub fn detail_entries(&self) -> Vec<(String, usize)> {
        let Some(i) = self.sel_idx() else {
            return Vec::new();
        };
        let g = &self.graph;
        let mut out = Vec::new();
        if let Some(p) = g.parent[i] {
            out.push(("Parent".to_string(), p));
        }
        let n = g.children[i].len();
        out.extend(
            g.children[i]
                .iter()
                .map(|&c| (format!("Children ({n})"), c)),
        );
        out.extend(g.blockers[i].iter().map(|&b| ("Blocked by".to_string(), b)));
        out.extend(g.dependents[i].iter().map(|&d| ("Blocks".to_string(), d)));
        out.extend(
            g.other_links[i]
                .iter()
                .map(|(t, j)| (format!("Other links ({t})"), *j)),
        );
        out
    }

    /// Returns true if the key was consumed by the focused detail pane.
    fn on_detail_key(&mut self, k: KeyEvent) -> bool {
        let n = self.detail_entries().len();
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.detail_sel = self.detail_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.detail_sel = (self.detail_sel + 1).min(n.saturating_sub(1));
            }
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(10),
            KeyCode::PageDown => self.detail_scroll = self.detail_scroll.saturating_add(10),
            KeyCode::Esc | KeyCode::Tab => self.focus = Focus::Main,
            KeyCode::Enter => {
                if let Some(&(_, j)) = self.detail_entries().get(self.detail_sel) {
                    let id = self.graph.beads[j].id.clone();
                    self.reveal(&id);
                    self.focus = Focus::Main;
                }
            }
            _ => return false,
        }
        true
    }

    /// Selects `id` in the tree: expands its ancestors and drops any filter
    /// component that would hide it.
    pub fn reveal(&mut self, id: &str) {
        let Some(i) = self.graph.get(id) else {
            return self.flash(format!("{id} is not loaded"));
        };
        let g = &self.graph;
        let b = &g.beads[i];
        if !self.filter.query.is_empty()
            && !b.id.to_lowercase().contains(&self.filter.query)
            && !b.title.to_lowercase().contains(&self.filter.query)
        {
            self.filter.query.clear();
        }
        if self
            .filter
            .label
            .as_ref()
            .is_some_and(|l| !b.labels.contains(l))
        {
            self.filter.label = None;
        }
        if !self.filter.matches(g, i) {
            self.filter.status = view_model::StatusFilter::All;
        }
        for a in g.ancestors(i) {
            self.expanded.insert(g.beads[a].id.clone());
        }
        self.sel = Some(id.to_string());
        self.detail_sel = 0;
        self.detail_scroll = 0;
        self.invalidate();
        self.refresh_views();
    }

    // ---------------------------------------------------------------- graph

    /// Columns as rendered: layer columns, then the cycle column if any.
    pub fn graph_columns(&self) -> Vec<&[usize]> {
        let mut cols: Vec<&[usize]> = self.layers.columns.iter().map(Vec::as_slice).collect();
        if !self.layers.cycle.is_empty() {
            cols.push(&self.layers.cycle);
        }
        cols
    }

    pub fn graph_pos(&self) -> Option<(usize, usize)> {
        let i = self.sel_idx()?;
        self.graph_columns()
            .iter()
            .enumerate()
            .find_map(|(c, col)| col.iter().position(|&x| x == i).map(|r| (c, r)))
    }

    fn graph_select(&mut self, col: usize, row: usize) {
        let cols = self.graph_columns();
        let Some(c) = cols.get(col) else { return };
        if c.is_empty() {
            return;
        }
        let i = c[row.min(c.len() - 1)];
        let id = self.graph.beads[i].id.clone();
        // Keep the tree in sync: the card must exist as a tree row.
        for a in self.graph.ancestors(i) {
            if self.expanded.insert(self.graph.beads[a].id.clone()) {
                self.rows_dirty = true;
            }
        }
        self.select_idx(i);
        self.sel = Some(id);
        self.refresh_views();
        if let Some(pos) = self.rows.iter().position(|r| r.idx == i) {
            self.sel_row = pos;
        }
    }

    fn on_graph_key(&mut self, k: KeyEvent) {
        let ncols = self.graph_columns().len();
        if ncols == 0 {
            if k.code == KeyCode::Char('s') {
                self.toggle_scope();
            } else if k.code == KeyCode::Backspace {
                self.anchor_up();
            }
            return;
        }
        let Some((c, r)) = self.graph_pos() else {
            // Selection is outside the scope: the first nav key lands on the first card.
            match k.code {
                KeyCode::Char('s') => self.toggle_scope(),
                KeyCode::Backspace => self.anchor_up(),
                _ => self.graph_select(0, 0),
            }
            return;
        };
        match k.code {
            KeyCode::Left | KeyCode::Char('h') => self.graph_select(c.saturating_sub(1), r),
            KeyCode::Right | KeyCode::Char('l') => self.graph_select((c + 1).min(ncols - 1), r),
            KeyCode::Up | KeyCode::Char('k') => self.graph_select(c, r.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.graph_select(c, r + 1),
            KeyCode::Enter => {
                if let Some(i) = self.sel_idx()
                    && !self.graph.children[i].is_empty()
                {
                    self.anchor = self.sel.clone();
                    self.layers_dirty = true;
                    self.refresh_views();
                    self.graph_select(0, 0);
                }
            }
            KeyCode::Backspace => self.anchor_up(),
            KeyCode::Char('s') => self.toggle_scope(),
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Char('d') => self.open_dispatch(Requested::Default),
            KeyCode::Char('D') => self.open_dispatch(Requested::Decompose),
            _ => {}
        }
    }

    fn toggle_scope(&mut self) {
        self.scope = match self.scope {
            GraphScope::Local => GraphScope::AllOpen,
            GraphScope::AllOpen => GraphScope::Local,
        };
        self.layers_dirty = true;
        self.graph_hscroll = 0;
    }

    fn anchor_up(&mut self) {
        let parent = self
            .anchor
            .as_deref()
            .and_then(|a| self.graph.get(a))
            .and_then(|a| self.graph.parent[a]);
        if let Some(p) = parent {
            self.anchor = Some(self.graph.beads[p].id.clone());
            self.layers_dirty = true;
            self.graph_hscroll = 0;
        }
    }

    // ---------------------------------------------------------------- dispatch

    fn open_dispatch(&mut self, req: Requested) {
        let mut targets: Vec<usize> = if self.marks.is_empty() {
            self.sel_idx().into_iter().collect()
        } else {
            self.marks
                .iter()
                .filter_map(|id| self.graph.get(id))
                .collect()
        };
        if targets.is_empty() {
            return self.flash("nothing selected");
        }
        self.graph.sort(&mut targets);

        // Plan in order, treating earlier accepted lines as active so a batch
        // cannot contain both an epic's resolve and one of its children.
        let mut pending: Vec<(String, Mode)> = Vec::new();
        let mut lines = Vec::new();
        for i in targets {
            let b = &self.graph.beads[i];
            let outcome = {
                let mut active = self.active_runs(None);
                active.extend(pending.iter().map(|(id, m)| ActiveRun {
                    run_id: "(this batch)",
                    bead_id: id,
                    mode: *m,
                }));
                dispatch::plan_dispatch(&self.graph, &active, i, req, None)
            };
            if let Ok(m) = outcome {
                pending.push((b.id.clone(), m));
            }
            lines.push(PlanLine {
                bead_id: b.id.clone(),
                title: b.title.clone(),
                gh: gh_number(b),
                outcome,
                retry_of: None,
            });
        }
        if lines.iter().all(|l| l.outcome.is_err()) {
            let reasons: Vec<String> = lines
                .iter()
                .map(|l| format!("{}: {}", l.bead_id, l.outcome.as_ref().unwrap_err()))
                .collect();
            return self.flash(format!("nothing to dispatch — {}", reasons.join("; ")));
        }
        self.modal = Some(Modal::Dispatch(lines));
    }

    // ---------------------------------------------------------------- agents

    /// Visible runs in display order: running, starting, queued, then ended (newest first).
    pub fn agent_rows(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.registry.runs.len())
            .filter(|&i| !self.registry.runs[i].meta.hidden)
            .collect();
        let rank = |s: RunState| match s {
            RunState::Running => 0,
            RunState::Starting => 1,
            RunState::Queued => 2,
            _ => 3,
        };
        v.sort_by(|&a, &b| {
            let (ma, mb) = (&self.registry.runs[a].meta, &self.registry.runs[b].meta);
            rank(ma.state)
                .cmp(&rank(mb.state))
                .then_with(|| match ma.state {
                    RunState::Queued => ma.queued_at.cmp(&mb.queued_at),
                    s if rank(s) == 3 => mb.ended_at.cmp(&ma.ended_at),
                    _ => ma.started_at.cmp(&mb.started_at),
                })
                .then_with(|| ma.run_id.cmp(&mb.run_id))
        });
        v
    }

    pub fn selected_run(&self) -> Option<&Run> {
        let rows = self.agent_rows();
        let i = *rows.get(self.agents_sel.min(rows.len().saturating_sub(1)))?;
        self.registry.runs.get(i)
    }

    fn on_agents_key(&mut self, k: KeyEvent) {
        let n = self.agent_rows().len();
        let run = self.selected_run().map(|r| r.meta.clone());
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.agents_sel = self.agents_sel.min(n.saturating_sub(1)).saturating_sub(1);
                self.log_scroll = None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.agents_sel = (self.agents_sel + 1).min(n.saturating_sub(1));
                self.log_scroll = None;
            }
            KeyCode::PageUp => {
                let len = self.selected_run().map_or(0, |r| r.progress.log.len());
                let top = self.log_scroll.unwrap_or(len.saturating_sub(self.page));
                self.log_scroll = Some(top.saturating_sub(self.page.max(1)));
            }
            KeyCode::PageDown => {
                if let Some(top) = self.log_scroll {
                    let len = self.selected_run().map_or(0, |r| r.progress.log.len());
                    let next = top + self.page.max(1);
                    self.log_scroll = (next + self.page < len).then_some(next);
                }
            }
            KeyCode::Enter => {
                if let Some(m) = run {
                    self.reveal(&m.bead_id);
                    self.set_view(View::Tree);
                }
            }
            KeyCode::Char('K') => match run.map(|m| (m.state, m.run_id)) {
                Some((RunState::Running, id)) => self.modal = Some(Modal::Kill(id)),
                Some((RunState::Queued, id)) => self.cancel_queued(&id),
                Some((RunState::Starting, _)) => {
                    self.flash("run is starting; kill it once running")
                }
                Some(_) => self.flash("run has already ended"),
                None => {}
            },
            KeyCode::Char('R') => {
                if let Some(m) = run {
                    self.retry(&m);
                }
            }
            KeyCode::Char('X') => match run {
                Some(m) if !m.state.is_active() && m.mode != Mode::Decompose => {
                    self.modal = Some(Modal::Release(m.run_id));
                }
                Some(m) if m.mode == Mode::Decompose => {
                    self.flash("decompose runs never claim their bead")
                }
                Some(_) => self.flash("release applies to ended runs"),
                None => {}
            },
            KeyCode::Char('o') => {
                if let Some(m) = run {
                    self.open_session(&m);
                }
            }
            KeyCode::Char('C') => self.clear_ended(),
            _ => {}
        }
    }

    fn cancel_queued(&mut self, run_id: &str) {
        self.queue.retain(|q| q != run_id);
        if let Some(r) = self.registry.find_mut(run_id) {
            r.meta.state = RunState::Killed;
            r.meta.ended_at = Some(runs::now_secs());
            r.meta.error = Some("cancelled while queued".into());
        }
        self.flash("queued run cancelled");
    }

    fn kill(&mut self, run_id: &str) {
        match self.registry.kill(run_id) {
            Ok(()) => self.flash(format!("killed {run_id}")),
            Err(e) => self.flash(format!("kill failed: {e:#}")),
        }
    }

    fn retry(&mut self, m: &RunMeta) {
        if m.state.is_active() {
            return self.flash("run is still active");
        }
        let Some(i) = self.graph.get(&m.bead_id) else {
            return self.flash(format!("{} is not loaded", m.bead_id));
        };
        let req = if m.mode == Mode::Decompose {
            Requested::Decompose
        } else {
            Requested::Default
        };
        match dispatch::plan_dispatch(&self.graph, &self.active_runs(None), i, req, Some(m.mode)) {
            Ok(mode) => {
                self.enqueue(&m.bead_id, mode, Some(m.run_id.clone()));
                self.flash(format!("retry of {} queued", m.bead_id));
            }
            Err(reason) => self.flash(format!("cannot retry {}: {reason}", m.bead_id)),
        }
    }

    fn release(&mut self, run_id: &str) {
        let Some(m) = self.registry.find(run_id).map(|r| r.meta.clone()) else {
            return;
        };
        let tx = self.tx.clone();
        let repo = self.cfg.repo_root.clone();
        std::thread::spawn(move || {
            let note = format!("abacus: released after run {}", m.run_id);
            let message = match bd::release(&repo, &m.bead_id, &note) {
                Ok(()) => format!(
                    "released {}; worktree kept at {}",
                    m.bead_id,
                    m.worktree
                        .as_ref()
                        .map_or("-".into(), |p| p.display().to_string())
                ),
                Err(e) => format!("release of {} failed: {e:#}", m.bead_id),
            };
            let _ = tx.send(AppEvent::BdWriteDone { message });
        });
    }

    pub fn session_command(&self, m: &RunMeta) -> Option<String> {
        let wt = m.worktree.as_ref()?.to_string_lossy().into_owned();
        let sess = runs::run_dir(&self.cfg.state_dir, &m.run_id)
            .join("session")
            .to_string_lossy()
            .into_owned();
        let q = session::shell_quote;
        Some(format!(
            "cd {wt_q} && {cmd} --cwd {wt_q} --session-dir {sess} --continue",
            wt_q = q(&wt),
            cmd = q(&self.cfg.agent_cmd),
            sess = q(&sess),
        ))
    }

    fn open_session(&mut self, m: &RunMeta) {
        match m.state {
            RunState::Starting | RunState::Running => {
                return self.flash("kill it first (K) or wait");
            }
            RunState::Queued => return self.flash("run has not started"),
            _ => {}
        }
        let Some(cmd) = self.session_command(m) else {
            return self.flash("run has no worktree");
        };
        match session::open(&cmd) {
            Ok(msg) => self.flash(msg),
            Err(e) => self.flash(format!("open session failed: {e:#}")),
        }
    }

    fn clear_ended(&mut self) {
        let mut scratch = Vec::new();
        let mut n = 0;
        for r in self
            .registry
            .runs
            .iter_mut()
            .filter(|r| !r.meta.hidden && !r.meta.state.is_active())
        {
            r.meta.hidden = true;
            n += 1;
            // Queued-then-cancelled runs were never persisted.
            if r.meta.started_at.is_some() || r.meta.worktree.is_some() || r.meta.pid.is_some() {
                let _ = runs::persist(&self.cfg.state_dir, &r.meta);
            }
            if r.meta.mode == Mode::Decompose
                && let Some(wt) = &r.meta.worktree
            {
                scratch.push(wt.clone());
            }
        }
        self.agents_sel = 0;
        self.log_scroll = None;
        self.flash(format!("hid {n} ended run(s)"));
        if scratch.is_empty() {
            return;
        }
        let tx = self.tx.clone();
        let repo = self.cfg.repo_root.clone();
        std::thread::spawn(move || {
            let failed: Vec<String> = scratch
                .iter()
                .filter_map(|p| {
                    let s = p.to_string_lossy();
                    git::git(&repo, &["worktree", "remove", "--force", &s])
                        .err()
                        .map(|e| format!("{e:#}"))
                })
                .collect();
            let message = if failed.is_empty() {
                format!("removed {} scratch worktree(s)", scratch.len())
            } else {
                format!("scratch cleanup: {}", failed.join("; "))
            };
            let _ = tx.send(AppEvent::BdWriteDone { message });
        });
    }
}
