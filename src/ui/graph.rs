use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::{detail, theme, truncate};
use crate::app::App;
use crate::model::Status;
use crate::view_model::GraphScope;

const CARD_H: u16 = 3;
const MIN_COL_W: u16 = 26;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let [cols_area, detail_area] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(12)]).areas(area);

    let scope = match app.scope {
        GraphScope::Local => "Local",
        GraphScope::AllOpen => "AllOpen",
    };
    let anchor = app.anchor.clone().unwrap_or_else(|| "-".into());
    let outer = Block::bordered()
        .title(format!(" Graph · anchor {anchor} · scope {scope} "))
        .border_style(theme::border(true));
    let inner = outer.inner(cols_area);
    f.render_widget(outer, cols_area);

    let cols: Vec<Vec<usize>> = app.graph_columns().iter().map(|c| c.to_vec()).collect();
    if cols.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "no beads in scope — s toggles AllOpen, Backspace moves the anchor up",
                Style::new().fg(Color::DarkGray),
            )),
            inner,
        );
        detail::draw(f, app, detail_area, false);
        return;
    }
    let has_cycle = !app.layers.cycle.is_empty();
    let ncols = cols.len() as u16;
    let col_w = MIN_COL_W.max(inner.width / ncols.max(1));
    let visible = (inner.width / col_w).max(1) as usize;
    let pos = app.graph_pos();
    if let Some((c, _)) = pos {
        if c < app.graph_hscroll {
            app.graph_hscroll = c;
        } else if c >= app.graph_hscroll + visible {
            app.graph_hscroll = c + 1 - visible;
        }
    }
    app.graph_hscroll = app.graph_hscroll.min(cols.len().saturating_sub(1));

    let g = &app.graph;
    let sel = app.sel_idx();
    let (blockers, dependents) = match sel {
        Some(s) => (g.blockers[s].clone(), g.dependents[s].clone()),
        None => (Vec::new(), Vec::new()),
    };

    for (slot, ci) in (app.graph_hscroll..cols.len()).take(visible).enumerate() {
        let col = &cols[ci];
        let rect = Rect {
            x: inner.x + slot as u16 * col_w,
            y: inner.y,
            width: col_w.min(inner.right().saturating_sub(inner.x + slot as u16 * col_w)),
            height: inner.height,
        };
        let is_cycle = has_cycle && ci == cols.len() - 1;
        let (title, tstyle) = if is_cycle {
            (
                format!(" cycle! ({}) ", col.len()),
                Style::new().fg(Color::Red),
            )
        } else {
            (
                format!(" L{ci} ({}) ", col.len()),
                Style::new().fg(Color::DarkGray),
            )
        };
        let block = Block::bordered().title(title).border_style(tstyle);
        let body = block.inner(rect);
        f.render_widget(block, rect);

        let capacity = (body.height / CARD_H).max(1) as usize;
        let offset = match pos {
            Some((c, r)) if c == ci => r.saturating_sub(capacity - 1),
            _ => 0,
        };
        let text_w = body.width.saturating_sub(2) as usize;
        let mut lines: Vec<Line> = Vec::new();
        for &i in col.iter().skip(offset).take(capacity) {
            let b = &g.beads[i];
            let d = g.derived(i);
            let (prefix, style) = if Some(i) == sel {
                ("", theme::selected())
            } else if blockers.contains(&i) && app.members.contains(&i) {
                ("▶", Style::new().fg(theme::CORAL))
            } else if dependents.contains(&i) && app.members.contains(&i) {
                ("◀", Style::new().fg(theme::CYAN))
            } else {
                ("", Style::new())
            };
            let outside = g.blockers[i]
                .iter()
                .filter(|&&j| !app.members.contains(&j) && g.beads[j].status != Status::Closed)
                .count();
            let mark = if app.marks.contains(&b.id) { "▪" } else { "" };
            let mut l1 = vec![
                Span::styled(prefix, style),
                Span::styled(mark, Style::new().fg(theme::MAGENTA)),
                Span::styled(d.glyph(), style.fg(d.color())),
                Span::styled(format!(" P{} {}", b.priority, b.id), style),
            ];
            if outside > 0 {
                l1.push(Span::styled(
                    format!(" ↗{outside}"),
                    style.fg(theme::VIOLET),
                ));
            }
            lines.push(Line::from(l1));
            lines.push(Line::from(Span::styled(
                format!("  {}", truncate(&b.title, text_w)),
                if style == Style::new() {
                    Style::new().fg(Color::Gray)
                } else {
                    style
                },
            )));
            lines.push(Line::raw(""));
        }
        if col.len() > offset + capacity {
            lines.pop();
            lines.push(Line::from(Span::styled(
                format!("  … {} more", col.len() - offset - capacity),
                Style::new().fg(Color::DarkGray),
            )));
        }
        f.render_widget(Paragraph::new(lines), body);
    }
    detail::draw(f, app, detail_area, false);
}
