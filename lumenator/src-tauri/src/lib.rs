mod setup;
use futures_util::StreamExt;
use serde::Serialize;
use sqlx::sqlite::SqlitePoolOptions;
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent, TrayIconId};
use tauri::{Emitter, Manager, State, WindowEvent};
use tauri_plugin_positioner::{Position, WindowExt};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

/// Holds the most recent snapshot JSON received from the daemon, so the
/// frontend can fetch it on demand (avoids the connect-before-listen race).
#[derive(Default)]
struct SnapshotCache(Mutex<Option<String>>);

async fn connect_daemon(app: tauri::AppHandle) {
    loop {
        match tokio_tungstenite::connect_async("ws://127.0.0.1:9999").await {
            Ok((ws, _)) => {
                let (_w, mut read) = ws.split();
                while let Some(Ok(msg)) = read.next().await {
                    if let Ok(text) = msg.into_text() {
                        let text = text.to_string();
                        // cache snapshots so a late-loading frontend can request them
                        if text.contains("\"type\":\"snapshot\"") {
                            if let Some(cache) = app.try_state::<SnapshotCache>() {
                                *cache.0.lock().unwrap() = Some(text.clone());
                            }
                        }
                        // forward raw daemon JSON to the frontend as a "daemon" event
                        let _ = app.emit("daemon", text);
                    }
                }
            }
            Err(_) => {}
        }
        // daemon not up or disconnected — retry shortly
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tray icon: a firefly silhouette in the current status color. Rendered as
// a COLORED image (icon_as_template(false)) at 2× for retina sharpness.
// ─────────────────────────────────────────────────────────────────────────

const ICON_SIZE: u32 = 44; // 2× the ~22pt menu-bar slot

/// Status -> RGB, matching the frontend thresholds (warn ≥ .80, alert ≥ .95).
fn status_rgb(status: &str) -> (u8, u8, u8) {
    match status {
        "alert" => (248, 81, 73), // red
        "warn" => (210, 153, 34), // amber
        _ => (63, 185, 80),       // green (ok)
    }
}

fn in_circle(fx: f32, fy: f32, cx: f32, cy: f32, r: f32) -> bool {
    let dx = fx - cx;
    let dy = fy - cy;
    dx * dx + dy * dy <= r * r
}

fn in_ellipse(fx: f32, fy: f32, cx: f32, cy: f32, erx: f32, ery: f32, angle: f32) -> bool {
    let dx = fx - cx;
    let dy = fy - cy;
    let (sin_a, cos_a) = angle.sin_cos();
    let lx = dx * cos_a + dy * sin_a;
    let ly = -dx * sin_a + dy * cos_a;
    (lx / erx) * (lx / erx) + (ly / ery) * (ly / ery) <= 1.0
}

/// Render the firefly silhouette tray icon for a given status.
fn render_tray_icon(_percent: u8, status: &str) -> tauri::image::Image<'static> {
    let size = ICON_SIZE;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let col = status_rgb(status);
    let ss = 3u32;

    // Shape parameters (pixels, origin top-left, canvas 44×44)
    let (hcx, hcy, hr)        = (22.0_f32,  8.5_f32,  4.0_f32);            // head
    let (bcx, bcy, brx, bry)  = (22.0_f32, 20.0_f32,  5.0_f32,  9.0_f32); // body
    let (lcx, lcy, lwx, lwy)  = (10.0_f32, 17.5_f32,  9.5_f32,  3.0_f32); // left wing
    let (rcx, rcy, rwx, rwy)  = (34.0_f32, 17.5_f32,  9.5_f32,  3.0_f32); // right wing
    let wing_a                 = 20.0_f32.to_radians();
    let (tcx, tcy, trx, try_) = (22.0_f32, 33.0_f32,  4.5_f32,  4.0_f32); // tail
    let (kcx, kcy, kr)        = (22.0_f32, 33.0_f32,  2.0_f32);            // bright core

    for y in 0..size {
        for x in 0..size {
            let mut r_acc = 0.0_f32;
            let mut g_acc = 0.0_f32;
            let mut b_acc = 0.0_f32;
            let mut a_acc = 0.0_f32;

            for sy in 0..ss {
                for sx in 0..ss {
                    let fx = x as f32 + (sx as f32 + 0.5) / ss as f32;
                    let fy = y as f32 + (sy as f32 + 0.5) / ss as f32;

                    let (sr, sg, sb, sa): (u8, u8, u8, f32) =
                        if in_circle(fx, fy, kcx, kcy, kr) {
                            // bright core: 30% status + 70% white
                            let br = (col.0 as f32 * 0.3 + 255.0 * 0.7) as u8;
                            let bg = (col.1 as f32 * 0.3 + 255.0 * 0.7) as u8;
                            let bb = (col.2 as f32 * 0.3 + 255.0 * 0.7) as u8;
                            (br, bg, bb, 1.0)
                        } else if in_circle(fx, fy, hcx, hcy, hr)
                               || in_ellipse(fx, fy, bcx, bcy, brx, bry, 0.0)
                               || in_ellipse(fx, fy, tcx, tcy, trx, try_, 0.0)
                        {
                            (col.0, col.1, col.2, 1.0)
                        } else if in_ellipse(fx, fy, lcx, lcy, lwx, lwy,  wing_a)
                               || in_ellipse(fx, fy, rcx, rcy, rwx, rwy, -wing_a)
                        {
                            (col.0, col.1, col.2, 0.55)
                        } else {
                            (0, 0, 0, 0.0)
                        };

                    r_acc += sr as f32 * sa;
                    g_acc += sg as f32 * sa;
                    b_acc += sb as f32 * sa;
                    a_acc += sa;
                }
            }

            if a_acc > 0.0 {
                let idx = ((y * size + x) * 4) as usize;
                rgba[idx]     = (r_acc / a_acc) as u8;
                rgba[idx + 1] = (g_acc / a_acc) as u8;
                rgba[idx + 2] = (b_acc / a_acc) as u8;
                rgba[idx + 3] = ((a_acc / (ss * ss) as f32) * 255.0).round() as u8;
            }
        }
    }

    tauri::image::Image::new_owned(rgba, size, size)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        // positioner: lets us place the panel relative to the tray icon
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_notification::init())
        .manage(SnapshotCache::default())
        // Window lifecycle:
        //  - panel: hide on focus-loss (click-elsewhere dismisses the popover)
        //  - main:  intercept close-requested → hide instead of destroy, so
        //           "Open Lumen" can always re-show it via get_webview_window.
        .on_window_event(|window, event| {
            match event {
                WindowEvent::Focused(false) => {
                    if window.label() == "panel" {
                        let _ = window.hide();
                    }
                }
                WindowEvent::CloseRequested { api, .. } => {
                    if window.label() == "main" {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
                _ => {}
            }
        })
        .setup(|app| {
            // macOS: run as a menu-bar accessory (no Dock icon, tray shows reliably)
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            // resolve a stable DB path in the app-data dir
            let db_path = app
                .path()
                .app_data_dir()
                .map(|d| {
                    std::fs::create_dir_all(&d).ok();
                    d.join("lumen.db")
                })
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "lumen.db".to_string());

            unsafe {
                std::env::set_var("LUMEN_DB", &db_path);
            }

            // Migrate DB from old com.tauri.dev identifier if this is a new install path.
            // This handles the one-time transition when the bundle identifier changed.
            if !std::path::Path::new(&db_path).exists() {
                if let Some(home) = dirs::home_dir() {
                    let old_db = home.join("Library/Application Support/com.tauri.dev/lumen.db");
                    if old_db.exists() {
                        let _ = std::fs::copy(&old_db, &db_path);
                        log::info!("Migrated DB from com.tauri.dev to io.speedata.lumen");
                    }
                }
            }

            // Write pointer file so lumen-mcp can auto-discover the same DB path.
            if let Ok(home) = std::env::var("HOME") {
                let _ = std::fs::write(
                    std::path::Path::new(&home).join(".lumen_db_path"),
                    &db_path,
                );
            }

            // spawn the bundled daemon as a sidecar, passing the DB path via env
            let sidecar = app
                .shell()
                .sidecar("lumen-daemon")
                .expect("sidecar lumen-daemon not found")
                .env("LUMEN_DB", &db_path);
            let (mut rx, _child) = sidecar.spawn().expect("failed to spawn daemon");

            tauri::async_runtime::spawn(async move {
                while let Some(event) = rx.recv().await {
                    if let CommandEvent::Stderr(line) = event {
                        log::info!("[daemon] {}", String::from_utf8_lossy(&line));
                    }
                }
            });

            // --- tray icon ---
            let quit = MenuItem::with_id(app, "quit", "Quit Lumen", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "Open Lumen", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            let tray_result = TrayIconBuilder::with_id("lumen-tray")
                // colored battery-ring at 0%; recolored/redrawn live by update_tray
                .icon(render_tray_icon(0, "ok"))
                .icon_as_template(false)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.unminimize();
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // record the tray geometry for the positioner (needed for
                    // TrayBottomCenter to resolve a screen position)
                    tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);

                    // left-click toggles the panel popover
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(panel) = app.get_webview_window("panel") {
                            if panel.is_visible().unwrap_or(false) {
                                let _ = panel.hide();
                            } else {
                                let _ = panel.move_window(Position::TrayBottomCenter);
                                let _ = panel.show();
                                let _ = panel.set_focus();
                            }
                        }
                    }
                })
                .build(app);

            match tray_result {
                Ok(_) => log::info!("TRAY: built successfully"),
                Err(e) => log::error!("TRAY: build failed: {e}"),
            }

            // connect to the daemon WS and forward to the frontend
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(connect_daemon(handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_stats,
            update_tray,
            request_snapshot,
            get_usage,
            get_sessions,
            get_optimizer_stats,
            setup::lumen_setup_needed,
            setup::lumen_run_setup,
            setup::lumen_uninstall
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Return the cached daemon snapshot JSON (or null if not received yet).
#[tauri::command]
fn request_snapshot(cache: State<SnapshotCache>) -> Option<String> {
    cache.0.lock().unwrap().clone()
}

#[derive(Serialize)]
struct Stats {
    turns: i64,
    output_total: i64,
    factor: f64,
}

#[tauri::command]
async fn get_stats() -> Result<Stats, String> {
    let pool = SqlitePoolOptions::new()
        .connect(&{
            let p = std::env::var("LUMEN_DB").unwrap_or_else(|_| "../../lumen.db".to_string());
            format!("sqlite:{p}?mode=rwc")
        })
        .await
        .map_err(|e| e.to_string())?;

    let (turns, output_total): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(output_tokens),0) FROM turns")
            .fetch_one(&pool)
            .await
            .map_err(|e| e.to_string())?;

    let factor: (Option<f64>,) = sqlx::query_as("SELECT factor FROM correction_factor")
        .fetch_one(&pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(Stats {
        turns,
        output_total,
        factor: factor.0.unwrap_or(1.0),
    })
}

#[tauri::command]
fn update_tray(app: tauri::AppHandle, percent: u8, status: String) {
    if let Some(tray) = app.tray_by_id(&TrayIconId::new("lumen-tray")) {
        // redraw the firefly icon; clear the text title.
        let _ = tray.set_icon(Some(render_tray_icon(percent, &status)));
        let _ = tray.set_icon_as_template(false);
        let _ = tray.set_title(None::<&str>);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Usage & Cost aggregates (read-only, on demand). This is SEPARATE from the
// live turn stream / snapshot — it just runs grouped SUMs over `turns`.
//
// HONESTY: these are CONSUMPTION figures. Plan quota size / remaining / the
// real reset are server-side and unknown locally, so nothing here is a
// "% of limit" or "remaining". Dollar costs are NOT computed here — the
// frontend applies the single RATE table to these token sums (one price
// source of truth). `cache_read` over all-time feeds the reported
// "Saved by caching" value = cache_read * (RATE.input − RATE.cacheRead).
// ─────────────────────────────────────────────────────────────────────────

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct TokenAgg {
    turns: i64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    total_tokens: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageReport {
    rolling_5h: TokenAgg,
    /// earliest turn inside the trailing 5h window (ISO-8601 UTC), or null
    window_start: Option<String>,
    /// PROXY reset = window_start + 5h. NOT the real server reset — "approx".
    reset_approx: Option<String>,
    rolling_7d_opus: TokenAgg,
    rolling_7d_other: TokenAgg,
    today: TokenAgg,
    this_week: TokenAgg,
    all_time: TokenAgg,
}

fn db_url() -> String {
    let p = std::env::var("LUMEN_DB").unwrap_or_else(|_| "../../lumen.db".to_string());
    format!("sqlite:{p}?mode=rwc")
}

/// Run the standard token-aggregate SELECT with a caller-supplied WHERE clause
/// (pass "" for all-time). The clause is a fixed string literal at every call
/// site below — no user input is interpolated.
async fn fetch_agg(pool: &sqlx::SqlitePool, where_clause: &str) -> Result<TokenAgg, String> {
    let sql = format!(
        "SELECT COUNT(*),
                COALESCE(SUM(input_tokens),0),
                COALESCE(SUM(output_tokens),0),
                COALESCE(SUM(cache_read_input_tokens),0),
                COALESCE(SUM(cache_creation_input_tokens),0),
                COALESCE(SUM(input_tokens + output_tokens
                           + cache_read_input_tokens + cache_creation_input_tokens),0)
         FROM turns {where_clause}"
    );
    let t: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(&sql)
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(TokenAgg {
        turns: t.0,
        input: t.1,
        output: t.2,
        cache_read: t.3,
        cache_write: t.4,
        total_tokens: t.5,
    })
}

#[tauri::command]
async fn get_usage() -> Result<UsageReport, String> {
    let pool = SqlitePoolOptions::new()
        .connect(&db_url())
        .await
        .map_err(|e| e.to_string())?;

    // (a) Rolling 5h consumption. `ts` is ISO-8601 UTC ('…Z'); datetime()
    //     normalizes both sides to canonical UTC so the comparison is correct.
    //     reset_approx is a PROXY (window_start + 5h), not the server reset.
    let r5: (i64, i64, i64, i64, i64, i64, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT COUNT(*),
                COALESCE(SUM(input_tokens),0),
                COALESCE(SUM(output_tokens),0),
                COALESCE(SUM(cache_read_input_tokens),0),
                COALESCE(SUM(cache_creation_input_tokens),0),
                COALESCE(SUM(input_tokens + output_tokens
                           + cache_read_input_tokens + cache_creation_input_tokens),0),
                MIN(ts),
                datetime(MIN(ts), '+5 hours')
         FROM turns
         WHERE datetime(ts) >= datetime('now','-5 hours')",
    )
    .fetch_one(&pool)
    .await
    .map_err(|e| e.to_string())?;

    // (b) Rolling 7d consumption, split Opus vs other.
    let rows: Vec<(String, i64, i64, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT CASE WHEN model LIKE '%opus%' THEN 'opus' ELSE 'other' END AS model_class,
                COUNT(*),
                COALESCE(SUM(input_tokens),0),
                COALESCE(SUM(output_tokens),0),
                COALESCE(SUM(cache_read_input_tokens),0),
                COALESCE(SUM(cache_creation_input_tokens),0),
                COALESCE(SUM(input_tokens + output_tokens
                           + cache_read_input_tokens + cache_creation_input_tokens),0)
         FROM turns
         WHERE datetime(ts) >= datetime('now','-7 days')
         GROUP BY model_class",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| e.to_string())?;

    let mut opus = TokenAgg::default();
    let mut other = TokenAgg::default();
    for (class, turns, input, output, cache_read, cache_write, total_tokens) in rows {
        let agg = TokenAgg { turns, input, output, cache_read, cache_write, total_tokens };
        if class == "opus" {
            opus = agg;
        } else {
            other = agg;
        }
    }

    // (c) Calendar rollups in LOCAL time. Week starts MONDAY (ISO-8601):
    //     strftime('%w') is 0=Sun..6=Sat, so (%w + 6) % 7 = days since Monday.
    let today = fetch_agg(&pool, "WHERE date(ts,'localtime') = date('now','localtime')").await?;
    let this_week = fetch_agg(
        &pool,
        "WHERE date(ts,'localtime') >= \
         date('now','localtime','-'||((strftime('%w','now','localtime')+6)%7)||' days')",
    )
    .await?;
    let all_time = fetch_agg(&pool, "").await?;

    Ok(UsageReport {
        rolling_5h: TokenAgg {
            turns: r5.0,
            input: r5.1,
            output: r5.2,
            cache_read: r5.3,
            cache_write: r5.4,
            total_tokens: r5.5,
        },
        window_start: r5.6,
        reset_approx: r5.7,
        rolling_7d_opus: opus,
        rolling_7d_other: other,
        today,
        this_week,
        all_time,
    })
}

// ─────────────────────────────────────────────────────────────────────────
// Session history (read-only, on demand). One summary row per session_id,
// newest activity first. Reads straight from the DB, so it is independent of
// the frontend's in-memory live-session cap and can list far more sessions.
//
// Dollar cost is NOT computed here — the frontend multiplies these token sums
// by the single RATE table. `peak_cache_read` (MAX cache_read in the session)
// is the peak-context-fill proxy, mirroring the live gauge's `fill`.
// ─────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionSummary {
    session_id: String,
    model: Option<String>,
    first_ts: String,
    last_ts: String,
    turn_count: i64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    total_tokens: i64,
    peak_cache_read: i64,
}

#[tauri::command]
async fn get_sessions() -> Result<Vec<SessionSummary>, String> {
    let pool = SqlitePoolOptions::new()
        .connect(&db_url())
        .await
        .map_err(|e| e.to_string())?;

    // One row per session. `model` = the most recent non-null model seen in the
    // session (sessions are normally single-model). Newest activity first.
    let rows: Vec<(
        String,
        Option<String>,
        String,
        String,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    )> = sqlx::query_as(
        "SELECT
            session_id,
            (SELECT t2.model FROM turns t2
                WHERE t2.session_id = t.session_id AND t2.model IS NOT NULL
                ORDER BY t2.ts DESC LIMIT 1)                                       AS model,
            MIN(ts)                                                                AS first_ts,
            MAX(ts)                                                                AS last_ts,
            COUNT(*)                                                               AS turn_count,
            COALESCE(SUM(input_tokens),0),
            COALESCE(SUM(output_tokens),0),
            COALESCE(SUM(cache_read_input_tokens),0),
            COALESCE(SUM(cache_creation_input_tokens),0),
            COALESCE(SUM(input_tokens + output_tokens
                       + cache_read_input_tokens + cache_creation_input_tokens),0) AS total_tokens,
            COALESCE(MAX(cache_read_input_tokens),0)                               AS peak_cache_read
         FROM turns t
         GROUP BY session_id
         ORDER BY MAX(ts) DESC
         LIMIT 100",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| SessionSummary {
            session_id: r.0,
            model: r.1,
            first_ts: r.2,
            last_ts: r.3,
            turn_count: r.4,
            input: r.5,
            output: r.6,
            cache_read: r.7,
            cache_write: r.8,
            total_tokens: r.9,
            peak_cache_read: r.10,
        })
        .collect())
}

// ─────────────────────────────────────────────────────────────────────────
// E5 — Optimizer savings (CAUSED by Lumen). Distinct from "caching saved"
// (REPORTED by Claude Code and stored in the turns table's cache_read column).
//
// routed_via IN ('smart_read','recall_file','compress_logs') = Lumen tool
// calls that returned fewer tokens than the full file.  saved_tokens is exact
// BPE measured per call.  Dollar conversion uses RATE.input (these are input
// token reads); the frontend owns RATE and applies it so there is one price
// source of truth.
//
// builtin_read rows have saved_tokens=0 and represent CLI-only "missed
// optimizations" — they are surfaced honestly but NEVER counted as savings.
// ─────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChannelBreakdown {
    channel: String,
    calls: i64,
    saved_tokens: i64,
    full_tokens: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolBreakdown {
    tool: String,
    calls: i64,
    saved_tokens: i64,
    full_tokens: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OptimizerReport {
    /// SUM(saved_tokens) over lumen routes — CAUSED by Lumen, not reported.
    /// Convert to USD in the frontend: lifetimeOptimizedTokens * RATE.input.
    lifetime_optimized_tokens: i64,
    /// SUM(full_tokens) over lumen routes — denominator for effectivenessPct.
    lifetime_full_tokens: i64,
    /// Calendar rollups (local time, same method as get_usage).
    today_saved_tokens: i64,
    this_week_saved_tokens: i64,
    /// Per-channel breakdown (cli | vscode | unknown).
    by_channel: Vec<ChannelBreakdown>,
    /// Per-tool breakdown (smart_read | recall_file | compress_logs).
    by_tool: Vec<ToolBreakdown>,
    /// Channel of the most recent read_events row — proxy for active context.
    current_channel: String,
    /// CLI-only: reads that bypassed Lumen (builtin_read, channel=cli).
    /// Label as "not optimized (read in full)". Never count as savings.
    missed_calls: i64,
    missed_full_tokens: i64,
}

#[tauri::command]
async fn get_optimizer_stats() -> Result<OptimizerReport, String> {
    let pool = SqlitePoolOptions::new()
        .connect(&db_url())
        .await
        .map_err(|e| e.to_string())?;

    // ── Lifetime totals ───────────────────────────────────────────────────────
    let (lifetime_optimized_tokens, lifetime_full_tokens): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(saved_tokens),0), COALESCE(SUM(full_tokens),0)
         FROM read_events
         WHERE routed_via IN ('smart_read','recall_file','compress_logs')",
    )
    .fetch_one(&pool)
    .await
    .map_err(|e| e.to_string())?;

    // ── Calendar rollups (local time — same method as get_usage) ─────────────
    let (today_saved_tokens,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(saved_tokens),0)
         FROM read_events
         WHERE routed_via IN ('smart_read','recall_file','compress_logs')
           AND date(ts,'localtime') = date('now','localtime')",
    )
    .fetch_one(&pool)
    .await
    .map_err(|e| e.to_string())?;

    // Week starts Monday (ISO-8601): (strftime('%w')+6)%7 = days since Monday.
    let (this_week_saved_tokens,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(saved_tokens),0)
         FROM read_events
         WHERE routed_via IN ('smart_read','recall_file','compress_logs')
           AND date(ts,'localtime') >= date('now','localtime',
               '-'||((strftime('%w','now','localtime')+6)%7)||' days')",
    )
    .fetch_one(&pool)
    .await
    .map_err(|e| e.to_string())?;

    // ── Per-channel breakdown ─────────────────────────────────────────────────
    let channel_rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT channel, COUNT(*),
                COALESCE(SUM(saved_tokens),0), COALESCE(SUM(full_tokens),0)
         FROM read_events
         WHERE routed_via IN ('smart_read','recall_file','compress_logs')
         GROUP BY channel
         ORDER BY SUM(saved_tokens) DESC",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| e.to_string())?;

    let by_channel = channel_rows
        .into_iter()
        .map(|(channel, calls, saved_tokens, full_tokens)| ChannelBreakdown {
            channel,
            calls,
            saved_tokens,
            full_tokens,
        })
        .collect();

    // ── Per-tool breakdown ────────────────────────────────────────────────────
    let tool_rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT routed_via, COUNT(*),
                COALESCE(SUM(saved_tokens),0), COALESCE(SUM(full_tokens),0)
         FROM read_events
         WHERE routed_via IN ('smart_read','recall_file','compress_logs')
         GROUP BY routed_via
         ORDER BY SUM(saved_tokens) DESC",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| e.to_string())?;

    let by_tool = tool_rows
        .into_iter()
        .map(|(tool, calls, saved_tokens, full_tokens)| ToolBreakdown {
            tool,
            calls,
            saved_tokens,
            full_tokens,
        })
        .collect();

    // ── Current channel (most recent event) ──────────────────────────────────
    let current_channel: (Option<String>,) = sqlx::query_as(
        "SELECT channel FROM read_events ORDER BY ts DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap_or((None,));

    // ── CLI missed reads ──────────────────────────────────────────────────────
    // builtin_read rows written by the CLI PostToolUse hook when the model used
    // the built-in Read instead of lumen tools.  saved_tokens=0 always.
    let (missed_calls, missed_full_tokens): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(full_tokens),0)
         FROM read_events
         WHERE routed_via = 'builtin_read' AND channel = 'cli'",
    )
    .fetch_one(&pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(OptimizerReport {
        lifetime_optimized_tokens,
        lifetime_full_tokens,
        today_saved_tokens,
        this_week_saved_tokens,
        by_channel,
        by_tool,
        current_channel: current_channel.0.unwrap_or_else(|| "unknown".to_string()),
        missed_calls,
        missed_full_tokens,
    })
}
