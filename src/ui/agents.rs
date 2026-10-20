use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};

use super::{fmt_duration, spinner, state_style, theme, truncate};
use crate::app::App;
use crate::runs::{RunState, now_secs};

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let [table_area, log_area] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);
    let order = app.agent_rows();
    if !order.is_empty() {
        app.agents_sel = app.agents_sel.min(order.len() - 1);
    }
    let now = now_secs();
    let dim = Style::new().fg(Color::DarkGray);

    let rows: Vec<Row> = order
        .iter()
        .map(|&i| {
            let r = &app.registry.runs[i];
            let m = &r.meta;
            let state = match m.state {
                RunState::Running | RunState::Starting => {
                    format!("{} {}", spinner(app.frame), m.state.label())
                }
                s => s.label().to_string(),
            };
            let started = m
                .started_at
                .map(|s| format!("{} ago", fmt_duration(now.saturating_sub(s))))
                .unwrap_or_else(|| "-".into());
            let elapsed = m
                .started_at
                .map(|s| fmt_duration(m.ended_at.unwrap_or(now).saturating_sub(s)))
                .unwrap_or_else(|| "-".into());
            let (activity, astyle) = match (&r.progress.result_line, &m.error, &r.progress.activity)
            {
                (Some(res), _, _) if !m.state.is_active() => (res.clone(), Style::new()),
                (_, Some(e), _) => (e.clone(), Style::new().fg(Color::Red)),
                (_, _, Some(a)) => (a.clone(), dim),
                _ => (String::new(), dim),
            };
            Row::new(vec![
                Cell::from(Span::styled(state, state_style(m.state))),
                Cell::from(Span::styled(
                    m.bead_id.clone(),
                    Style::new().fg(theme::CYAN),
                )),
                Cell::from(m.mode.label()),
                Cell::from(started),
                Cell::from(elapsed),
                Cell::from(format!("${:.2}", r.progress.cost_usd)),
                Cell::from(Span::styled(activity, astyle)),
            ])
        })
        .collect();
    let empty = rows.is_empty();
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(16),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new([
            "State",
            "Bead",
            "Mode",
            "Started",
            "Elapsed",
            "Cost",
            "Activity / RESULT",
        ])
        .style(theme::heading()),
    )
    .block(
        Block::bordered()
            .title(" Agents ")
            .border_style(theme::border(true)),
    )
    .row_highlight_style(theme::selected());
    let mut state = TableState::default();
    if !empty {
        state.select(Some(app.agents_sel));
    }
    f.render_stateful_widget(table, table_area, &mut state);

    let Some(run) = app.selected_run() else {
        f.render_widget(
            Paragraph::new(Span::styled(
                "no runs yet — mark beads with space and press d in the tree",
                dim,
            ))
            .block(Block::bordered().title(" Log ").border_style(dim)),
            log_area,
        );
        return;
    };
    let m = &run.meta;
    let mut title = format!(" Log · {} · {} ", m.run_id, m.bead_title);
    if let Some(wt) = &m.worktree {
        title.push_str(&format!("· {} ", wt.display()));
    }
    let block = Block::bordered()
        .title(truncate(&title, log_area.width.saturating_sub(4) as usize))
        .border_style(dim);
    let inner = block.inner(log_area);
    let h = inner.height as usize;
    let log = &run.progress.log;
    let top = match app.log_scroll {
        Some(t) => t.min(log.len().saturating_sub(1)),
        None => log.len().saturating_sub(h),
    };
    let w = inner.width as usize;
    let lines: Vec<Line> = log
        .iter()
        .skip(top)
        .take(h)
        .map(|l| {
            let style = if l.starts_with("stderr: ") {
                Style::new().fg(Color::Red)
            } else if l.starts_with('→') {
                dim
            } else if l.starts_with('■') {
                Style::new().fg(Color::Green)
            } else {
                Style::new()
            };
            Line::from(Span::styled(truncate(l, w), style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), log_area);
}
