use std::{
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use lumen_core::{meter::db_path, rates};
use serde::Deserialize;

#[derive(Default)]
pub struct AppState {
    pub fill: u64,
    /// Session high-water mark. The window is derived from this, never from
    /// `fill`: /compact drops fill sharply and would otherwise shrink the window.
    pub peak_fill: u64,
    pub model: String,
    /// Short project label for the session being shown. With several editor
    /// windows open the gauge follows whichever is newest, so it has to say which.
    pub project: String,
    pub window: u64,
    pub session_input: i64,
    pub session_output: i64,
    pub session_cache_read: i64,
    pub session_cache_write: i64,
    pub today_input: i64,
    pub today_output: i64,
    pub today_cache_read: i64,
    pub today_cache_write: i64,
    pub saved_tokens: i64,
    pub full_tokens: i64,
    pub daemon_connected: bool,
    pub no_data: bool,
    pub tick: u64,
    pub last_update: Option<Instant>,
}

pub struct OneshotData {
    pub fill: u64,
    pub window: u64,
    pub model: String,
}

// ── WS message shapes ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    sessions: Option<Vec<Session>>,
    turn: Option<Turn>,
}

#[derive(Deserialize)]
struct Session {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    fill: i64,
    /// Absent on daemons older than this field; falls back to `fill`.
    #[serde(default)]
    peak_fill: Option<i64>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    project: Option<String>,
    ts: String,
}

#[derive(Deserialize)]
struct Turn {
    model: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    /// Subagent turns cost money but carry their own context — see
    /// lumen_core::project. Absent on older daemons, defaulting to false.
    #[serde(default)]
    is_subagent: bool,
    #[serde(default)]
    project: Option<String>,
}

// ── Background data loop ──────────────────────────────────────────────────────

pub fn run(state: Arc<Mutex<AppState>>) {
    loop {
        match tungstenite::connect("ws://127.0.0.1:9999") {
            Ok((mut ws, _)) => {
                state.lock().unwrap().daemon_connected = true;
                loop {
                    let msg = match ws.read() {
                        Ok(m) => m,
                        Err(_) => break,
                    };
                    if msg.is_close() {
                        break;
                    }
                    if msg.is_text()
                        && let Ok(text) = msg.into_text()
                    {
                        apply_msg(&text, &state);
                    }
                }
                state.lock().unwrap().daemon_connected = false;
                // Brief pause before reconnect attempt
                thread::sleep(Duration::from_millis(500));
            }
            Err(_) => {
                // Daemon offline — fall back to DB polling
                poll_db(&state);
                thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

fn apply_msg(text: &str, state: &Arc<Mutex<AppState>>) {
    let env: Envelope = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };
    let mut s = state.lock().unwrap();
    s.last_update = Some(Instant::now());
    s.no_data = false;

    match env.kind.as_str() {
        "snapshot" => {
            if let Some(sessions) = env.sessions {
                // Use the session with the latest ts for the fill value
                if let Some(latest) = sessions.iter().max_by_key(|x| &x.ts) {
                    s.fill = latest.fill.max(0) as u64;
                    // Prefer the daemon's peak; fall back to the current fill.
                    let peak = latest.peak_fill.unwrap_or(latest.fill).max(0) as u64;
                    s.peak_fill = s.peak_fill.max(peak).max(s.fill);
                    if let Some(m) = latest.model.as_deref()
                        && !m.is_empty()
                    {
                        s.model = m.to_string();
                    }
                    if let Some(p) = latest.project.as_deref()
                        && !p.is_empty()
                    {
                        s.project = p.to_string();
                    }
                    s.window = rates::resolve_window(&s.model, s.peak_fill);
                }
                // Cost is scoped to the session the gauge is showing, so the two
                // agree. Summing every session made "SESSION" actually mean "all
                // sessions", which read as inflated next to one window's context.
                if let Some(latest) = sessions.iter().max_by_key(|x| &x.ts) {
                    s.session_input = latest.input;
                    s.session_output = latest.output;
                    s.session_cache_read = latest.cache_read;
                    s.session_cache_write = latest.cache_write;
                }
            }
        }
        "event" => {
            if let Some(t) = env.turn {
                // Cost counts every turn, subagents included — it is real spend.
                s.session_input += t.input_tokens;
                s.session_output += t.output_tokens;
                s.session_cache_read += t.cache_read_input_tokens;
                s.session_cache_write += t.cache_creation_input_tokens;

                if let Some(p) = t.project.as_deref()
                    && !p.is_empty()
                {
                    s.project = p.to_string();
                }

                // The gauge does NOT. A subagent reuses the parent's sessionId but
                // starts with a fresh, small context, so letting it set `fill` made
                // the gauge dip every time one ran.
                if !t.is_subagent {
                    s.fill = t.cache_read_input_tokens.max(0) as u64;
                    s.peak_fill = s.peak_fill.max(s.fill);
                    if !t.model.is_empty() {
                        s.model = t.model;
                    }
                    // Model first, peak second — see rates::resolve_window.
                    s.window = rates::resolve_window(&s.model, s.peak_fill);
                }
            }
        }
        _ => {}
    }
}

fn poll_db(state: &Arc<Mutex<AppState>>) {
    let Some(path) = db_path() else {
        state.lock().unwrap().no_data = true;
        return;
    };
    poll_db_at(&path, state);
}

/// Fill `state` from the DB at `path`. Split out from `poll_db` so tests can
/// point it at a tempdir rather than mutating the ambient LUMEN_DB — env
/// mutation is racy across test threads and `unsafe` in edition 2024.
fn poll_db_at(path: &std::path::Path, state: &Arc<Mutex<AppState>>) {
    let conn = match lumen_core::meter::connect_db(path) {
        Ok(c) => c,
        Err(_) => {
            state.lock().unwrap().no_data = true;
            return;
        }
    };

    let row = conn.query_row(
        "SELECT model, cache_read_input_tokens, session_id \
         FROM turns ORDER BY ts DESC LIMIT 1",
        [],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        },
    );
    let (model, fill_raw, session_id) = match row {
        Ok(v) => v,
        Err(_) => {
            state.lock().unwrap().no_data = true;
            return;
        }
    };
    let fill = fill_raw.max(0) as u64;
    // Peak over the whole session, so the window survives a /compact.
    let peak: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(cache_read_input_tokens),0) FROM turns WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(fill_raw);
    let peak_fill = peak.max(fill_raw).max(0) as u64;
    let window = rates::resolve_window(&model, peak_fill);

    let sess = conn
        .query_row(
            "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                    COALESCE(SUM(cache_read_input_tokens),0),
                    COALESCE(SUM(cache_creation_input_tokens),0)
             FROM turns WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .unwrap_or((0, 0, 0, 0));

    let today = conn
        .query_row(
            "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                    COALESCE(SUM(cache_read_input_tokens),0),
                    COALESCE(SUM(cache_creation_input_tokens),0)
             FROM turns WHERE ts >= date('now')",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .unwrap_or((0, 0, 0, 0));

    let opt = conn
        .query_row(
            "SELECT COALESCE(SUM(saved_tokens),0), COALESCE(SUM(full_tokens),0) FROM read_events",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .unwrap_or((0, 0));

    let mut s = state.lock().unwrap();
    s.fill = fill;
    s.peak_fill = peak_fill;
    s.window = window;
    s.model = model;
    s.session_input = sess.0;
    s.session_output = sess.1;
    s.session_cache_read = sess.2;
    s.session_cache_write = sess.3;
    s.today_input = today.0;
    s.today_output = today.1;
    s.today_cache_read = today.2;
    s.today_cache_write = today.3;
    s.saved_tokens = opt.0;
    s.full_tokens = opt.1;
    s.no_data = false;
    s.last_update = Some(Instant::now());
}

pub fn read_db_oneshot() -> Option<OneshotData> {
    read_db_oneshot_at(&db_path()?)
}

/// Read the one-shot summary from the DB at `path`. See `poll_db_at` for why the
/// path is a parameter.
fn read_db_oneshot_at(path: &std::path::Path) -> Option<OneshotData> {
    let conn = lumen_core::meter::connect_db(path).ok()?;
    let (model, fill_raw, session_id) = conn
        .query_row(
            "SELECT model, cache_read_input_tokens, session_id \
             FROM turns ORDER BY ts DESC LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .ok()?;
    let fill = fill_raw.max(0) as u64;
    let peak: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(cache_read_input_tokens),0) FROM turns WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(fill_raw);
    let window = rates::resolve_window(&model, peak.max(fill_raw).max(0) as u64);
    Some(OneshotData {
        fill,
        window,
        model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::meter::connect_db;
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn state() -> Arc<Mutex<AppState>> {
        Arc::default()
    }

    /// A snapshot envelope with one session per supplied tuple:
    /// (session_id, input, output, cache_read, cache_write, fill, ts).
    /// Carries no model or peak_fill — exercises the pre-upgrade daemon shape.
    fn snapshot(rows: &[(&str, i64, i64, i64, i64, i64, &str)]) -> String {
        let sessions: Vec<String> = rows
            .iter()
            .map(|(id, i, o, cr, cw, fill, ts)| {
                format!(
                    r#"{{"session_id":"{id}","input":{i},"output":{o},"cache_read":{cr},
                        "cache_write":{cw},"fill":{fill},"ts":"{ts}"}}"#
                )
            })
            .collect();
        format!(
            r#"{{"type":"snapshot","sessions":[{}]}}"#,
            sessions.join(",")
        )
    }

    /// A snapshot carrying the model and peak fill the current daemon sends.
    fn snapshot_full(session: &str, fill: i64, peak: i64, model: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"snapshot","sessions":[{{"session_id":"{session}","input":0,
               "output":0,"cache_read":0,"cache_write":0,"fill":{fill},
               "peak_fill":{peak},"model":"{model}","ts":"{ts}"}}]}}"#
        )
    }

    /// An event envelope for one turn.
    fn event(model: &str, input: i64, output: i64, cache_read: i64, cache_write: i64) -> String {
        format!(
            r#"{{"type":"event","turn":{{"model":"{model}","input_tokens":{input},
                "output_tokens":{output},"cache_read_input_tokens":{cache_read},
                "cache_creation_input_tokens":{cache_write}}}}}"#
        )
    }

    /// An event envelope for a SUBAGENT turn, as the daemon now flags them.
    fn subagent_event(cache_read: i64, input: i64, output: i64) -> String {
        format!(
            r#"{{"type":"event","turn":{{"model":"claude-opus-4-8","input_tokens":{input},
                "output_tokens":{output},"cache_read_input_tokens":{cache_read},
                "cache_creation_input_tokens":0,"is_subagent":true,
                "project":"lumen"}}}}"#
        )
    }

    // ── apply_msg: snapshot frames ───────────────────────────────────────────

    #[test]
    fn snapshot_scopes_cost_to_the_session_the_gauge_shows() {
        // Cost and gauge must describe the same session. Summing every session
        // made "SESSION" mean "all sessions", which looks inflated next to one
        // window's context — and gets worse the more editor windows are open.
        let st = state();
        apply_msg(
            &snapshot(&[
                ("newest", 10, 20, 30, 40, 1_000, "2026-01-01T10:00:00Z"),
                ("older", 1, 2, 3, 4, 500, "2026-01-01T09:00:00Z"),
            ]),
            &st,
        );
        let s = st.lock().unwrap();
        assert_eq!(s.session_input, 10, "only the active session's tokens");
        assert_eq!(s.session_output, 20);
        assert_eq!(s.session_cache_read, 30);
        assert_eq!(s.session_cache_write, 40);
    }

    #[test]
    fn snapshot_takes_fill_from_the_latest_session_only() {
        let st = state();
        apply_msg(
            &snapshot(&[
                ("stale", 0, 0, 0, 0, 190_000, "2026-01-01T09:00:00Z"),
                ("fresh", 0, 0, 0, 0, 12_000, "2026-01-01T12:00:00Z"),
            ]),
            &st,
        );
        assert_eq!(
            st.lock().unwrap().fill,
            12_000,
            "fill is a point-in-time gauge, not a sum — newest ts wins"
        );
    }

    #[test]
    fn snapshot_infers_the_window_from_the_observed_fill() {
        let st = state();
        apply_msg(
            &snapshot(&[("a", 0, 0, 0, 0, 250_000, "2026-01-01T10:00:00Z")]),
            &st,
        );
        assert_eq!(st.lock().unwrap().window, rates::infer_window(250_000));
    }

    #[test]
    fn snapshot_clamps_a_negative_fill_to_zero() {
        let st = state();
        apply_msg(
            &snapshot(&[("a", 0, 0, 0, 0, -7, "2026-01-01T10:00:00Z")]),
            &st,
        );
        assert_eq!(
            st.lock().unwrap().fill,
            0,
            "fill is u64 — a negative must clamp, not wrap to 18 quintillion"
        );
    }

    #[test]
    fn an_empty_snapshot_clears_no_data_without_touching_the_gauge() {
        let st = state();
        st.lock().unwrap().no_data = true;
        apply_msg(&snapshot(&[]), &st);
        let s = st.lock().unwrap();
        assert!(!s.no_data, "a frame arrived, so we are connected");
        assert_eq!(s.fill, 0);
    }

    // ── apply_msg: event frames ──────────────────────────────────────────────

    #[test]
    fn an_event_accumulates_cost_and_replaces_fill() {
        let st = state();
        apply_msg(&event("claude-sonnet-4", 10, 20, 5_000, 40), &st);
        apply_msg(&event("claude-sonnet-4", 1, 2, 9_000, 4), &st);
        let s = st.lock().unwrap();
        assert_eq!(s.session_input, 11, "input accumulates");
        assert_eq!(s.session_output, 22);
        assert_eq!(s.session_cache_write, 44);
        assert_eq!(s.fill, 9_000, "fill is replaced by the latest turn");
    }

    #[test]
    fn an_event_records_the_model() {
        let st = state();
        apply_msg(&event("claude-opus-4", 0, 0, 0, 0), &st);
        assert_eq!(st.lock().unwrap().model, "claude-opus-4");
    }

    #[test]
    fn an_event_with_an_empty_model_keeps_the_last_known_one() {
        let st = state();
        apply_msg(&event("claude-opus-4", 0, 0, 0, 0), &st);
        apply_msg(&event("", 0, 0, 0, 0), &st);
        assert_eq!(
            st.lock().unwrap().model,
            "claude-opus-4",
            "an empty model must not blank a known one"
        );
    }

    #[test]
    fn an_event_clamps_a_negative_fill_to_zero() {
        let st = state();
        apply_msg(&event("m", 0, 0, -1, 0), &st);
        assert_eq!(st.lock().unwrap().fill, 0);
    }

    #[test]
    fn an_event_marks_the_state_as_updated() {
        let st = state();
        assert!(st.lock().unwrap().last_update.is_none());
        apply_msg(&event("m", 0, 0, 0, 0), &st);
        assert!(st.lock().unwrap().last_update.is_some());
    }

    // ── subagent turns ───────────────────────────────────────────────────────

    #[test]
    fn a_subagent_turn_adds_cost_but_does_not_move_the_gauge() {
        // Subagent transcripts reuse the parent's sessionId, so their turns arrive
        // on the same session. Their tokens are real spend; their small fresh
        // context is not this session's context.
        let st = state();
        apply_msg(&event("claude-opus-4-8", 10, 20, 300_000, 5), &st);
        apply_msg(&subagent_event(4_000, 7, 9), &st);

        let s = st.lock().unwrap();
        assert_eq!(
            s.fill, 300_000,
            "the gauge must not dip to the subagent's 4,000"
        );
        assert_eq!(s.peak_fill, 300_000);
        assert_eq!(s.session_input, 17, "but the cost includes both turns");
        assert_eq!(s.session_output, 29);
        assert_eq!(s.session_cache_read, 304_000);
    }

    #[test]
    fn a_subagent_turn_still_reports_the_project() {
        let st = state();
        apply_msg(&subagent_event(1_000, 1, 1), &st);
        assert_eq!(st.lock().unwrap().project, "lumen");
    }

    #[test]
    fn a_subagent_turn_does_not_overwrite_the_model_or_window() {
        let st = state();
        apply_msg(&event("claude-haiku-4-5", 0, 0, 100_000, 0), &st);
        // The subagent runs opus-4-8; adopting its model would jump the window
        // from 200K to 1M for a session that is actually on Haiku.
        apply_msg(&subagent_event(1_000, 0, 0), &st);
        let s = st.lock().unwrap();
        assert_eq!(s.model, "claude-haiku-4-5");
        assert_eq!(s.window, 200_000);
    }

    #[test]
    fn a_turn_without_the_subagent_flag_is_treated_as_main_agent() {
        // Older daemons omit the field; serde defaults it to false.
        let st = state();
        apply_msg(&event("claude-opus-4-8", 0, 0, 123_000, 0), &st);
        assert_eq!(st.lock().unwrap().fill, 123_000);
    }

    // ── project label ────────────────────────────────────────────────────────

    #[test]
    fn a_snapshot_reports_the_project_label() {
        let st = state();
        apply_msg(
            r#"{"type":"snapshot","sessions":[{"session_id":"s1","input":0,"output":0,
               "cache_read":0,"cache_write":0,"fill":5000,"peak_fill":5000,
               "model":"claude-opus-4-8","project":"datapulse-gitlab",
               "ts":"2026-01-01T10:00:00Z"}]}"#,
            &st,
        );
        assert_eq!(st.lock().unwrap().project, "datapulse-gitlab");
    }

    #[test]
    fn a_missing_project_leaves_the_label_empty_rather_than_guessing() {
        let st = state();
        apply_msg(&event("claude-opus-4-8", 0, 0, 1_000, 0), &st);
        assert!(st.lock().unwrap().project.is_empty());
    }

    // ── apply_msg: malformed input ───────────────────────────────────────────

    #[test]
    fn malformed_json_is_ignored_without_panicking() {
        let st = state();
        for bad in [
            "",
            "not json",
            "{",
            "[]",
            "null",
            r#"{"type":"event"}"#,           // event with no turn
            r#"{"type":"snapshot"}"#,        // snapshot with no sessions
            r#"{"type":"event","turn":{}}"#, // turn missing every field
        ] {
            apply_msg(bad, &st);
        }
        let s = st.lock().unwrap();
        assert_eq!(s.fill, 0);
        assert_eq!(s.session_input, 0);
        assert!(s.model.is_empty());
    }

    #[test]
    fn an_unknown_envelope_kind_is_ignored_but_still_counts_as_contact() {
        let st = state();
        st.lock().unwrap().no_data = true;
        apply_msg(r#"{"type":"heartbeat"}"#, &st);
        let s = st.lock().unwrap();
        assert!(!s.no_data, "we heard from the daemon");
        assert_eq!(s.fill, 0, "but nothing about the gauge changed");
    }

    #[test]
    fn a_malformed_frame_does_not_clear_no_data() {
        let st = state();
        st.lock().unwrap().no_data = true;
        apply_msg("garbage", &st);
        assert!(
            st.lock().unwrap().no_data,
            "unparseable bytes are not evidence of a healthy daemon"
        );
    }

    // ── context window resolution ────────────────────────────────────────────

    #[test]
    fn a_known_model_gets_its_published_window_not_one_inferred_from_fill() {
        // The reported bug: 267,593 / 500,000 on a model whose window is 1M.
        let st = state();
        apply_msg(&event("claude-opus-4-8", 0, 0, 267_593, 0), &st);
        let s = st.lock().unwrap();
        assert_eq!(s.fill, 267_593);
        assert_eq!(
            s.window, 1_000_000,
            "opus-4-8 has a 1M window; inferring 500K from the fill was the bug"
        );
    }

    #[test]
    fn haiku_keeps_its_smaller_window() {
        let st = state();
        apply_msg(&event("claude-haiku-4-5-20251001", 0, 0, 150_000, 0), &st);
        assert_eq!(st.lock().unwrap().window, 200_000);
    }

    #[test]
    fn an_unknown_model_still_infers_from_the_peak() {
        let st = state();
        apply_msg(&event("some-future-model", 0, 0, 250_000, 0), &st);
        assert_eq!(st.lock().unwrap().window, 500_000);
    }

    #[test]
    fn a_compaction_does_not_shrink_the_window() {
        // Real pattern from the shipped DB: fill climbs, drops hard on /compact,
        // then climbs again. Inferring from the momentary fill made the window
        // (and therefore the whole gauge) jump.
        let st = state();
        apply_msg(&event("mystery-model", 0, 0, 317_241, 0), &st);
        let after_peak = st.lock().unwrap().window;
        assert_eq!(after_peak, 500_000);

        apply_msg(&event("mystery-model", 0, 0, 16_794, 0), &st);
        let s = st.lock().unwrap();
        assert_eq!(s.fill, 16_794, "the gauge follows the current fill");
        assert_eq!(
            s.window, 500_000,
            "but the window holds at the peak instead of collapsing to 200K"
        );
        assert_eq!(s.peak_fill, 317_241);
    }

    #[test]
    fn the_peak_only_ever_rises() {
        let st = state();
        for fill in [1_000i64, 50_000, 20_000, 90_000, 10_000] {
            apply_msg(&event("m", 0, 0, fill, 0), &st);
        }
        assert_eq!(st.lock().unwrap().peak_fill, 90_000);
    }

    #[test]
    fn a_snapshot_uses_the_daemons_peak_and_model() {
        let st = state();
        // Post-compaction fill of 12K, but the session peaked at 400K on a 1M model.
        apply_msg(
            &snapshot_full(
                "s1",
                12_000,
                400_000,
                "claude-opus-4-8",
                "2026-01-01T10:00:00Z",
            ),
            &st,
        );
        let s = st.lock().unwrap();
        assert_eq!(s.fill, 12_000);
        assert_eq!(s.peak_fill, 400_000);
        assert_eq!(s.model, "claude-opus-4-8");
        assert_eq!(s.window, 1_000_000);
    }

    #[test]
    fn a_snapshot_without_peak_or_model_still_works() {
        // An older daemon omits both fields; the client must not break.
        let st = state();
        apply_msg(
            &snapshot(&[("a", 0, 0, 0, 0, 250_000, "2026-01-01T10:00:00Z")]),
            &st,
        );
        let s = st.lock().unwrap();
        assert_eq!(s.peak_fill, 250_000, "falls back to the current fill");
        assert_eq!(s.window, 500_000, "and to tier inference");
    }

    // ── DB fallback path ─────────────────────────────────────────────────────

    /// Temp DB with the production schema plus the given turns:
    /// (message_id, session_id, ts, model, input, output, cache_read, cache_write).
    #[allow(clippy::type_complexity)]
    fn db_with(
        turns: &[(&str, &str, &str, &str, i64, i64, i64, i64)],
    ) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let conn: Connection = connect_db(&path).unwrap();
        for (mid, sid, ts, model, i, o, cr, cw) in turns {
            conn.execute(
                "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,output_tokens,
                                   cache_read_input_tokens,cache_creation_input_tokens)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                rusqlite::params![mid, sid, ts, model, i, o, cr, cw],
            )
            .unwrap();
        }
        drop(conn);
        (dir, path)
    }

    #[test]
    fn oneshot_reads_the_most_recent_turn() {
        let (_d, path) = db_with(&[
            (
                "m1",
                "s1",
                "2026-01-01T10:00:00Z",
                "claude-sonnet-4",
                1,
                1,
                5_000,
                0,
            ),
            (
                "m2",
                "s1",
                "2026-01-01T12:00:00Z",
                "claude-opus-4",
                1,
                1,
                42_000,
                0,
            ),
        ]);
        let d = read_db_oneshot_at(&path).expect("a row exists");
        assert_eq!(d.fill, 42_000, "newest ts wins");
        assert_eq!(d.model, "claude-opus-4");
        assert_eq!(d.window, rates::infer_window(42_000));
    }

    #[test]
    fn oneshot_returns_none_for_an_empty_db() {
        let (_d, path) = db_with(&[]);
        assert!(read_db_oneshot_at(&path).is_none());
    }

    #[test]
    fn oneshot_returns_none_for_a_missing_db() {
        let dir = TempDir::new().unwrap();
        // connect_db would create the file, but with no turns table populated
        // there is still no row to read.
        assert!(read_db_oneshot_at(&dir.path().join("nope.db")).is_none());
    }

    #[test]
    fn oneshot_clamps_a_negative_stored_fill() {
        let (_d, path) = db_with(&[("m1", "s1", "2026-01-01T10:00:00Z", "m", 0, 0, -3, 0)]);
        assert_eq!(read_db_oneshot_at(&path).unwrap().fill, 0);
    }

    #[test]
    fn poll_fills_state_from_the_db() {
        let (_d, path) = db_with(&[
            (
                "m1",
                "s1",
                "2026-01-01T10:00:00Z",
                "claude-sonnet-4",
                10,
                100,
                1_000,
                5,
            ),
            (
                "m2",
                "s1",
                "2026-01-01T11:00:00Z",
                "claude-sonnet-4",
                20,
                200,
                7_000,
                6,
            ),
        ]);
        let st = state();
        poll_db_at(&path, &st);
        let s = st.lock().unwrap();
        assert!(!s.no_data);
        assert_eq!(s.fill, 7_000, "gauge tracks the newest turn");
        assert_eq!(s.model, "claude-sonnet-4");
        assert_eq!(s.session_input, 30, "session totals are summed");
        assert_eq!(s.session_output, 300);
        assert_eq!(s.session_cache_read, 8_000);
        assert_eq!(s.session_cache_write, 11);
        assert!(s.last_update.is_some());
    }

    #[test]
    fn poll_scopes_session_totals_to_the_active_session() {
        let (_d, path) = db_with(&[
            (
                "m1",
                "other",
                "2026-01-01T09:00:00Z",
                "m",
                999,
                999,
                999,
                999,
            ),
            ("m2", "active", "2026-01-01T12:00:00Z", "m", 5, 6, 7, 8),
        ]);
        let st = state();
        poll_db_at(&path, &st);
        let s = st.lock().unwrap();
        assert_eq!(
            s.session_input, 5,
            "another session's tokens must not leak into the active one"
        );
        assert_eq!(s.session_output, 6);
    }

    #[test]
    fn poll_marks_no_data_for_an_empty_db() {
        let (_d, path) = db_with(&[]);
        let st = state();
        poll_db_at(&path, &st);
        assert!(st.lock().unwrap().no_data);
    }

    #[test]
    fn poll_sums_optimizer_totals_from_read_events() {
        let (_d, path) = db_with(&[("m1", "s1", "2026-01-01T10:00:00Z", "m", 0, 0, 0, 0)]);
        let conn = connect_db(&path).unwrap();
        for (saved, full) in [(900i64, 1_000i64), (400, 500)] {
            conn.execute(
                "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,
                                         saved_tokens,routed_via,channel)
                 VALUES('2026-01-01T10:00:00Z','t','/p.rs',10,?1,?2,?3,'smart_read','cli')",
                rusqlite::params![full - saved, full, saved],
            )
            .unwrap();
        }
        drop(conn);

        let st = state();
        poll_db_at(&path, &st);
        let s = st.lock().unwrap();
        assert_eq!(s.saved_tokens, 1_300);
        assert_eq!(s.full_tokens, 1_500);
    }
}
