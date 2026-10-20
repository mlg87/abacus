use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem};

use super::{detail, run_badge, truncate};
use crate::app::{App, Focus};
use crate::model::{Derived, gh_number};

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);

    let row_w = left.width.saturating_sub(2) as usize;
    let items: Vec<ListItem<'static>> = {
        let latest = app.latest_runs();
        let g = &app.graph;
        app.rows
            .iter()
            .map(|r| {
                let b = &g.beads[r.idx];
                let d = g.derived(r.idx);
                let dim = r.context_only;
                let base = if dim {
                    Style::new().fg(Color::DarkGray)
                } else {
                    Style::new()
                };
                let arrow = match (r.has_visible_children, r.expanded) {
                    (true, true) => "▾",
                    (true, false) => "▸",
                    _ => " ",
                };
                let mut spans = vec![Span::raw(format!(
                    "{}{arrow} ",
                    "  ".repeat(r.depth as usize)
                ))];
                if app.marks.contains(&b.id) {
                    spans.push(Span::styled("▪", Style::new().fg(Color::Magenta)));
                }
                let glyph_style = if dim {
                    base
                } else {
                    Style::new().fg(d.color())
                };
                spans.push(Span::styled(d.glyph(), glyph_style));
                spans.push(Span::styled(
                    format!(" P{} ", b.priority),
                    base.fg(Color::DarkGray),
                ));
                spans.push(Span::styled(
                    b.id.clone(),
                    if dim {
                        base
                    } else {
                        Style::new().fg(Color::Cyan)
                    },
                ));
                let mut suffix: Vec<Span<'static>> = Vec::new();
                let kids = &g.children[r.idx];
                if !kids.is_empty() {
                    let closed = kids
                        .iter()
                        .filter(|&&c| g.derived(c) == Derived::Closed)
                        .count();
                    suffix.push(Span::styled(
                        format!(" [{closed}/{}]", kids.len()),
                        Style::new().fg(Color::DarkGray),
                    ));
                }
                if let Some(n) = gh_number(b) {
                    suffix.push(Span::styled(format!(" #{n}"), Style::new().fg(Color::Blue)));
                }
                if let Some(run) = latest.get(b.id.as_str()) {
                    suffix.push(Span::raw(" "));
                    suffix.push(run_badge(run, app.frame));
                }
                // Titles yield width first so counts, issue numbers, and run
                // badges stay visible on long rows.
                let used: usize = spans.iter().chain(&suffix).map(Span::width).sum();
                let avail = row_w.saturating_sub(used + 1);
                spans.push(Span::styled(
                    format!(" {}", truncate(&b.title, avail)),
                    base,
                ));
                spans.extend(suffix);
                ListItem::new(Line::from(spans))
            })
            .collect()
    };

    let title = if app.marks.is_empty() {
        " Beads ".to_string()
    } else {
        format!(" Beads · {} marked ", app.marks.len())
    };
    let border = if app.focus == Focus::Main {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let list = List::new(items)
        .block(Block::bordered().title(title).border_style(border))
        .highlight_style(
            Style::new()
                .bg(Color::Rgb(50, 50, 70))
                .add_modifier(Modifier::BOLD),
        );
    app.tree_state.select(app.sel_row());
    f.render_stateful_widget(list, left, &mut app.tree_state);
    detail::draw(f, app, right, true);
}
