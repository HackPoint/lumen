#![allow(clippy::single_match)] // match with one arm used intentionally for clarity
#![allow(clippy::collapsible_match)] // guard conditions in match arms kept explicit for readability
#![allow(clippy::type_complexity)] // complex sqlx query types are self-documenting inline
mod setup;
use futures_util::StreamExt;
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
// Shape: concept P — solid core + 2 concentric rings, status-colored.
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

fn in_annulus(fx: f32, fy: f32, cx: f32, cy: f32, r_inner: f32, r_outer: f32) -> bool {
    let d2 = (fx - cx) * (fx - cx) + (fy - cy) * (fy - cy);
    d2 >= r_inner * r_inner && d2 <= r_outer * r_outer
}

/// Concept P: solid core dot + 2 concentric rings, all status-colored.
/// Redrawn on status/percent change only; no continuous animation.
fn render_tray_icon(_percent: u8, status: &str) -> tauri::image::Image<'static> {
    let size = ICON_SIZE;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let col = status_rgb(status);
    let ss = 3u32; // 3×3 supersampling for smooth edges

    let cx = 22.0_f32;
    let cy = 22.0_f32;

    // Geometry — all in buffer pixels, 44×44 canvas (2× retina for 22pt slot).
    // Core: filled solid dot at center.
    // Ring 1 / Ring 2: concentric annuli radiating outward, fading in opacity.
    let core_r = 3.5_f32;
    let ring1_in = 7.5_f32;
    let ring1_out = 10.5_f32;
    let ring2_in = 14.0_f32;
    let ring2_out = 17.0_f32;

    // Core is brightened (50% white blend) so it reads on both light + dark menu bars.
    let core_rv = (col.0 as f32 * 0.5 + 255.0 * 0.5) as u8;
    let core_gv = (col.1 as f32 * 0.5 + 255.0 * 0.5) as u8;
    let core_bv = (col.2 as f32 * 0.5 + 255.0 * 0.5) as u8;

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

                    let (sr, sg, sb, sa): (u8, u8, u8, f32) = if in_circle(fx, fy, cx, cy, core_r) {
                        // Bright core: 50% white blend for legibility
                        (core_rv, core_gv, core_bv, 1.0)
                    } else if in_annulus(fx, fy, cx, cy, ring1_in, ring1_out) {
                        // Inner ring: full status color, near-opaque
                        (col.0, col.1, col.2, 0.85)
                    } else if in_annulus(fx, fy, cx, cy, ring2_in, ring2_out) {
                        // Outer ring: fades outward — signals radiance without dominating
                        (col.0, col.1, col.2, 0.50)
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
                rgba[idx] = (r_acc / a_acc) as u8;
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
        .on_window_event(|window, event| match event {
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

            // One-time migration from the old com.tauri.dev bundle identifier.
            // macOS-only: that identifier never shipped on any other platform, and
            // the path below is a macOS layout, so elsewhere this could only ever
            // look for a file that cannot exist.
            #[cfg(target_os = "macos")]
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
            // dirs::home_dir() rather than $HOME: Windows sets USERPROFILE, not
            // HOME, so reading the env var directly skipped this entirely there.
            if let Some(home) = dirs::home_dir() {
                let _ = std::fs::write(home.join(".lumen_db_path"), &db_path);
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
            setup::lumen_uninstall,
            setup::lumen_install_cli
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Return the cached daemon snapshot JSON (or null if not received yet).
#[tauri::command]
fn request_snapshot(cache: State<SnapshotCache>) -> Option<String> {
    cache.0.lock().unwrap().clone()
}

/// Basic turn/output totals plus the calibration factor.
/// All the SQL lives in `lumen-stats` so it can be tested without a Tauri build.
#[tauri::command]
async fn get_stats() -> Result<lumen_stats::Stats, String> {
    let pool = lumen_stats::connect_default().await?;
    lumen_stats::get_stats(&pool).await
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

// ──────────────────────────────────────────────────────────────────────────
// Read-only aggregates (usage & cost, session history, optimizer savings).
//
// The queries, the wire structs and the honesty caveats all live in the
// `lumen-stats` crate. They were moved out of this file so they can be unit
// tested on every platform: this crate cannot build without bundled sidecar
// binaries, which CI does not have.
//
// These commands are deliberately nothing but "open a pool, delegate".
// ──────────────────────────────────────────────────────────────────────────

#[tauri::command]
async fn get_usage() -> Result<lumen_stats::UsageReport, String> {
    let pool = lumen_stats::connect_default().await?;
    lumen_stats::get_usage(&pool).await
}

#[tauri::command]
async fn get_sessions() -> Result<Vec<lumen_stats::SessionSummary>, String> {
    let pool = lumen_stats::connect_default().await?;
    lumen_stats::get_sessions(&pool).await
}

#[tauri::command]
async fn get_optimizer_stats() -> Result<lumen_stats::OptimizerReport, String> {
    let pool = lumen_stats::connect_default().await?;
    lumen_stats::get_optimizer_stats(&pool).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RGBA of one pixel in the tray buffer.
    fn px(rgba: &[u8], x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = ((y * ICON_SIZE + x) * 4) as usize;
        (rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3])
    }

    // ── status colours ───────────────────────────────────────────────────────

    #[test]
    fn status_maps_to_the_documented_traffic_light() {
        assert_eq!(status_rgb("alert"), (248, 81, 73), "red");
        assert_eq!(status_rgb("warn"), (210, 153, 34), "amber");
        assert_eq!(status_rgb("ok"), (63, 185, 80), "green");
    }

    #[test]
    fn an_unknown_status_falls_back_to_green() {
        // The frontend only ever sends ok/warn/alert; anything else must render
        // as healthy rather than panic or go transparent.
        assert_eq!(status_rgb(""), (63, 185, 80));
        assert_eq!(status_rgb("garbage"), (63, 185, 80));
    }

    // ── geometry predicates ──────────────────────────────────────────────────

    #[test]
    fn in_circle_is_inclusive_of_its_boundary() {
        assert!(in_circle(0.0, 0.0, 0.0, 0.0, 1.0), "centre is inside");
        assert!(in_circle(1.0, 0.0, 0.0, 0.0, 1.0), "exactly on the edge");
        assert!(!in_circle(1.001, 0.0, 0.0, 0.0, 1.0), "just outside");
    }

    #[test]
    fn in_annulus_excludes_the_hole_and_the_outside() {
        // Ring from r=2 to r=4 around the origin.
        assert!(!in_annulus(1.0, 0.0, 0.0, 0.0, 2.0, 4.0), "inside the hole");
        assert!(
            in_annulus(2.0, 0.0, 0.0, 0.0, 2.0, 4.0),
            "on the inner edge"
        );
        assert!(in_annulus(3.0, 0.0, 0.0, 0.0, 2.0, 4.0), "in the band");
        assert!(
            in_annulus(4.0, 0.0, 0.0, 0.0, 2.0, 4.0),
            "on the outer edge"
        );
        assert!(!in_annulus(4.001, 0.0, 0.0, 0.0, 2.0, 4.0), "beyond it");
    }

    #[test]
    fn in_annulus_is_symmetric_about_the_centre() {
        for (dx, dy) in [(3.0, 0.0), (-3.0, 0.0), (0.0, 3.0), (0.0, -3.0)] {
            assert!(
                in_annulus(50.0 + dx, 50.0 + dy, 50.0, 50.0, 2.0, 4.0),
                "offset ({dx},{dy}) must be in the ring"
            );
        }
    }

    // ── rendered icon ────────────────────────────────────────────────────────

    #[test]
    fn the_icon_is_a_square_retina_buffer() {
        let img = render_tray_icon(50, "ok");
        assert_eq!(img.width(), ICON_SIZE);
        assert_eq!(img.height(), ICON_SIZE);
        assert_eq!(
            img.rgba().len(),
            (ICON_SIZE * ICON_SIZE * 4) as usize,
            "4 bytes per pixel"
        );
    }

    #[test]
    fn the_core_is_opaque_and_the_far_corner_is_transparent() {
        let img = render_tray_icon(50, "ok");
        let rgba = img.rgba();
        let (_, _, _, centre_a) = px(rgba, 22, 22);
        let (_, _, _, corner_a) = px(rgba, 0, 0);
        assert_eq!(centre_a, 255, "the core dot must be fully opaque");
        assert_eq!(corner_a, 0, "the corner is outside every shape");
    }

    #[test]
    fn the_core_is_brightened_so_it_reads_on_both_menu_bars() {
        // The core is a 50% white blend of the status colour, so every channel
        // must be lighter than the raw colour.
        let raw = status_rgb("ok");
        let img = render_tray_icon(50, "ok");
        let (r, g, b, _) = px(img.rgba(), 22, 22);
        assert!(
            r > raw.0 && g > raw.1 && b > raw.2,
            "got ({r},{g},{b}) vs {raw:?}"
        );
    }

    #[test]
    fn the_rings_are_translucent_rather_than_solid() {
        let img = render_tray_icon(50, "ok");
        let rgba = img.rgba();
        // Ring 1 spans r=7.5..10.5 from centre (22,22): x=31 is r=9.
        let (_, _, _, ring1_a) = px(rgba, 31, 22);
        // Ring 2 spans r=14..17: x=38 is r=16.
        let (_, _, _, ring2_a) = px(rgba, 38, 22);
        assert!(ring1_a > 0 && ring1_a < 255, "inner ring alpha {ring1_a}");
        assert!(ring2_a > 0 && ring2_a < 255, "outer ring alpha {ring2_a}");
        assert!(
            ring2_a < ring1_a,
            "the outer ring must fade: {ring2_a} should be under {ring1_a}"
        );
    }

    #[test]
    fn the_gap_between_the_rings_is_transparent() {
        let img = render_tray_icon(50, "ok");
        // r=12 sits between ring1 (ends 10.5) and ring2 (starts 14).
        let (_, _, _, a) = px(img.rgba(), 34, 22);
        assert_eq!(a, 0, "the gap must be see-through");
    }

    #[test]
    fn each_status_paints_a_different_icon() {
        let ok = render_tray_icon(50, "ok").rgba().to_vec();
        let warn = render_tray_icon(50, "warn").rgba().to_vec();
        let alert = render_tray_icon(50, "alert").rgba().to_vec();
        assert_ne!(ok, warn);
        assert_ne!(warn, alert);
        assert_ne!(ok, alert);
    }

    #[test]
    fn the_percent_argument_does_not_change_the_icon() {
        // Concept P is status-coloured only — the ring geometry is fixed and the
        // percentage is conveyed by colour, not by fill. Pinned so a future
        // percent-driven design is a deliberate change, not an accident.
        let a = render_tray_icon(0, "ok").rgba().to_vec();
        let b = render_tray_icon(99, "ok").rgba().to_vec();
        assert_eq!(a, b);
    }

    #[test]
    fn every_percent_renders_without_panicking() {
        for p in [0u8, 1, 50, 80, 95, 100, 255] {
            for status in ["ok", "warn", "alert"] {
                let img = render_tray_icon(p, status);
                assert_eq!(img.rgba().len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
            }
        }
    }

    // ── db_url ───────────────────────────────────────────────────────────────

    #[test]
    fn db_url_is_a_sqlite_url_in_read_write_create_mode() {
        // rwc so a first run against a missing file creates it instead of erroring.
        let url = lumen_stats::db_url();
        assert!(url.starts_with("sqlite:"), "got {url}");
        assert!(url.ends_with("?mode=rwc"), "got {url}");
    }
}
