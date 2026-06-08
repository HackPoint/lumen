use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use lumen_core::rates;

use crate::data::AppState;

// ── Public entry ──────────────────────────────────────────────────────────────

pub fn render(f: &mut Frame, state: &AppState, no_anim: bool) {
    let area = f.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Min(8),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    render_header(f, outer[0], state, no_anim);
    render_body(f, outer[1], state);
    render_footer(f, outer[2], state);
}

// ── Header ────────────────────────────────────────────────────────────────────

/// Returns the current ring animation frame string (7 cols wide).
fn ring_frame(tick: u64, no_anim: bool) -> &'static str {
    if no_anim {
        return "·○◌◉◌○·";
    }
    // 5-frame pulse: expand → full → contract → core → expand. 2 ticks per frame ≈ 4 s cycle.
    const FRAMES: &[&str] = &["·○◌◉◌○·", "·○◌◉◌○·", " ·○◉○· ", "  ·◉·  ", " ·○◉○· "];
    FRAMES[(tick as usize / 2) % FRAMES.len()]
}

fn fill_color(fill: u64, window: u64) -> Color {
    if window == 0 {
        return Color::DarkGray;
    }
    let ratio = fill as f64 / window as f64;
    if ratio >= rates::ALERT_RATIO {
        Color::Red
    } else if ratio >= rates::WARN_RATIO {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn render_header(f: &mut Frame, area: Rect, state: &AppState, no_anim: bool) {
    let ring = ring_frame(state.tick, no_anim);
    let color = fill_color(state.fill, state.window);

    let line = Line::from(vec![
        Span::styled(
            ring,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            "LUMEN",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  ·  context monitor"),
    ]);

    f.render_widget(Paragraph::new(vec![line]), area);
}

// ── Body ──────────────────────────────────────────────────────────────────────

fn render_body(f: &mut Frame, area: Rect, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(area);

    render_gauge_panel(f, chunks[0], state);
    render_cost_panel(f, chunks[1], state);
}

fn render_gauge_panel(f: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Context Fill ")
        .title_style(Style::default().fg(Color::White));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if state.no_data {
        f.render_widget(
            Paragraph::new("\n  No data — start Claude Code while Lumen is running."),
            inner,
        );
        return;
    }

    let (fill, window) = (state.fill, state.window);
    let ratio = if window > 0 {
        (fill as f64 / window as f64).min(1.0)
    } else {
        0.0
    };
    let pct = (ratio * 100.0).round() as u64;
    let color = fill_color(fill, window);

    // Unicode block gauge
    let bar_width = inner.width.saturating_sub(2) as usize;
    let filled = ((bar_width as f64) * ratio).round() as usize;
    let filled = filled.min(bar_width);
    let empty = bar_width - filled;
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));

    let status = if window > 0 {
        let r = fill as f64 / window as f64;
        if r >= rates::ALERT_RATIO {
            ("⚠  ALERT — context nearly full", Color::Red)
        } else if r >= rates::WARN_RATIO {
            ("⚠  WARN — approaching limit", Color::Yellow)
        } else {
            ("●  OK", Color::Green)
        }
    } else {
        ("–  no data", Color::DarkGray)
    };

    let model_label = if state.model.is_empty() {
        "–".to_string()
    } else {
        state.model.clone()
    };

    let lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!(" {}", bar),
            Style::default().fg(color),
        )),
        Line::from(format!(
            "  {}%    {:>} / {} tokens",
            pct,
            fmt_u64(fill),
            fmt_u64(window)
        )),
        Line::from(""),
        Line::from(format!("  {}", model_label)),
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", status.0),
            Style::default().fg(status.1),
        )),
    ];

    f.render_widget(Paragraph::new(lines), inner);
}

fn render_cost_panel(f: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Cost ")
        .title_style(Style::default().fg(Color::White));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if state.no_data {
        return;
    }

    let session_cost = rates::session_cost(
        state.session_input,
        state.session_output,
        state.session_cache_read,
        state.session_cache_write,
    );
    let today_cost = rates::session_cost(
        state.today_input,
        state.today_output,
        state.today_cache_read,
        state.today_cache_write,
    );
    let cache_savings = rates::caching_savings(state.session_cache_read);

    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw(" SESSION  "),
            Span::styled(
                format!("${:.4}", session_cost),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw(" TODAY    "),
            Span::styled(
                format!("${:.4}", today_cost),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " ─────────────────",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            " Saved by caching",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            " (reported by Claude Code)",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            format!(" ${:.4}", cache_savings),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    if state.saved_tokens > 0 {
        let eff_pct = if state.full_tokens > 0 {
            (state.saved_tokens as f64 / state.full_tokens as f64 * 100.0).round() as u64
        } else {
            0
        };
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " ─────────────────",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Lumen optimizer",
            Style::default().fg(Color::Green),
        )));
        lines.push(Line::from(Span::styled(
            format!(" {}% · {} tok saved", eff_pct, fmt_i64(state.saved_tokens)),
            Style::default().fg(Color::Green),
        )));
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

// ── Footer ────────────────────────────────────────────────────────────────────

fn render_footer(f: &mut Frame, area: Rect, state: &AppState) {
    let daemon_span = if state.daemon_connected {
        Span::styled("● daemon: connected", Style::default().fg(Color::Green))
    } else {
        Span::styled(
            "○ daemon: offline — polling DB",
            Style::default().fg(Color::DarkGray),
        )
    };

    let line = Line::from(vec![
        daemon_span,
        Span::raw("   "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" quit"),
    ]);

    f.render_widget(Paragraph::new(vec![line]), area);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn fmt_u64(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() + s.len() / 3);
    for (i, &b) in bytes.iter().enumerate() {
        let from_end = bytes.len() - 1 - i;
        if i > 0 && from_end % 3 == 2 {
            out.push(b',');
        }
        out.push(b);
    }
    String::from_utf8(out).unwrap()
}

fn fmt_i64(n: i64) -> String {
    fmt_u64(n.max(0) as u64)
}
