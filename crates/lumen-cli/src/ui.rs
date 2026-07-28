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

    let mut spans = vec![
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
    ];

    // With several editor windows open the gauge follows whichever session is
    // newest, so name the project it belongs to rather than leaving it ambiguous.
    if !state.project.is_empty() {
        spans.push(Span::raw("  ·  "));
        spans.push(Span::styled(
            state.project.clone(),
            Style::default().fg(Color::Cyan),
        ));
    }

    let line = Line::from(spans);

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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    /// Render the whole TUI into an off-screen buffer of the given size.
    fn draw(state: &AppState, no_anim: bool, w: u16, h: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
        terminal
            .draw(|f| render(f, state, no_anim))
            .expect("draw must not fail");
        terminal.backend().buffer().clone()
    }

    /// Flatten the buffer to text so assertions can talk about what a user sees.
    fn text(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(c) = buf.cell((x, y)) {
                    out.push_str(c.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    /// Every distinct foreground colour present in the buffer.
    fn colors(buf: &Buffer) -> Vec<Color> {
        let mut seen = Vec::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(c) = buf.cell((x, y))
                    && !seen.contains(&c.fg)
                {
                    seen.push(c.fg);
                }
            }
        }
        seen
    }

    fn healthy() -> AppState {
        AppState {
            fill: 50_000,
            window: 200_000,
            model: "claude-sonnet-4".into(),
            session_input: 1_000,
            session_output: 2_000,
            session_cache_read: 3_000,
            session_cache_write: 4_000,
            daemon_connected: true,
            ..AppState::default()
        }
    }

    // ── fmt_u64 / fmt_i64 ────────────────────────────────────────────────────

    #[test]
    fn thousands_separators_are_inserted_from_the_right() {
        assert_eq!(fmt_u64(0), "0");
        assert_eq!(fmt_u64(7), "7");
        assert_eq!(fmt_u64(999), "999");
        assert_eq!(fmt_u64(1_000), "1,000");
        assert_eq!(fmt_u64(12_345), "12,345");
        assert_eq!(fmt_u64(123_456), "123,456");
        assert_eq!(fmt_u64(1_234_567), "1,234,567");
        assert_eq!(fmt_u64(1_000_000_000), "1,000,000,000");
    }

    #[test]
    fn formatting_the_largest_u64_does_not_panic() {
        assert_eq!(fmt_u64(u64::MAX), "18,446,744,073,709,551,615");
    }

    #[test]
    fn negative_counts_render_as_zero_rather_than_wrapping() {
        assert_eq!(fmt_i64(-1), "0");
        assert_eq!(fmt_i64(i64::MIN), "0");
        assert_eq!(fmt_i64(4_200), "4,200");
    }

    // ── fill_color thresholds ────────────────────────────────────────────────

    #[test]
    fn fill_color_is_grey_when_the_window_is_unknown() {
        assert_eq!(fill_color(0, 0), Color::DarkGray);
        assert_eq!(fill_color(50_000, 0), Color::DarkGray);
    }

    #[test]
    fn fill_color_follows_the_warn_and_alert_ratios() {
        let w = 200_000u64;
        let warn_at = (w as f64 * rates::WARN_RATIO) as u64;
        let alert_at = (w as f64 * rates::ALERT_RATIO) as u64;

        assert_eq!(fill_color(0, w), Color::Green);
        assert_eq!(fill_color(warn_at - 1, w), Color::Green);
        assert_eq!(fill_color(warn_at, w), Color::Yellow, "warn boundary");
        assert_eq!(fill_color(alert_at - 1, w), Color::Yellow);
        assert_eq!(fill_color(alert_at, w), Color::Red, "alert boundary");
        assert_eq!(fill_color(w, w), Color::Red);
        assert_eq!(fill_color(w * 2, w), Color::Red, "over-full stays red");
    }

    // ── ring_frame animation ─────────────────────────────────────────────────

    #[test]
    fn the_ring_is_frozen_when_animation_is_disabled() {
        let frozen = ring_frame(0, true);
        for tick in [0u64, 1, 7, 1_000, u64::MAX] {
            assert_eq!(
                ring_frame(tick, true),
                frozen,
                "tick {tick} must not animate"
            );
        }
    }

    #[test]
    fn the_ring_cycles_and_returns_to_its_first_frame() {
        // 5 frames at 2 ticks each = a 10-tick cycle.
        assert_eq!(ring_frame(0, false), ring_frame(10, false));
        assert_eq!(ring_frame(3, false), ring_frame(13, false));
        assert_ne!(
            ring_frame(0, false),
            ring_frame(4, false),
            "the frames must actually differ"
        );
    }

    #[test]
    fn the_ring_never_panics_at_the_tick_ceiling() {
        // tick is wrapping_add'd forever, so the modulo must hold at the top.
        assert!(!ring_frame(u64::MAX, false).is_empty());
    }

    // ── full render ──────────────────────────────────────────────────────────

    #[test]
    fn the_healthy_screen_shows_the_brand_gauge_and_cost() {
        let out = text(&draw(&healthy(), true, 80, 20));
        assert!(out.contains("LUMEN"), "header brand:\n{out}");
        assert!(out.contains("context monitor"));
        assert!(out.contains("Context Fill"), "gauge panel title");
        assert!(out.contains("Cost"), "cost panel title");
        assert!(out.contains("claude-sonnet-4"), "model label");
        assert!(out.contains("25%"), "50k of 200k is 25%:\n{out}");
        assert!(out.contains("50,000"), "fill is thousands-separated");
        assert!(out.contains("200,000"), "window is thousands-separated");
        assert!(out.contains("q"), "footer quit hint");
    }

    #[test]
    fn a_healthy_gauge_reports_ok_in_green() {
        let buf = draw(&healthy(), true, 80, 20);
        assert!(text(&buf).contains("OK"));
        assert!(colors(&buf).contains(&Color::Green));
    }

    #[test]
    fn crossing_the_warn_ratio_shows_a_yellow_warning() {
        let mut st = healthy();
        st.fill = (200_000.0 * rates::WARN_RATIO) as u64;
        let buf = draw(&st, true, 80, 20);
        let out = text(&buf);
        assert!(out.contains("WARN"), "{out}");
        assert!(out.contains("approaching limit"));
        assert!(colors(&buf).contains(&Color::Yellow));
    }

    #[test]
    fn crossing_the_alert_ratio_shows_a_red_alert() {
        let mut st = healthy();
        st.fill = (200_000.0 * rates::ALERT_RATIO) as u64;
        let buf = draw(&st, true, 80, 20);
        let out = text(&buf);
        assert!(out.contains("ALERT"), "{out}");
        assert!(out.contains("nearly full"));
        assert!(colors(&buf).contains(&Color::Red));
    }

    #[test]
    fn the_bar_is_empty_at_zero_and_full_at_capacity() {
        let mut st = healthy();

        st.fill = 0;
        let empty = text(&draw(&st, true, 80, 20));
        assert!(empty.contains('░'), "an empty bar shows light blocks");
        assert!(!empty.contains('█'), "and no filled blocks:\n{empty}");

        st.fill = st.window;
        let full = text(&draw(&st, true, 80, 20));
        assert!(full.contains('█'), "a full bar shows filled blocks");
        assert!(!full.contains('░'), "and no light blocks:\n{full}");
    }

    #[test]
    fn an_over_full_gauge_clamps_to_one_hundred_percent() {
        let mut st = healthy();
        st.fill = st.window * 3;
        let out = text(&draw(&st, true, 80, 20));
        assert!(out.contains("100%"), "must clamp, not print 300%:\n{out}");
        assert!(!out.contains('░'), "the bar must not overflow its width");
    }

    #[test]
    fn the_no_data_state_explains_itself_instead_of_showing_zeros() {
        let st = AppState {
            no_data: true,
            ..AppState::default()
        };
        let out = text(&draw(&st, true, 80, 20));
        assert!(out.contains("No data"), "{out}");
        assert!(out.contains("start Claude Code"));
        assert!(
            !out.contains("SESSION"),
            "the cost panel stays blank:\n{out}"
        );
    }

    #[test]
    fn a_missing_model_renders_as_a_dash() {
        let mut st = healthy();
        st.model = String::new();
        let out = text(&draw(&st, true, 80, 20));
        assert!(
            out.contains(" – "),
            "expected an em-dash placeholder:\n{out}"
        );
    }

    #[test]
    fn an_unknown_window_shows_no_data_in_the_status_line() {
        let mut st = healthy();
        st.window = 0;
        let out = text(&draw(&st, true, 80, 20));
        assert!(out.contains("no data"), "{out}");
        assert!(out.contains("0%"), "an unknown window means 0%");
    }

    // ── cost panel ───────────────────────────────────────────────────────────

    #[test]
    fn the_cost_panel_prices_the_session_and_today() {
        let mut st = healthy();
        st.today_input = 10_000;
        st.today_output = 20_000;
        let out = text(&draw(&st, true, 100, 24));
        assert!(out.contains("SESSION"), "{out}");
        assert!(out.contains("TODAY"));
        let expected = rates::session_cost(1_000, 2_000, 3_000, 4_000);
        assert!(
            out.contains(&format!("${expected:.4}")),
            "expected ${expected:.4} in:\n{out}"
        );
    }

    #[test]
    fn caching_savings_are_labelled_as_reported_not_caused() {
        // The honesty rule: cache savings come FROM Claude Code, they are not
        // something Lumen achieved. The label must say so.
        let out = text(&draw(&healthy(), true, 100, 30));
        assert!(out.contains("Saved by caching"), "{out}");
        assert!(out.contains("reported by Claude Code"), "{out}");
    }

    #[test]
    fn the_optimizer_block_is_hidden_until_lumen_has_saved_something() {
        let out = text(&draw(&healthy(), true, 100, 30));
        assert!(
            !out.contains("Lumen optimizer"),
            "nothing saved yet, so claim nothing:\n{out}"
        );
    }

    #[test]
    fn the_optimizer_block_reports_effectiveness_once_there_are_savings() {
        let mut st = healthy();
        st.saved_tokens = 750;
        st.full_tokens = 1_000;
        let out = text(&draw(&st, true, 100, 30));
        assert!(out.contains("Lumen optimizer"), "{out}");
        assert!(out.contains("75%"), "750/1000 is 75%:\n{out}");
        assert!(out.contains("750"), "and the raw token count:\n{out}");
    }

    #[test]
    fn effectiveness_is_zero_rather_than_a_division_by_zero() {
        let mut st = healthy();
        st.saved_tokens = 500;
        st.full_tokens = 0; // nonsensical, but must not panic or print NaN
        let out = text(&draw(&st, true, 100, 30));
        assert!(out.contains("0%"), "{out}");
        assert!(!out.contains("NaN"), "{out}");
        assert!(!out.contains("inf"), "{out}");
    }

    // ── footer ───────────────────────────────────────────────────────────────

    #[test]
    fn the_footer_distinguishes_a_live_daemon_from_db_polling() {
        let mut st = healthy();

        st.daemon_connected = true;
        let connected = text(&draw(&st, true, 80, 20));
        assert!(connected.contains("daemon: connected"), "{connected}");

        st.daemon_connected = false;
        let offline = text(&draw(&st, true, 80, 20));
        assert!(offline.contains("daemon: offline"), "{offline}");
        assert!(
            offline.contains("polling DB"),
            "offline must say what it fell back to:\n{offline}"
        );
    }

    // ── robustness ───────────────────────────────────────────────────────────

    #[test]
    fn rendering_survives_a_tiny_terminal() {
        // A 20x10 terminal leaves the panels almost no room; layout must not
        // panic on a saturating_sub or an out-of-bounds slice.
        for (w, h) in [(20u16, 10u16), (30, 12), (40, 11)] {
            let buf = draw(&healthy(), true, w, h);
            assert_eq!(buf.area.width, w);
            assert_eq!(buf.area.height, h);
        }
    }

    #[test]
    fn rendering_survives_a_very_wide_terminal() {
        let buf = draw(&healthy(), true, 300, 60);
        assert!(text(&buf).contains("LUMEN"));
    }

    #[test]
    fn every_animation_frame_renders_cleanly() {
        let mut st = healthy();
        for tick in 0..12 {
            st.tick = tick;
            let out = text(&draw(&st, false, 80, 20));
            assert!(out.contains("LUMEN"), "tick {tick} broke the header");
        }
    }
}
