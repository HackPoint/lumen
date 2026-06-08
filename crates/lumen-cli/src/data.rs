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
    pub model: String,
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
    ts: String,
}

#[derive(Deserialize)]
struct Turn {
    model: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
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
                    if msg.is_text() {
                        if let Ok(text) = msg.into_text() {
                            apply_msg(&text, &state);
                        }
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
                    s.window = rates::infer_window(s.fill);
                }
                // Sum all sessions for cost display
                s.session_input = sessions.iter().map(|x| x.input).sum();
                s.session_output = sessions.iter().map(|x| x.output).sum();
                s.session_cache_read = sessions.iter().map(|x| x.cache_read).sum();
                s.session_cache_write = sessions.iter().map(|x| x.cache_write).sum();
            }
        }
        "event" => {
            if let Some(t) = env.turn {
                s.session_input += t.input_tokens;
                s.session_output += t.output_tokens;
                s.session_cache_read += t.cache_read_input_tokens;
                s.session_cache_write += t.cache_creation_input_tokens;
                s.fill = t.cache_read_input_tokens.max(0) as u64;
                s.window = rates::infer_window(s.fill);
                if !t.model.is_empty() {
                    s.model = t.model;
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
    let conn = match lumen_core::meter::connect_db(&path) {
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
    let window = rates::infer_window(fill);

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
    let path = db_path()?;
    let conn = lumen_core::meter::connect_db(&path).ok()?;
    let (model, fill_raw) = conn
        .query_row(
            "SELECT model, cache_read_input_tokens FROM turns ORDER BY ts DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .ok()?;
    let fill = fill_raw.max(0) as u64;
    let window = rates::infer_window(fill);
    Some(OneshotData { fill, window, model })
}
