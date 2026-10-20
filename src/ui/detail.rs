use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};

use super::{fmt_duration, state_style, truncate};
use crate::app::{App, Focus};
use crate::dispatch::issue_url;
use crate::model::gh_number;
use crate::runs::now_secs;

/// `full` adds the latest run summary and the long text fields (tree view);
/// the graph view's compact pane shows only header and relationships.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect, full: bool) {
    let focused = app.focus == Focus::Detail;
    let border = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let block = Block::bordered().title(" Detail ").border_style(border);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(i) = app.sel_idx() else {
        f.render_widget(
            Paragraph::new(Span::styled(
                "no bead selected",
                Style::new().fg(Color::DarkGray),
            )),
            inner,
        );
        return;
    };
    let g = &app.graph;
    let b = &g.beads[i];
    let d = g.derived(i);
    let dim = Style::new().fg(Color::DarkGray);

    let mut header = vec![
        Line::from(vec![
            Span::styled(format!("{} ", d.glyph()), Style::new().fg(d.color())),
            Span::styled(b.title.clone(), Style::new().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled(b.id.clone(), Style::new().fg(Color::Cyan)),
            Span::styled(
                format!(
                    " · {} · P{} · {} · {} · {}",
                    if b.issue_type.is_empty() {
                        "?"
                    } else {
                        &b.issue_type
                    },
                    b.priority,
                    d.label(),
                    b.assignee
                        .as_deref()
                        .filter(|a| !a.is_empty())
                        .unwrap_or("unassigned"),
                    if b.labels.is_empty() {
                        "no labels".into()
                    } else {
                        b.labels.join(",")
                    },
                ),
                dim,
            ),
        ]),
    ];
    if let Some(url) = gh_number(b).and_then(|n| issue_url(app.cfg.origin.as_ref(), n)) {
        header.push(Line::from(Span::styled(url, Style::new().fg(Color::Blue))));
    }
    if let Some(at) = &b.closed_at {
        let reason = b.close_reason.as_deref().unwrap_or("");
        let date = at.get(..10).unwrap_or(at);
        header.push(Line::from(Span::styled(
            format!("closed {date}: {reason}"),
            dim,
        )));
    }

    // Relationship list: section headers interleaved with selectable entries.
    let entries = app.detail_entries();
    let mut items: Vec<ListItem<'static>> = Vec::new();
    let mut entry_item = Vec::with_capacity(entries.len());
    let mut last_section: Option<&str> = None;
    for (section, j) in &entries {
        if last_section != Some(section.as_str()) {
            items.push(ListItem::new(Line::from(Span::styled(
                section.clone(),
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ))));
            last_section = Some(section.as_str());
        }
        let jd = g.derived(*j);
        entry_item.push(items.len());
        items.push(ListItem::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(jd.glyph(), Style::new().fg(jd.color())),
            Span::raw(" "),
            Span::styled(g.beads[*j].id.clone(), Style::new().fg(Color::Cyan)),
            Span::raw(format!(" {}", g.beads[*j].title)),
        ])));
    }
    if items.is_empty() {
        items.push(ListItem::new(Span::styled("no relationships", dim)));
    }

    let mut summary: Vec<Line<'static>> = Vec::new();
    if full {
        if let Some(run) = app.latest_runs().get(b.id.as_str()) {
            let m = &run.meta;
            let elapsed = m
                .started_at
                .map(|s| fmt_duration(m.ended_at.unwrap_or_else(now_secs).saturating_sub(s)))
                .unwrap_or_else(|| "-".into());
            summary.push(Line::from(vec![
                Span::styled(
                    "Latest run ",
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{} · ", m.mode.label())),
                Span::styled(m.state.label(), state_style(m.state)),
                Span::raw(format!(" · {elapsed} · ${:.2}", run.progress.cost_usd)),
            ]));
            if let Some(a) = &run.progress.activity {
                summary.push(Line::from(Span::styled(
                    format!("  {}", truncate(a, 200)),
                    dim,
                )));
            }
            if let Some(r) = &run.progress.result_line {
                summary.push(Line::raw(format!("  {r}")));
            }
            if let Some(e) = &m.error {
                summary.push(Line::from(Span::styled(
                    format!("  {e}"),
                    Style::new().fg(Color::Red),
                )));
            }
            if let Some(pr) = &run.progress.pr_url {
                summary.push(Line::from(Span::styled(
                    format!("  {pr}"),
                    Style::new().fg(Color::Blue),
                )));
            }
            if let Some(wt) = &m.worktree {
                summary.push(Line::from(Span::styled(format!("  {}", wt.display()), dim)));
            }
        }
        for (heading, body) in [
            ("Description", &b.description),
            ("Acceptance criteria", &b.acceptance_criteria),
            ("Notes", &b.notes),
        ] {
            if body.trim().is_empty() {
                continue;
            }
            summary.push(Line::raw(""));
            summary.push(Line::from(Span::styled(
                heading,
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));
            summary.extend(body.lines().map(|l| Line::raw(l.to_string())));
        }
    }

    let header_h = header.len() as u16 + 1;
    let rel_h = if full {
        (items.len() as u16)
            .min(inner.height.saturating_sub(header_h) * 2 / 5)
            .max(1)
    } else {
        inner.height.saturating_sub(header_h)
    };
    let [h_area, rel_area, text_area] = Layout::vertical([
        Constraint::Length(header_h),
        Constraint::Length(rel_h),
        Constraint::Min(0),
    ])
    .areas(inner);
    f.render_widget(Paragraph::new(header).wrap(Wrap { trim: false }), h_area);

    let mut state = ListState::default();
    if !entry_item.is_empty() {
        let sel = app.detail_sel.min(entry_item.len() - 1);
        state.select(Some(entry_item[sel]));
    }
    let list = List::new(items).highlight_style(if focused {
        Style::new()
            .bg(Color::Rgb(50, 50, 70))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    });
    f.render_stateful_widget(list, rel_area, &mut state);

    if full && !summary.is_empty() {
        f.render_widget(
            Paragraph::new(summary)
                .wrap(Wrap { trim: false })
                .scroll((app.detail_scroll, 0)),
            text_area,
        );
    }
}
