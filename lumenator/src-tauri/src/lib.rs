#![allow(clippy::single_match)] // match with one arm used intentionally for clarity
#![allow(clippy::collapsible_match)] // guard conditions in match arms kept explicit for readability
#![allow(clippy::type_complexity)] // complex sqlx query types are self-documenting inline
mod health;
mod setup;
use futures_util::StreamExt;
use std::sync::Mutex;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent, TrayIconId};
use crate::health::StartupHealth;
use tauri::{Emitter, Manager, State, WindowEvent};
use tauri_plugin_positioner::{Position, WindowExt};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// Holds the most recent snapshot JSON received from the daemon, so the
/// frontend can fetch it on demand (avoids the connect-before-listen race).
#[derive(Default)]
struct SnapshotCache(Mutex<Option<String>>);

/// The spawned daemon, kept so it can be killed when the app exits.
///
/// This used to be bound to `_child` and dropped immediately. Dropping a
/// `CommandChild` does not terminate the process, so every quit left a daemon
/// running, reparented to launchd and still holding 127.0.0.1:9999 — after an
/// upgrade the new app's daemon could not bind and the GUI silently kept reading
/// from the previous build's daemon.
struct DaemonChild(Mutex<Option<CommandChild>>);

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
        // Launch at login. LaunchAgent rather than the AppleScript login-item
        // route on macOS: it does not require the app to live in /Applications,
        // so it keeps working for a build run from anywhere. No extra argv — the
        // app cannot tell a login launch from a manual one, and does not need to,
        // because both windows start hidden and the tray is the whole interface.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
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
                    // Hiding is right only while there is a tray to re-open from. With the
                    // icon unavailable, hiding this window puts the app back exactly where
                    // issue #5 found it — running, with nothing to click. Quit instead.
                    let reachable = window
                        .app_handle()
                        .try_state::<StartupHealth>()
                        .map(|h| !health::needs_fallback(&h.tray()))
                        .unwrap_or(true);
                    if reachable {
                        api.prevent_close();
                        let _ = window.hide();
                    } else {
                        log::warn!("CLOSE: no reachable tray — quitting rather than hiding");
                        window.app_handle().exit(0);
                    }
                }
            }
            _ => {}
        })
        .manage(StartupHealth::default())
        .setup(|app| {
            // Nothing below this line uses `?` or `expect`.
            //
            // Every step either succeeds or records a degradation, and setup ends with one
            // decision about whether the app is reachable. Before this rule, three `?` on
            // menu construction and two `expect`s on the daemon sidecar could abort startup
            // outright, and the tray fallback added in 1.5.1 covered none of them — so a
            // failure in any of those five places produced a process with no interface at
            // all, which is the shape of issue #5.

            // macOS: run as a menu-bar accessory (no Dock icon, tray shows reliably).
            // Deliberately reversible — `reveal_main_window` switches back to Regular, because
            // an Accessory process has no Dock icon and is absent from the app switcher, so a
            // window it "shows" has nothing to bring it forward.
            //
            // Ordered before the `state()` borrow below: this one needs `&mut App`.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let health = app.state::<StartupHealth>();

            // Registered unconditionally. This used to be `if cfg!(debug_assertions)`, and
            // since no other logger exists in this crate, every log::error! in a released
            // build went to a no-op sink — so ~/Library/Logs/io.speedata.lumen/Lumen.log was
            // never created. Both the 1.5.1 CHANGELOG and a comment on issue #5 asked the
            // reporter for a line from that file. There was nothing to find.
            //
            // Warn by default in release, not off and not opt-in: a user who cannot reach the
            // app cannot be told to set an env var, and by the time they are asked the launch
            // that mattered is over. LUMEN_LOG raises it for a follow-up.
            let level = health::log_level_from(std::env::var("LUMEN_LOG").ok().as_deref(), cfg!(debug_assertions));
            if let Err(e) = app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(level)
                    // These are chatty at Debug and would bury the lines we actually want.
                    .level_for("sqlx", log::LevelFilter::Warn)
                    .level_for("tao", log::LevelFilter::Warn)
                    .level_for("wry", log::LevelFilter::Warn)
                    .level_for("tungstenite", log::LevelFilter::Warn)
                    .level_for("tokio_tungstenite", log::LevelFilter::Warn)
                    .level_for("hyper", log::LevelFilter::Warn)
                    .level_for("reqwest", log::LevelFilter::Warn)
                    // Stdout is kept in release on purpose: it is what makes "quit, then run
                    // the binary from Terminal" a working diagnostic. The no-println rule in
                    // lumen-daemon is about *that* process, whose pipes this one owns.
                    .targets([
                        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir { file_name: None }),
                    ])
                    // The plugin default is 40 KB, which a Debug session overruns in seconds.
                    .max_file_size(256 * 1024)
                    .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepOne)
                    .build(),
            ) {
                // No logger yet, so this is the one place stderr is the only option.
                eprintln!("LOG: could not register the log plugin: {e}");
            }
            log::warn!("STARTUP: Lumen {} (log level {level})", env!("CARGO_PKG_VERSION"));

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
            // LUMEN_SUPERVISED tells the daemon to exit when this process does. The
            // sidecar's stdin is a pipe held by the app, so the daemon sees EOF even
            // when the app is killed outright and no exit handler runs.
            //
            // Managed unconditionally, and before the spawn is attempted: the RunEvent::Exit
            // handler does `app.state::<DaemonChild>()`, which panics if nothing was ever
            // managed. Registering it only on the success path meant that degrading past a
            // failed spawn would trade a startup panic for a shutdown panic.
            app.manage(DaemonChild(Mutex::new(None)));

            match app.shell().sidecar("lumen-daemon") {
                Ok(cmd) => {
                    let cmd = cmd
                        .env("LUMEN_DB", &db_path)
                        .env("LUMEN_SUPERVISED", "1");
                    match cmd.spawn() {
                        Ok((mut rx, child)) => {
                            *app.state::<DaemonChild>().0.lock().unwrap() = Some(child);
                            tauri::async_runtime::spawn(async move {
                                while let Some(event) = rx.recv().await {
                                    if let CommandEvent::Stderr(line) = event {
                                        // warn!, not info!: release defaults to Warn, and the
                                        // daemon only writes here for genuinely notable
                                        // events, so this does not spam.
                                        log::warn!("[daemon] {}", String::from_utf8_lossy(&line));
                                    }
                                }
                            });
                        }
                        // Survivable: get_stats/get_usage/get_sessions read SQLite directly via
                        // lumen-stats, so the app still opens and still shows history. Only the
                        // live gauge is missing.
                        Err(e) => health.degrade("daemon", format!("could not spawn: {e}")),
                    }
                }
                Err(e) => health.degrade("daemon", format!("sidecar not found: {e}")),
            }

            // --- tray icon ---
            //
            // Before building: clear any persisted "this status item is hidden" preference, so
            // the item is created visible rather than created and immediately hidden.
            //
            // This is the likely cause of issue #5. ⌘-dragging a status item off the menu bar
            // makes macOS write `NSStatusItem Visible <autosave>` = false, permanently — and
            // from then on nothing fails: AppKit creates the item, hides it, build() returns
            // Ok, and the 1.5.1 fallback never fires. It is per-user state, which is why it
            // does not reproduce on the maintainer's machine.
            let restored = health::clear_hidden_status_item_prefs();

            let menu = match health::build_tray_menu_items(app) {
                Ok(m) => Some(m),
                // Was three `?`, which aborted Tauri startup entirely and left nothing running.
                // A tray with no menu is worse than no tray, so skip the tray and let the
                // reachability gate below reveal a window.
                Err(e) => {
                    health.degrade("tray-menu", format!("could not build the menu: {e}"));
                    None
                }
            };

            let simulate = health::simulated_tray_from(std::env::var("LUMEN_SIMULATE_TRAY").ok().as_deref());
            if simulate != health::SimulatedTray::None {
                log::warn!("TRAY: LUMEN_SIMULATE_TRAY is set — simulating {simulate:?}");
            }

            let tray_result = match (&menu, simulate) {
                (_, health::SimulatedTray::BuildError) => Err(tray_error("simulated tray build failure (LUMEN_SIMULATE_TRAY=err)")),
                (None, _) => Err(tray_error("no tray menu could be built")),
                (Some(menu), _) => TrayIconBuilder::with_id("lumen-tray")
                // colored battery-ring at 0%; recolored/redrawn live by update_tray
                .icon(render_tray_icon(0, "ok"))
                .icon_as_template(false)
                .menu(menu)
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
                    .build(app),
            };

            match tray_result {
                // "Built" is NOT "visible" — that conflation is why issue #5 produced a log
                // saying everything was fine. Left Unknown here on purpose and verified after
                // RunEvent::Ready, once the event loop has run and AppKit has laid the status
                // bar out; checking now would report a false absence on every launch.
                Ok(_) => log::warn!("TRAY: built; visibility not yet verified"),
                Err(e) => {
                    health.set_tray(health::TrayState::Failed(e.to_string()));
                    health.degrade("tray", format!("build failed: {e}"));
                }
            }

            // If a hidden-icon preference was cleared, say so once. The icon reappearing with
            // no explanation reads as the app fighting the user — and someone who ⌘-dragged it
            // away deliberately needs to be told that is not how to remove it, and what is.
            if !restored.is_empty() && health.claim_restore_explanation() {
                log::warn!("TRAY: restored a hidden menu-bar icon ({} pref(s))", restored.len());
                reveal_main_window(app.handle(), "the menu-bar icon was restored");
            }

            // Register the login item for installs that completed setup before
            // this feature existed. run_setup is skipped once its marker is
            // present, so without this an existing user would never get one.
            if setup::ensure_autostart_once_for(app.handle()) {
                log::info!("registered Lumen as a login item");
            }

            // Repair hook scripts that have drifted from this build. Scripts only:
            // they are Lumen's own files, so a bad write harms nothing else, whereas
            // ~/.claude.json holds every MCP server the user has and is reported
            // rather than rewritten. This is also what keeps the meter current — a
            // script from an older release silently records nothing for columns
            // added since, while every report still looks healthy.
            if let Some(what) = setup::ensure_scripts_fresh() {
                log::info!("hook scripts were stale: {what}");
            }

            // connect to the daemon WS and forward to the frontend
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(connect_daemon(handle));

            // One reachability gate, covering a failed menu, a failed build and (after the
            // check scheduled on Ready) an invisible icon — identically. Previously each case
            // was handled where it happened, or not at all.
            if health::needs_fallback(&health.tray()) {
                reveal_main_window(app.handle(), "the menu-bar icon is unavailable");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_stats,
            update_tray,
            request_snapshot,
            get_usage,
            get_sessions,
            get_optimizer_stats,
            get_context_report,
            get_fault_report,
            get_fault_count,
            check_for_update,
            file_fault_report,
            show_main_window,
            lumen_startup_health,
            resize_panel,
            setup::lumen_setup_needed,
            setup::lumen_run_setup,
            setup::lumen_uninstall,
            setup::lumen_install_cli,
            setup::lumen_autostart_enabled,
            setup::lumen_set_autostart,
            setup::lumen_artifact_health
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            // Kill the daemon on the way out. The daemon's own stdin watchdog covers
            // the case where this never runs (SIGKILL, force quit); this covers the
            // ordinary quit, and does it before the process image goes away so the
            // port is free by the time a replacement app launches.
            tauri::RunEvent::Exit => {
                if let Some(child) = app.state::<DaemonChild>().0.lock().unwrap().take() {
                    let _ = child.kill();
                }
            }

            // Verify the icon is actually on screen, now that the event loop has run.
            // Scheduled here rather than in setup() because AppKit lays the status bar out
            // asynchronously — a check inside setup() sees a rect that does not exist yet.
            tauri::RunEvent::Ready => {
                let handle = app.clone();
                tauri::async_runtime::spawn(async move { verify_tray_presence(handle).await });
            }

            // Double-clicking Lumen in /Applications, or `open -a Lumen`, arrives here.
            // Previously it did nothing at all: both windows start hidden, so the most natural
            // thing a user does when they cannot find the icon had no effect whatsoever. This
            // is the tray-independent way back in.
            tauri::RunEvent::Reopen { .. } => {
                log::warn!("REOPEN: the app was re-activated");
                reveal_main_window(app, "the app was re-opened");
            }
            _ => {}
        });
}

/// A `tauri::Error` carrying our own message.
///
/// Used so a degraded tray reports *why* rather than borrowing an unrelated variant —
/// `WebviewNotFound` in a log reads like a real defect somewhere else entirely.
fn tray_error(msg: &'static str) -> tauri::Error {
    tauri::Error::Anyhow(anyhow::anyhow!(msg))
}

/// Bring the main window forward, logging every branch.
///
/// Switching to `Regular` first is load-bearing on macOS. Startup sets
/// `ActivationPolicy::Accessory`, which leaves the process with no Dock icon and absent from
/// the app switcher — so `show()` + `set_focus()` can order a window in behind everything and
/// look exactly like doing nothing. And while the tray is unavailable the Dock icon is not a
/// cosmetic regression, it *is* the escape hatch, so the policy stays Regular until the tray
/// verifies Present.
///
/// The 1.5.1 version of this discarded both results with `let _ =` and fell through silently
/// when there was no window, so "we showed it and macOS refused" was indistinguishable from
/// "there was nothing to show".
fn reveal_main_window(app: &tauri::AppHandle, why: &str) {
    #[cfg(target_os = "macos")]
    if let Err(e) = app.set_activation_policy(tauri::ActivationPolicy::Regular) {
        log::error!("FALLBACK: could not switch to Regular activation: {e}");
    }
    let Some(w) = app.get_webview_window("main") else {
        log::error!("FALLBACK: no 'main' window exists — the app is unreachable ({why})");
        return;
    };
    if let Err(e) = w.unminimize() {
        log::warn!("FALLBACK: unminimize failed: {e}");
    }
    if let Err(e) = w.show() {
        log::error!("FALLBACK: show failed: {e}");
    }
    if let Err(e) = w.set_focus() {
        log::warn!("FALLBACK: set_focus failed: {e}");
    }
    log::warn!(
        "FALLBACK: revealed the main window ({why}); visible={:?}",
        w.is_visible()
    );
}

/// Check whether the status item is on screen, and repair it if it is not.
///
/// Three bounded attempts rather than a loop: AppKit's status-bar layout settles
/// asynchronously, so a single check right after `Ready` can be too early, but an unbounded
/// retry would spin forever on a genuinely full menu bar.
///
/// Note what is *not* retried: `TrayIconBuilder::build` itself. On macOS its only error paths
/// are `NotMainThread` and image conversion of a fixed 44×44 in-memory buffer — both
/// deterministic, so a second attempt cannot succeed. The thing worth re-checking is
/// visibility, which genuinely does change between attempts.
async fn verify_tray_presence(app: tauri::AppHandle) {
    // Nothing to verify if the tray was never built: the setup gate has already revealed a
    // window, and checking anyway reported "it was created but is not visible" about a tray that
    // was never created, then revealed the same window a second time.
    if matches!(
        app.state::<StartupHealth>().tray(),
        health::TrayState::Failed(_)
    ) {
        log::warn!("TRAY: skipping the presence check — the tray was never built");
        return;
    }

    const DELAYS_MS: [u64; 3] = [500, 1_500, 4_000];
    let simulate = health::simulated_tray_from(std::env::var("LUMEN_SIMULATE_TRAY").ok().as_deref());

    let mut last = health::TrayPresence::Unknown;
    for (attempt, delay) in DELAYS_MS.iter().enumerate() {
        tokio::time::sleep(std::time::Duration::from_millis(*delay)).await;

        last = match simulate {
            health::SimulatedTray::Absent => health::TrayPresence::Absent,
            health::SimulatedTray::OffScreen => health::TrayPresence::OffScreen,
            _ => tray_presence(&app),
        };
        log::warn!("TRAY: presence check {} of 3 → {last:?}", attempt + 1);

        if last == health::TrayPresence::Present {
            app.state::<StartupHealth>().set_tray(health::TrayState::Present);
            // Healthy: drop back to Accessory so there is no Dock icon, which is the intended
            // look for a menu-bar app.
            #[cfg(target_os = "macos")]
            if let Err(e) = app.set_activation_policy(tauri::ActivationPolicy::Accessory) {
                log::warn!("TRAY: could not return to Accessory activation: {e}");
            }
            return;
        }

        // Absent means the status item has no on-screen window. Ask AppKit directly to show
        // it — Tauri's own `set_visible(true)` cannot help here, because it only re-creates a
        // *missing* item and never calls `NSStatusItem::setVisible`.
        if last == health::TrayPresence::Absent && simulate == health::SimulatedTray::None {
            #[cfg(target_os = "macos")]
            if repair_tray_visibility(&app) {
                log::warn!("TRAY: asked AppKit to show the status item; re-checking");
                continue;
            }
        }
    }

    let health = app.state::<StartupHealth>();
    let why = match last {
        health::TrayPresence::OffScreen => {
            // Honest answer: there is nothing the app can do about a full menu bar.
            "the menu bar has no room for it (full, or clipped by the notch)".to_string()
        }
        health::TrayPresence::Absent => "it was created but is not visible".to_string(),
        other => format!("presence could not be determined ({other:?})"),
    };
    log::error!("TRAY: not visible after 3 checks — {why}");
    health.set_tray(health::TrayState::Absent(why.clone()));
    reveal_main_window(&app, &format!("the menu-bar icon is not visible: {why}"));
}

/// Ask the tray for its on-screen rect and classify it.
fn tray_presence(app: &tauri::AppHandle) -> health::TrayPresence {
    let Some(tray) = app.tray_by_id(&TrayIconId::new("lumen-tray")) else {
        return health::TrayPresence::Absent;
    };
    // Linux always returns None here, which is why the whole check is macOS-gated: treating
    // that as Absent would report every Linux launch as broken.
    if !cfg!(target_os = "macos") {
        return health::TrayPresence::Unknown;
    }
    let rect = match tray.rect() {
        Ok(Some(r)) => {
            let pos = r.position.to_physical::<f64>(1.0);
            let size = r.size.to_physical::<f64>(1.0);
            Some((pos.x, pos.y, size.width, size.height))
        }
        Ok(None) => None,
        Err(e) => {
            log::warn!("TRAY: rect() failed: {e}");
            return health::TrayPresence::Unknown;
        }
    };
    let monitors: Vec<health::MonitorBounds> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let p = m.position();
            let s = m.size();
            (p.x as f64, p.y as f64, s.width as f64, s.height as f64)
        })
        .collect();
    health::classify_presence(rect, &monitors)
}

/// Reach through Tauri to AppKit and set the status item visible. Returns whether the call
/// was made.
///
/// The closure must return a `Send` value, so it returns `bool` — `NSStatusItem` is
/// main-thread-only and not `Send`, and handing it back would not compile. `setVisible(true)`
/// also re-persists the preference, which is what makes the repair stick.
#[cfg(target_os = "macos")]
fn repair_tray_visibility(app: &tauri::AppHandle) -> bool {
    let Some(tray) = app.tray_by_id(&TrayIconId::new("lumen-tray")) else {
        return false;
    };
    tray.with_inner_tray_icon(|inner| {
        if let Some(item) = inner.ns_status_item() {
            unsafe { item.setVisible(true) };
            true
        } else {
            false
        }
    })
    .unwrap_or(false)
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
        return;
    }
    // This ran on every daemon snapshot and did nothing, invisibly. A tray that vanishes
    // *after* startup — the app was fine, then wasn't — left no trace at all. Logged once
    // (not every second) and recorded, so the banner and the fault report both learn about it.
    if let Some(health) = app.try_state::<StartupHealth>() {
        if health.claim_missing_tray_warning() {
            log::error!("TRAY: gone at update time — the gauge cannot be drawn");
            health.set_tray(health::TrayState::Absent(
                "disappeared after startup".to_string(),
            ));
            reveal_main_window(&app, "the menu-bar icon disappeared");
        }
    }
}

/// What degraded during startup, and whether the tray is actually visible.
///
/// Read by the frontend so a degraded app says so instead of looking healthy. Also folded into
/// the fault report, so a user who *can* reach the app files something that already contains
/// the answer.
#[tauri::command]
fn lumen_startup_health(app: tauri::AppHandle) -> serde_json::Value {
    let Some(health) = app.try_state::<StartupHealth>() else {
        return serde_json::json!({ "degraded": false, "tray": "unknown", "degradations": [] });
    };
    serde_json::json!({
        "degraded": health.is_degraded(),
        "tray": health.tray().describe(),
        "degradations": health.degradations(),
    })
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

/// Where this project's context has gone.
///
/// Diagnosis, not savings: it reads the ledger and returns nothing that claims a saving,
/// which is why it is the one figure in the product that cannot be net-negative.
#[tauri::command]
async fn get_context_report() -> Result<lumen_stats::ContextReport, String> {
    let pool = lumen_stats::connect_default().await?;
    lumen_stats::get_context_report(&pool).await
}

/// A rendered fault report, ready to show and then file.
///
/// The body is carried back to the frontend and handed to [`file_fault_report`] unchanged,
/// so what the user approved is byte-for-byte what gets filed. Re-rendering at file time
/// would let the two diverge — and the body is the thing being consented to.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FaultReport {
    pub body: String,
    pub title: String,
    pub fingerprint: String,
    /// Distinct `(kind, variant)` groups, for a badge that does not need the body parsed.
    pub kinds: usize,
    /// Total occurrences across every group.
    pub occurrences: u64,
    pub repo: String,
}

/// Render the current fault report, or `None` when there is nothing to report.
///
/// rusqlite rather than the sqlx pool: this drains the JSONL fault spool into the `faults`
/// table, and the drain and the aggregation have to see the same connection. SQLite in WAL
/// mode takes a second connection without complaint.
#[tauri::command]
async fn get_fault_report(app: tauri::AppHandle) -> Result<Option<FaultReport>, String> {
    // Read the tray state on this side: `Environment::collect` runs on a blocking thread and
    // cannot know it, because the tray belongs to this process rather than to lumen-core.
    let (tray, degradations) = match app.try_state::<StartupHealth>() {
        Some(h) => (Some(h.tray().describe()), h.degradations()),
        None => (None, Vec::new()),
    };
    tauri::async_runtime::spawn_blocking(move || {
        let path = lumen_core::meter::db_path()
            .ok_or_else(|| "cannot resolve a database path".to_string())?;
        let conn = lumen_core::meter::connect_db(&path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;

        let faults = lumen_core::report::load_faults_from_db(&conn)?;
        // "gui", not the default "cli": this report is being filed from the app's own button,
        // and a report that misnames its own channel misleads the one reader who trusts it.
        let mut env = lumen_core::report::Environment::collect_for("gui");
        env.tray = tray;
        env.startup_degradations = degradations;

        // Metadata-only by default, exactly as the CLI renders it. Embedding source is a
        // deliberate opt-in with a manifest, which is not something a button can offer.
        let opts = lumen_core::report::RenderOpts::default();
        Ok(
            lumen_core::report::render(&faults, &env, &opts).map(|body| FaultReport {
                title: lumen_core::report::title_from(&body),
                fingerprint: lumen_core::report::fingerprint(&faults, &env),
                kinds: faults.len(),
                occurrences: faults.iter().map(|f| f.count).sum(),
                repo: lumen_core::report::DEFAULT_REPO.to_string(),
                body,
            }),
        )
    })
    .await
    .map_err(|e| format!("fault report task failed: {e}"))?
}

/// Check whether a newer Lumen has been released, for minor and major bumps only.
///
/// **The only unprompted network request Lumen makes.** An unauthenticated GET of the
/// repository's latest release: no credential, no identifier, nothing about the machine or
/// the ledger. `LUMEN_UPDATE_CHECK=0` disables it, and the README documents it under
/// Security & privacy alongside everything else that leaves the machine — which is
/// otherwise nothing.
///
/// Returns `None` when there is nothing to say: check disabled, not yet due, already
/// current, a patch-only bump, or this version already announced. The frontend shows a
/// notification only for `Some`.
#[tauri::command]
async fn check_for_update() -> Result<Option<lumen_core::update::UpdateAvailable>, String> {
    use lumen_core::update;

    tauri::async_runtime::spawn_blocking(|| {
        if !update::enabled() {
            return Ok(None);
        }
        let Some(state_path) = update::state_path() else {
            return Ok(None);
        };
        let mut state = update::load_state(&state_path);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if !update::due(&state, now, update::CHECK_INTERVAL_SECS) {
            return Ok(None);
        }

        // Recorded before the request, not after: a network that always fails must not
        // turn into a request on every launch.
        state.last_checked = now;
        update::save_state(&state_path, &state);

        let repo = lumen_core::report::DEFAULT_REPO;
        let json = match fetch_latest_release(repo) {
            Some(j) => j,
            // Offline is not an error worth surfacing; the next check will try again.
            None => return Ok(None),
        };
        let Some(latest) = update::latest_from_json(&json) else {
            return Ok(None);
        };

        let found = update::decide(update::Version::current(), latest, &state, repo);
        if found.is_some() {
            // Announce a given version once.
            state.last_notified = Some(latest.to_string());
            update::save_state(&state_path, &state);
        }
        Ok(found)
    })
    .await
    .map_err(|e| format!("update check task failed: {e}"))?
}

/// GET the latest release as JSON, or `None` on any failure.
///
/// curl for the same reason the filing routes use it: the workspace has no HTTP client and
/// adding one pulls a TLS stack in for one request a day.
fn fetch_latest_release(repo: &str) -> Option<String> {
    let out = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "10",
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "X-GitHub-Api-Version: 2022-11-28",
            "--header",
            "User-Agent: lumen",
            &format!("https://api.github.com/repos/{repo}/releases/latest"),
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// How many faults are waiting, for the nav badge and the tray panel.
///
/// Separate from [`get_fault_report`] because a badge refreshes on every navigation and
/// rendering a whole issue body for a number would be absurd. Read-only: it does not
/// drain the spool, so opening a screen is never a write.
#[tauri::command]
async fn get_fault_count() -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(path) = lumen_core::meter::db_path() else {
            return Ok(0);
        };
        // A database that will not open is not a reason to fail a navigation; the badge
        // simply stays dark and the report screen reports the real error.
        match lumen_core::meter::connect_db(&path) {
            Ok(conn) => Ok(lumen_core::report::actionable_fault_count(&conn)),
            Err(_) => Ok(0),
        }
    })
    .await
    .map_err(|e| format!("fault count task failed: {e}"))?
}

/// Width of the tray popover. Fixed: it is positioned under the tray icon and a varying
/// width would make it jump horizontally as rows appear.
const PANEL_WIDTH: f64 = 320.0;
/// Never shorter than the gauge it exists to show, never taller than a popover should be.
const PANEL_MIN_HEIGHT: f64 = 400.0;
const PANEL_MAX_HEIGHT: f64 = 720.0;

/// Resize the tray popover to fit its content.
///
/// The popover was a fixed 320x400 window whose card clipped anything that did not fit,
/// so each conditional row added — the project label, the compaction badge, a recorded
/// fault, an update notice — ate into the space below it until the fault button was
/// entirely off-screen. It was rendered, styled and unreachable.
///
/// Clamped rather than trusted: a measurement bug should make the popover slightly wrong,
/// not turn it into a full-screen sheet or collapse it to nothing.
#[tauri::command]
async fn resize_panel(app: tauri::AppHandle, height: f64) -> Result<(), String> {
    let window = app
        .get_webview_window("panel")
        .ok_or_else(|| "panel window not found".to_string())?;
    let h = height.clamp(PANEL_MIN_HEIGHT, PANEL_MAX_HEIGHT);
    window
        .set_size(tauri::LogicalSize::new(PANEL_WIDTH, h))
        .map_err(|e| format!("cannot resize the panel: {e}"))?;
    // Re-park under the tray icon: the positioner anchors by the window's own geometry, so
    // a taller window left unmoved hangs down past where it was placed.
    let _ = window.move_window(Position::TrayBottomCenter);
    Ok(())
}

/// Reveal the main window, for the tray panel's fault indicator.
///
/// The panel is a 320x400 popover with no navigation, so a fault noticed there has no
/// route to the report screen without this.
#[tauri::command]
async fn show_main_window(app: tauri::AppHandle) -> Result<(), String> {
    let w = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    w.show().map_err(|e| e.to_string())?;
    let _ = w.set_focus();
    Ok(())
}

/// What filing did, and which route did it.
///
/// `handoff` is the field that matters: the browser route opens a prefilled form and
/// nothing is published until the user submits it, so the UI must not say "Filed".
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilingResult {
    pub url: String,
    /// `gh`, `api` or `browser`.
    pub route: String,
    pub handoff: bool,
    pub commented: bool,
    /// Why each earlier route was passed over. Shown so a degraded setup is visible.
    pub fell_back: Vec<String>,
}

/// File a previously-rendered body, commenting on the existing issue if this fingerprint
/// has already been reported.
///
/// Takes the body rather than regenerating it: the user approved a specific text, and this
/// is the call that publishes it. The frontend must not reach this without that approval.
#[tauri::command]
async fn file_fault_report(
    body: String,
    title: String,
    fingerprint: String,
    repo: String,
) -> Result<FilingResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let filing = lumen_core::report::file_issue(&repo, &title, &body, &fingerprint)?;
        let (url, handoff, commented) = match filing.outcome {
            lumen_core::report::Filed::Created(u) => (u, false, false),
            lumen_core::report::Filed::Commented(u) => (u, false, true),
            lumen_core::report::Filed::Handoff(u) => (u, true, false),
        };
        Ok(FilingResult {
            url,
            route: filing.route.to_string(),
            handoff,
            commented,
            fell_back: filing.fell_back,
        })
    })
    .await
    .map_err(|e| format!("filing task failed: {e}"))?
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
