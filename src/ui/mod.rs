mod agents;
mod detail;
mod graph;
mod theme;
mod tree;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::app::{App, Modal, View};
use crate::model::Derived;
use crate::runs::{Run, RunState};
use crate::update::{Status as UpdateStatus, Version};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn spinner(frame: u64) -> &'static str {
    SPINNER[(frame % SPINNER.len() as u64) as usize]
}

pub fn run_badge(run: &Run, frame: u64) -> Span<'static> {
    match run.meta.state {
        RunState::Queued => Span::styled("⏳", Style::new().fg(theme::CYAN)),
        RunState::Starting | RunState::Running => Span::styled(
            format!("{} running", spinner(frame)),
            Style::new().fg(Color::Yellow),
        ),
        RunState::Succeeded => Span::styled("✓", Style::new().fg(Color::Green)),
        RunState::Failed | RunState::Killed => Span::styled("✗", Style::new().fg(Color::Red)),
    }
}

pub fn state_style(s: RunState) -> Style {
    match s {
        RunState::Queued => Style::new().fg(theme::CYAN),
        RunState::Starting | RunState::Running => Style::new().fg(Color::Yellow),
        RunState::Succeeded => Style::new().fg(Color::Green),
        RunState::Failed | RunState::Killed => Style::new().fg(Color::Red),
    }
}

pub fn fmt_duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Renders unix seconds as `HH:MM:SS YYYY-MM-DD` in the machine's timezone. A
/// fixed timestamp replaced "Ns ago" in the Agents tab: a counter that never
/// stops ticking says nothing the Elapsed column doesn't.
pub fn fmt_local_time(secs: u64) -> String {
    let t = secs as libc::time_t;
    // SAFETY: `tm` is plain data; localtime_r is the reentrant variant and only
    // writes into the buffer we pass it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return "-".into();
    }
    format!(
        "{:02}:{:02}:{:02} {:04}-{:02}-{:02}",
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday
    )
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn draw(f: &mut Frame, app: &mut App) {
    app.frame = app.frame.wrapping_add(1);
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());
    app.page = body.height.saturating_sub(2).max(1) as usize;

    f.render_widget(Paragraph::new(top_bar(app)), top);
    match app.view {
        View::Tree => tree::draw(f, app, body),
        View::Graph => graph::draw(f, app, body),
        View::Agents => agents::draw(f, app, body),
    }
    f.render_widget(Paragraph::new(bottom_bar(app)), bottom);
    draw_modal(f, app);
}

fn top_bar(app: &App) -> Line<'static> {
    let dim = Style::new().fg(Color::DarkGray);
    let tab = |n: &str, name: &str, v: View| {
        let style = if app.view == v {
            Style::new().fg(Color::Black).bg(theme::MAGENTA)
        } else {
            Style::new()
        };
        Span::styled(format!("[{n} {name}]"), style)
    };
    let mut spans = theme::wordmark("abacus");
    spans.extend([
        Span::styled(format!(" {}", Version::current()), dim),
        Span::styled(" · ", dim),
        Span::raw(app.cfg.repo_name.clone()),
        Span::styled(" · ", dim),
        tab("1", "Tree", View::Tree),
        Span::raw(" "),
        tab("2", "Graph", View::Graph),
        Span::raw(" "),
        tab("3", "Agents", View::Agents),
        Span::styled(" · ", dim),
        Span::raw(format!(
            "filter:{} label:{}",
            app.filter.status.label(),
            app.filter.label.as_deref().unwrap_or("all")
        )),
    ]);
    if !app.filter.query.is_empty() {
        spans.push(Span::raw(format!(" /{}", app.filter.query)));
    }
    spans.push(Span::styled(" · ", dim));
    spans.push(Span::raw(format!("{} matching", app.matching_count())));
    spans.push(Span::styled(" · ", dim));
    spans.push(Span::raw(format!(
        "runs {} running {} queued (cap {})",
        app.running_count(),
        app.queued_count(),
        app.cfg.max_concurrent
    )));
    spans.push(Span::styled(" · ", dim));
    spans.push(match app.loaded_at {
        Some(t) => Span::raw(format!("loaded {}s ago", t.elapsed().as_secs())),
        None => Span::styled("loading…", dim),
    });
    if let Some(secs) = app.slow_load_secs() {
        spans.push(Span::styled(
            format!(" · ⚠ bd list waiting {secs}s (database busy?)"),
            Style::new().fg(Color::Yellow),
        ));
    }
    if let Some(e) = &app.bd_error {
        spans.push(Span::styled(
            format!(" · ⚠ bd: {e}"),
            Style::new().fg(Color::Red),
        ));
    }
    Line::from(spans)
}

fn bottom_bar(app: &App) -> Line<'static> {
    if app.searching {
        return Line::from(vec![
            Span::styled("/", Style::new().fg(theme::MAGENTA)),
            Span::raw(app.filter.query.clone()),
            Span::styled("█", Style::new().fg(theme::MAGENTA)),
            Span::styled("  Enter keep · Esc clear", Style::new().fg(Color::DarkGray)),
        ]);
    }
    if let Some((m, _)) = &app.message {
        return Line::from(Span::styled(m.clone(), Style::new().fg(theme::MAGENTA)));
    }
    let hints = match (app.view, app.focus) {
        (View::Tree, crate::app::Focus::Detail) => {
            "↑↓ relationship · Enter jump · PgUp/PgDn scroll · Esc/Tab back"
        }
        (View::Tree, _) => {
            "↑↓ move · →← expand/collapse · e/E all · space mark · d dispatch · A dispatch all ready · D decompose · X delete · Tab detail · / search · f filter · L label · ? help · q quit"
        }
        (View::Graph, _) => {
            "←→↑↓ move · Enter drill in · Backspace up · s scope · space mark · d dispatch · D decompose · X delete · ? help · q quit"
        }
        (View::Agents, _) => {
            "↑↓ run · Enter bead · K kill · R retry · Q drain queue · o session · C clear ended · PgUp/PgDn log · ? help · q quit"
        }
    };
    // Leads the line so a narrow terminal clips the hints, not the update.
    let mut spans = match app.update {
        Some(UpdateStatus::Available(v)) => vec![Span::styled(
            format!(" U update to {v} "),
            Style::new()
                .fg(Color::Black)
                .bg(theme::CYAN)
                .add_modifier(Modifier::BOLD),
        )],
        Some(UpdateStatus::Installing(v)) => vec![Span::styled(
            format!("{} installing {v}…", spinner(app.frame)),
            Style::new().fg(theme::CYAN),
        )],
        Some(UpdateStatus::Installed(v)) => vec![Span::styled(
            format!("✓ {v} installed · relaunch to use it"),
            Style::new().fg(Color::Green),
        )],
        None => Vec::new(),
    };
    if !spans.is_empty() {
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(hints, Style::new().fg(Color::DarkGray)));
    Line::from(spans)
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// Explains the tree/graph status glyphs. Leads the help so it survives a short
/// terminal clipping the modal; the glyph and colour come from `Derived` itself.
const LEGEND: &[(Derived, &str)] = &[
    (
        Derived::Ready,
        "an agent can start now; epic: some open child is ready",
    ),
    (
        Derived::Blocked,
        "open blocker or blocked ancestor; epic: no child ready or in progress",
    ),
    (
        Derived::InProgress,
        "being worked; epic: no child ready, some in progress",
    ),
    (Derived::Deferred, "deferred; not dispatchable"),
    (Derived::Closed, "done"),
    (Derived::Other, "status abacus does not recognise"),
];

const HELP: &[(&str, &str)] = &[
    ("Global", ""),
    ("1 / 2 / 3", "tree / graph / agents view"),
    ("f", "cycle status filter (Open, Ready, InProgress, All)"),
    ("L", "cycle label filter"),
    ("/", "search id or title (Enter keep, Esc clear)"),
    ("r", "reload beads now"),
    ("U", "install the newer release the bottom bar offers"),
    ("?", "toggle this help"),
    ("q, Ctrl-C", "quit (running agents keep running)"),
    ("Tree", ""),
    ("↑↓ k j, PgUp/PgDn, g/G", "move"),
    ("→ l / ← h", "expand / collapse or go to parent"),
    ("e / E", "expand all / collapse all"),
    ("space", "toggle mark"),
    (
        "d",
        "dispatch marked or selected; a blocked bead queues its prerequisites first",
    ),
    (
        "A",
        "queue every ready leaf the filter matches: priority, then most beads unblocked",
    ),
    ("D", "decompose marked or selected epics"),
    (
        "X",
        "delete selected bead and its descendants (beads it blocked are unblocked)",
    ),
    (
        "Tab",
        "focus detail: ↑↓ entries, Enter jump, PgUp/PgDn scroll",
    ),
    ("Esc", "clear search, else clear marks"),
    ("Graph", ""),
    ("←→↑↓ h l k j", "move between cards"),
    (
        "Enter / Backspace",
        "drill into card / up to anchor's parent",
    ),
    ("s", "toggle scope Local / AllOpen"),
    ("space, d, D, X", "as in tree"),
    ("Agents", ""),
    ("↑↓", "select run; PgUp/PgDn scroll its log"),
    ("Enter", "jump to the bead in the tree"),
    ("K", "kill running run (cancel queued)"),
    ("R", "retry ended run in the same mode"),
    ("Q", "drain the queue: cancel every queued run"),
    ("o", "open the run's session in iTerm (or copy command)"),
    ("C", "hide ended runs, remove decompose scratch worktrees"),
];

fn draw_modal(f: &mut Frame, app: &App) {
    let Some(modal) = &app.modal else { return };
    let area = f.area();
    let (title, lines): (&str, Vec<Line>) = match modal {
        Modal::Help => (
            "Help",
            std::iter::once(Line::from(Span::styled("Status", theme::heading())))
                .chain(LEGEND.iter().map(|&(d, meaning)| {
                    Line::from(vec![
                        Span::styled(
                            format!("  {} {:<22}", d.glyph(), d.label()),
                            Style::new().fg(d.color()),
                        ),
                        Span::raw(meaning),
                    ])
                }))
                .chain(HELP.iter().map(|(k, v)| {
                    if v.is_empty() {
                        Line::from(Span::styled(*k, theme::heading()))
                    } else {
                        Line::from(vec![
                            Span::styled(format!("  {k:<24}"), Style::new().fg(theme::VIOLET)),
                            Span::raw(*v),
                        ])
                    }
                }))
                .collect(),
        ),
        Modal::Dispatch(plan) => {
            let mut lines: Vec<Line> = plan
                .iter()
                .map(|l| {
                    let gh = l.gh.map(|n| format!("#{n}")).unwrap_or_default();
                    match &l.outcome {
                        Ok(m) => {
                            let mut spans = vec![
                                Span::raw(format!("{:<14} ", l.bead_id)),
                                Span::styled(
                                    format!("{:<10}", m.label()),
                                    Style::new().fg(Color::Green),
                                ),
                                Span::raw(format!("{gh:<7}")),
                            ];
                            // Dispatch-all ranks its lines; the rank column says why.
                            if let Some(n) = l.unblocks {
                                let p = app.graph.get(&l.bead_id).map_or(String::new(), |i| {
                                    format!("P{} ", app.graph.beads[i].priority)
                                });
                                spans.push(Span::styled(
                                    format!("{:<16}", format!("{p}unblocks {n}")),
                                    Style::new().fg(theme::CYAN),
                                ));
                            }
                            spans.push(Span::raw(truncate(&l.title, 60)));
                            if !l.after.is_empty() {
                                spans.push(Span::styled(
                                    format!("  after {}", l.after.join(", ")),
                                    Style::new().fg(theme::CYAN),
                                ));
                            }
                            Line::from(spans)
                        }
                        Err(reason) => Line::from(vec![
                            Span::raw(format!("{:<14} ", l.bead_id)),
                            Span::styled(format!("{:<10}", "SKIP"), Style::new().fg(Color::Red)),
                            Span::raw(format!("{gh:<7}")),
                            Span::styled(reason.clone(), Style::new().fg(Color::DarkGray)),
                        ]),
                    }
                })
                .collect();
            // Chains can run long; the count leads so it stays visible if the
            // list is clipped by a short terminal.
            let queued = plan.iter().filter(|l| l.outcome.is_ok()).count();
            let waiting = plan
                .iter()
                .filter(|l| l.outcome.is_ok() && !l.after.is_empty())
                .count();
            let summary = if plan.iter().any(|l| l.unblocks.is_some()) {
                format!(
                    "Queue {queued} ready run(s) matching the filter: highest priority first, then most beads unblocked"
                )
            } else if waiting > 0 {
                format!("Queue {queued} run(s); {waiting} wait for prerequisites to close")
            } else {
                format!("Queue {queued} run(s)")
            };
            lines.insert(0, Line::from(Span::styled(summary, theme::heading())));
            lines.insert(1, Line::raw(""));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "y/Enter queue · n/Esc cancel",
                Style::new().fg(Color::DarkGray),
            )));
            ("Dispatch", lines)
        }
        Modal::Kill(id) => (
            "Kill",
            vec![
                Line::raw(format!("Kill run {id}? (SIGTERM to its process group)")),
                Line::raw(""),
                Line::from(Span::styled(
                    "y/Enter kill · n/Esc cancel",
                    Style::new().fg(Color::DarkGray),
                )),
            ],
        ),
        Modal::Delete(ids) => {
            let g = &app.graph;
            let doomed: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
            let mut unblocked: Vec<&str> = ids
                .iter()
                .filter_map(|id| g.get(id))
                .flat_map(|i| g.dependents[i].iter().copied())
                .map(|d| g.beads[d].id.as_str())
                .filter(|d| !doomed.contains(d))
                .collect();
            unblocked.sort_unstable();
            unblocked.dedup();
            let title = ids
                .first()
                .and_then(|id| g.get(id))
                .map_or("", |i| g.beads[i].title.as_str());
            let mut lines = vec![Line::raw(format!(
                "Delete {} {}?",
                ids.first().map_or("", String::as_str),
                truncate(title, 60)
            ))];
            if ids.len() > 1 {
                lines.push(Line::raw(format!(
                    "Its {} descendant(s) are deleted too.",
                    ids.len() - 1
                )));
            }
            if !unblocked.is_empty() {
                lines.push(Line::raw(format!(
                    "No longer blocked by it: {}",
                    unblocked.join(", ")
                )));
            }
            lines.push(Line::from(Span::styled(
                "This cannot be undone.",
                Style::new().fg(Color::Red),
            )));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "y/Enter delete · n/Esc cancel",
                Style::new().fg(Color::DarkGray),
            )));
            ("Delete", lines)
        }
        Modal::DrainQueue(n) => (
            "Drain queue",
            vec![
                Line::raw(format!(
                    "Cancel all {n} queued run(s)? Running agents keep running."
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "y/Enter drain · n/Esc keep",
                    Style::new().fg(Color::DarkGray),
                )),
            ],
        ),
        Modal::Update { version, command } => (
            "Update",
            vec![
                Line::raw(format!("Update abacus {} → {version}?", Version::current())),
                Line::raw(""),
                Line::raw("Builds from source in the background (a few minutes):"),
                Line::from(Span::styled(
                    format!("  {command}"),
                    Style::new().fg(theme::VIOLET),
                )),
                Line::raw(format!(
                    "Output goes to {}. Running agents keep running.",
                    app.cfg.state_dir.join("update.log").display()
                )),
                Line::raw("Relaunch abacus afterwards to use the new version."),
                Line::raw(""),
                Line::from(Span::styled(
                    "y/Enter update · n/Esc cancel",
                    Style::new().fg(Color::DarkGray),
                )),
            ],
        ),
        Modal::Quit(n) => (
            "Quit",
            vec![
                Line::raw(format!(
                    "{n} queued runs will be dropped; running agents keep running. Quit?"
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "y/Enter quit · n/Esc stay",
                    Style::new().fg(Color::DarkGray),
                )),
            ],
        ),
    };
    let width = lines
        .iter()
        .map(|l| l.width() as u16)
        .max()
        .unwrap_or(20)
        .saturating_add(4)
        .max(40);
    let rect = centered(area, width, lines.len() as u16 + 2);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(format!(" {title} "))
                .border_style(theme::border(true)),
        ),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::{AppEvent, Config};
    use crate::runs::Registry;

    fn app(state: &Path) -> App {
        let repo = Path::new("/repo/a");
        let cfg = Config {
            repo_root: repo.to_path_buf(),
            state_dir: state.to_path_buf(),
            max_concurrent: 1,
            model: None,
            agent_cmd: "omp".into(),
            repo_name: "a".into(),
            origin: None,
        };
        let (tx, _) = std::sync::mpsc::channel();
        App::new(cfg, Registry::load(state, repo).unwrap(), tx)
    }

    fn screen(app: &mut App, width: u16) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(width, 16)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    fn press(app: &mut App, c: char) {
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        )));
    }

    #[test]
    fn update_offer_survives_a_narrow_bottom_bar_and_u_prompts_for_the_install() {
        let state = tempfile::tempdir().unwrap();
        let mut app = app(state.path());
        let newer = Version::parse("999.0.0").unwrap();
        assert!(screen(&mut app, 80)[0].starts_with(&format!("abacus {}", Version::current())));

        app.handle(AppEvent::UpdateAvailable(newer));
        // Too narrow for the key hints; the offer leads, so it is what stays.
        let bottom = screen(&mut app, 30).pop().unwrap();
        assert!(bottom.starts_with(" U update to v999.0.0 "), "{bottom}");

        press(&mut app, 'U');
        let prompt = screen(&mut app, 120).join("\n");
        assert!(
            prompt.contains("--git https://github.com/mlg87/abacus --tag v999.0.0"),
            "{prompt}"
        );
        press(&mut app, 'n');
        assert!(app.modal.is_none());

        // A failed build leaves the offer up for another try.
        app.handle(AppEvent::UpdateInstalled {
            version: newer,
            result: Err("cargo install failed".into()),
        });
        assert_eq!(app.update, Some(UpdateStatus::Available(newer)));
    }

    #[test]
    fn help_leads_with_every_status_glyph_even_in_a_short_terminal() {
        let state = tempfile::tempdir().unwrap();
        let mut app = app(state.path());
        press(&mut app, '?');
        // 16 rows clips the help well before its key bindings end.
        let help = screen(&mut app, 120).join("\n");
        for &(d, meaning) in LEGEND {
            let row = format!("{} {:<22}{meaning}", d.glyph(), d.label());
            assert!(help.contains(&row), "missing {row:?} in\n{help}");
        }
    }

    #[test]
    fn agents_started_is_a_full_wall_clock_stamp_not_a_running_counter() {
        use crate::runs::{Mode, Progress, Run, RunMeta, RunState};
        let state = tempfile::tempdir().unwrap();
        let mut app = app(state.path());
        app.registry.runs.push(Run {
            meta: RunMeta {
                run_id: "1-x-1".into(),
                repo_root: "/repo/a".into(),
                bead_id: "x-1".into(),
                bead_title: "t".into(),
                mode: Mode::Resolve,
                state: RunState::Succeeded,
                worktree: None,
                branch: None,
                pid: None,
                queued_at: 1_792_705_713,
                started_at: Some(1_792_705_713),
                ended_at: Some(1_792_705_748),
                exit_code: Some(0),
                error: None,
                retry_of: None,
                hidden: false,
                claim_settled: true,
                after: Vec::new(),
            },
            progress: Progress::default(),
        });
        press(&mut app, '3');
        let rows = screen(&mut app, 120);
        let row = rows.iter().find(|l| l.contains("x-1")).unwrap();
        assert!(!row.contains(" ago"), "{row}");
        // Timezone-independent: the stamp must render whole, not clipped by the column.
        let cell = &row[row.find("resolve").unwrap() + 11..][..19];
        let shape: String = cell
            .chars()
            .map(|c| if c.is_ascii_digit() { 'd' } else { c })
            .collect();
        assert_eq!(shape, "dd:dd:dd dddd-dd-dd", "{row}");
    }

    /// The dispatch-all dialog names each line's rank, so its order explains itself.
    #[test]
    fn dispatch_all_dialog_shows_priority_and_unblock_count_per_line() {
        use crate::model::tests::{bead, dep};
        let state = tempfile::tempdir().unwrap();
        let mut app = app(state.path());
        let mut waits = bead("waits", "open");
        waits.dependencies.push(dep("waits", "gate", "blocks"));
        app.handle(AppEvent::BeadsLoaded(Ok(vec![bead("gate", "open"), waits])));
        press(&mut app, 'A');
        let dialog = screen(&mut app, 140);
        assert!(
            dialog.iter().any(|l| l.contains("Queue 1 ready run(s)")),
            "{dialog:#?}"
        );
        let row = dialog.iter().find(|l| l.contains("implement")).unwrap();
        assert!(
            row.contains("gate") && row.contains("P2 unblocks 1"),
            "{row}"
        );
    }
}
