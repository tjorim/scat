//! Full-screen dependency-graph view (`ViewMode::DepGraph`, opened with
//! `d`): a `Canvas`-rendered node-link diagram of the selected script's
//! immediate neighborhood — the same "uses"/"used by" edges the Deps pane
//! already lists as text (`app.deps`), drawn spatially instead.
//!
//! Deliberately scoped to one script's neighborhood rather than the whole
//! catalog: a full-catalog graph of a few thousand scripts would be an
//! unreadable hairball in a terminal, and this reuses data the detail
//! worker already loads for the selected script — no new query or worker.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Context, Line as CanvasLine};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::super::{DependencyItem, TuiApp};
use super::common::{hint_key, left_truncate_path};

/// Cap on nodes drawn per side. Beyond this the labels would overlap; the
/// Deps pane (still reachable via Tab) is the place to see the full list.
const MAX_NODES_PER_SIDE: usize = 12;
const X_BOUNDS: [f64; 2] = [-100.0, 100.0];
const Y_BOUNDS: [f64; 2] = [-50.0, 50.0];
/// Horizontal distance from the center node to each side's nodes.
const NODE_X: f64 = 65.0;
const LABEL_MAX_CHARS: usize = 22;

pub(super) fn draw_dep_graph_view(frame: &mut Frame<'_>, app: &mut TuiApp) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let center_label = app
        .selected_logical_path()
        .unwrap_or_else(|| "(no script selected)".to_string());
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("Dependency Graph — {center_label}"),
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        root[0],
    );

    let uses: Vec<&DependencyItem> = app.deps.iter().filter(|d| d.kind == "uses").collect();
    let used_by: Vec<&DependencyItem> = app.deps.iter().filter(|d| d.kind == "used by").collect();

    if app.detail_loading {
        frame.render_widget(
            Paragraph::new("Loading…").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Dependency Graph"),
            ),
            root[1],
        );
    } else if uses.is_empty() && used_by.is_empty() {
        frame.render_widget(
            Paragraph::new("No dependencies.").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Dependency Graph"),
            ),
            root[1],
        );
    } else {
        let selected_path = app
            .deps
            .get(app.deps_selected)
            .map(|d| d.logical_path.clone());
        let center_node_label = left_truncate_path(&center_label, LABEL_MAX_CHARS);
        let canvas = Canvas::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Dependency Graph"),
            )
            .x_bounds(X_BOUNDS)
            .y_bounds(Y_BOUNDS)
            .paint(move |ctx| {
                ctx.print(
                    0.0,
                    3.0,
                    Span::styled(
                        center_node_label.clone(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                );
                ctx.print(
                    0.0,
                    0.0,
                    Span::styled("●", Style::default().fg(Color::Yellow)),
                );
                draw_side(ctx, &uses, NODE_X, Color::Cyan, selected_path.as_deref());
                draw_side(
                    ctx,
                    &used_by,
                    -NODE_X,
                    Color::Magenta,
                    selected_path.as_deref(),
                );
            });
        frame.render_widget(canvas, root[1]);
    }

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            hint_key("j/k"),
            Span::raw(" select  "),
            hint_key("Enter"),
            Span::raw(" open  "),
            hint_key("["),
            Span::raw(" back  "),
            hint_key("Esc/Backspace"),
            Span::raw(" close  "),
            hint_key("q"),
            Span::raw(" quit"),
        ])),
        root[2],
    );
}

/// Draw up to [`MAX_NODES_PER_SIDE`] nodes at horizontal offset `x` from the
/// center, evenly spaced vertically, each connected to the center by a line.
/// The node matching `selected_path` (the Deps pane's current selection,
/// shared with this view) is highlighted in yellow.
fn draw_side(
    ctx: &mut Context<'_>,
    items: &[&DependencyItem],
    x: f64,
    color: Color,
    selected_path: Option<&str>,
) {
    let shown = items.len().min(MAX_NODES_PER_SIDE);
    for (i, item) in items.iter().take(shown).enumerate() {
        let y = node_y(i, shown);
        let highlighted = selected_path == Some(item.logical_path.as_str());
        let node_color = if highlighted { Color::Yellow } else { color };

        ctx.draw(&CanvasLine {
            x1: 0.0,
            y1: 0.0,
            x2: x,
            y2: y,
            color: node_color,
        });
        ctx.print(x, y, Span::styled("●", Style::default().fg(node_color)));

        let label = left_truncate_path(&item.logical_path, LABEL_MAX_CHARS);
        let label_x = if x > 0.0 {
            x + 2.0
        } else {
            x - 2.0 - label.chars().count() as f64
        };
        let mut style = Style::default().fg(node_color);
        if highlighted {
            style = style.add_modifier(Modifier::BOLD);
        }
        ctx.print(label_x, y, Span::styled(label, style));
    }
}

/// Vertical position for node `index` of `count`, evenly spread across the
/// canvas height (with a margin so labels don't collide with the border).
fn node_y(index: usize, count: usize) -> f64 {
    if count <= 1 {
        return 0.0;
    }
    let margin = 8.0;
    let span = (Y_BOUNDS[1] - Y_BOUNDS[0]) - 2.0 * margin;
    let step = span / (count - 1) as f64;
    (Y_BOUNDS[1] - margin) - index as f64 * step
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_y_centers_a_single_node() {
        assert!(node_y(0, 1).abs() < f64::EPSILON);
    }

    #[test]
    fn node_y_spreads_nodes_from_top_to_bottom_within_bounds() {
        let first = node_y(0, 3);
        let last = node_y(2, 3);
        assert!(first > 0.0, "first node should be above center");
        assert!(last < 0.0, "last node should be below center");
        assert!(first <= Y_BOUNDS[1]);
        assert!(last >= Y_BOUNDS[0]);
    }

    #[test]
    fn node_y_is_monotonically_decreasing() {
        let ys: Vec<f64> = (0..5).map(|i| node_y(i, 5)).collect();
        for pair in ys.windows(2) {
            assert!(pair[0] > pair[1]);
        }
    }
}
