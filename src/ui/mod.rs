mod agents;
mod detail;
mod graph;
mod tree;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::app::{App, Modal, View};
use crate::runs::{Run, RunState};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn spinner(frame: u64) -> &'static str {
    SPINNER[(frame % SPINNER.len() as u64) as usize]
}

pub fn run_badge(run: &Run, frame: u64) -> Span<'static> {
    match run.meta.state {
        RunState::Queued => Span::styled("⏳", Style::new().fg(Color::Cyan)),
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
        RunState::Queued => Style::new().fg(Color::Cyan),
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
            Style::new().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::new()
        };
        Span::styled(format!("[{n} {name}]"), style)
    };
    let mut spans = vec![
        Span::styled("abacus", Style::new().add_modifier(Modifier::BOLD)),
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
    ];
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
            Span::styled("/", Style::new().fg(Color::Cyan)),
            Span::raw(app.filter.query.clone()),
            Span::styled("█", Style::new().fg(Color::Cyan)),
            Span::styled("  Enter keep · Esc clear", Style::new().fg(Color::DarkGray)),
        ]);
    }
    if let Some((m, _)) = &app.message {
        return Line::from(Span::styled(m.clone(), Style::new().fg(Color::Yellow)));
    }
    let hints = match (app.view, app.focus) {
        (View::Tree, crate::app::Focus::Detail) => {
            "↑↓ relationship · Enter jump · PgUp/PgDn scroll · Esc/Tab back"
        }
        (View::Tree, _) => {
            "↑↓ move · →← expand/collapse · e/E all · space mark · d dispatch · D decompose · Tab detail · / search · f filter · L label · ? help · q quit"
        }
        (View::Graph, _) => {
            "←→↑↓ move · Enter drill in · Backspace up · s scope · space mark · d dispatch · D decompose · ? help · q quit"
        }
        (View::Agents, _) => {
            "↑↓ run · Enter bead · K kill · R retry · X release · o session · C clear ended · PgUp/PgDn log · ? help · q quit"
        }
    };
    Line::from(Span::styled(hints, Style::new().fg(Color::DarkGray)))
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

const HELP: &[(&str, &str)] = &[
    ("Global", ""),
    ("1 / 2 / 3", "tree / graph / agents view"),
    ("f", "cycle status filter (Open, Ready, InProgress, All)"),
    ("L", "cycle label filter"),
    ("/", "search id or title (Enter keep, Esc clear)"),
    ("r", "reload beads now"),
    ("?", "toggle this help"),
    ("q, Ctrl-C", "quit (running agents keep running)"),
    ("Tree", ""),
    ("↑↓ k j, PgUp/PgDn, g/G", "move"),
    ("→ l / ← h", "expand / collapse or go to parent"),
    ("e / E", "expand all / collapse all"),
    ("space", "toggle mark"),
    (
        "d",
        "dispatch marked or selected (resolve epics, implement tasks)",
    ),
    ("D", "decompose marked or selected epics"),
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
    ("space, d, D", "as in tree"),
    ("Agents", ""),
    ("↑↓", "select run; PgUp/PgDn scroll its log"),
    ("Enter", "jump to the bead in the tree"),
    ("K", "kill running run (cancel queued)"),
    ("R", "retry ended run in the same mode"),
    ("X", "release ended run's bead back to open"),
    ("o", "open the run's session in iTerm (or copy command)"),
    ("C", "hide ended runs, remove decompose scratch worktrees"),
];

fn draw_modal(f: &mut Frame, app: &App) {
    let Some(modal) = &app.modal else { return };
    let area = f.area();
    let (title, lines): (&str, Vec<Line>) = match modal {
        Modal::Help => (
            "Help",
            HELP.iter()
                .map(|(k, v)| {
                    if v.is_empty() {
                        Line::from(Span::styled(
                            *k,
                            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                        ))
                    } else {
                        Line::from(vec![
                            Span::styled(format!("  {k:<24}"), Style::new().fg(Color::Yellow)),
                            Span::raw(*v),
                        ])
                    }
                })
                .collect(),
        ),
        Modal::Dispatch(plan) => {
            let mut lines: Vec<Line> = plan
                .iter()
                .map(|l| {
                    let gh = l.gh.map(|n| format!("#{n}")).unwrap_or_default();
                    match &l.outcome {
                        Ok(m) => Line::from(vec![
                            Span::raw(format!("{:<14} ", l.bead_id)),
                            Span::styled(
                                format!("{:<10}", m.label()),
                                Style::new().fg(Color::Green),
                            ),
                            Span::raw(format!("{gh:<7}")),
                            Span::raw(truncate(&l.title, 60)),
                        ]),
                        Err(reason) => Line::from(vec![
                            Span::raw(format!("{:<14} ", l.bead_id)),
                            Span::styled(format!("{:<10}", "SKIP"), Style::new().fg(Color::Red)),
                            Span::raw(format!("{gh:<7}")),
                            Span::styled(reason.clone(), Style::new().fg(Color::DarkGray)),
                        ]),
                    }
                })
                .collect();
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
        Modal::Release(id) => {
            let bead = app
                .registry
                .find(id)
                .map(|r| r.meta.bead_id.clone())
                .unwrap_or_default();
            (
                "Release",
                vec![
                    Line::raw(format!(
                        "Release {bead} back to open (run {id})? The worktree stays."
                    )),
                    Line::raw(""),
                    Line::from(Span::styled(
                        "y/Enter release · n/Esc cancel",
                        Style::new().fg(Color::DarkGray),
                    )),
                ],
            )
        }
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
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        rect,
    );
}
