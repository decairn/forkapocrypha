mod api;
mod commands;
mod db;
mod installer;
mod models;
mod process_watcher;
mod savedvars;
mod scanner;
mod state;

use state::AppState;
use std::future::Future;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;

const HIDDEN_ARG: &str = "--hidden";
// Polling cadence when auto-update is disabled — small enough that the user
// doesn't wait too long after enabling it for the first tick to fire.
const POLL_WHEN_DISABLED: Duration = Duration::from_secs(60);

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn can_auto_update() -> bool {
    match process_watcher::is_eso_running() {
        Ok(is_running) => !is_running,
        Err(error) => {
            eprintln!("failed to check ESO process status; skipping auto-update: {error}");
            false
        }
    }
}

async fn refresh_catalog_for_auto_update(handle: &tauri::AppHandle) -> bool {
    let state = handle.state::<AppState>();
    match state.run_sync(handle).await {
        Ok(_) => true,
        Err(error) => {
            eprintln!("automatic catalog refresh failed; skipping update check: {error:#}");
            false
        }
    }
}

async fn wait_for_auto_update_tick<F: Future<Output = ()>>(
    current_interval: impl Fn() -> u64,
    sleep: impl FnOnce(Duration) -> F,
) -> Option<u64> {
    let interval = current_interval();
    if interval == 0 {
        sleep(POLL_WHEN_DISABLED).await;
        return None;
    }

    sleep(Duration::from_secs(interval * 60)).await;
    (current_interval() == interval).then_some(interval)
}

async fn auto_update_loop(handle: tauri::AppHandle) {
    let state = handle.state::<AppState>();
    loop {
        let Some(interval) =
            wait_for_auto_update_tick(|| state.auto_update_interval(), tokio::time::sleep).await
        else {
            continue;
        };

        // Don't touch addon files while the user is in-game.
        if !can_auto_update() {
            continue;
        }

        // Refresh before reading update state. A failed or concurrent refresh
        // must never cause us to update from the stale SQLite catalog.
        if !refresh_catalog_for_auto_update(&handle).await
            || state.auto_update_interval() != interval
        {
            continue;
        }

        // Pull the current list of installed-and-out-of-date addons.
        let to_update: Vec<_> = match commands::enumerate_installed(&state).await {
            Ok(list) => list
                .into_iter()
                .filter(|a| a.update_available)
                .filter_map(|a| {
                    let addon_id = a.catalog_id?;
                    let addon_title = if a.title.trim().is_empty() {
                        a.dir_name
                    } else {
                        a.title
                    };
                    Some((addon_id, addon_title))
                })
                .collect(),
            Err(_) => continue,
        };

        for (addon_id, addon_title) in to_update {
            // The game or settings can change during the scan or a prior install.
            if !can_auto_update() || state.auto_update_interval() != interval {
                break;
            }
            if let Err(error) =
                installer::install_addon(state.inner(), &handle, addon_id.clone(), None).await
            {
                eprintln!("auto-update failed for {addon_title} (addon {addon_id}): {error:#}");
                let _ = handle.emit(
                    "addon:install:error",
                    &serde_json::json!({
                        "id": addon_id,
                        "error": error.to_string(),
                        "addon_title": addon_title,
                    }),
                );
            }
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_state = AppState::init().expect("failed to initialize app state");

    let start_hidden = std::env::args().any(|a| a == HIDDEN_ARG);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second launch (desktop shortcut, autostart, etc.) should just
            // surface the running window instead of spawning a duplicate
            // process — otherwise we leak a tray icon per launch.
            show_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_ARG]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(app_state)
        .manage(installer::InstallConflictApproval::default())
        .setup(move |app| {
            // Tray icon with a minimal Show / Quit menu.
            let show_item =
                MenuItem::with_id(app, "show", "Show Apocrypha", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit Apocrypha", true, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&show_item, &separator, &quit_item])?;

            let _tray = TrayIconBuilder::with_id("apocrypha-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Apocrypha")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            // Show the main window unless the user (or autostart) asked us to
            // stay tucked in the tray.
            if !start_hidden {
                show_main_window(&app.handle().clone());
            }

            // Catalog auto-sync on every boot.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                // One-shot migration: v0.1.8 / v0.1.9 dropped translation
                // overrides (`gamedata/`, `EsoUI/`) into `live/` instead of
                // `live/AddOns/`. Move any leftovers back where they belong.
                installer::migrate_live_orphans(state.inner()).await;
                if let Err(error) = state.run_sync(&handle).await {
                    eprintln!("startup catalog refresh failed: {error:#}");
                }
            });

            // Background auto-update loop (only acts when the setting is on
            // and ESO isn't currently running).
            let update_handle = app.handle().clone();
            tauri::async_runtime::spawn(auto_update_loop(update_handle));

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                if let Some(state) = app.try_state::<AppState>() {
                    if state.close_to_tray.load(Ordering::SeqCst) {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::search_addons,
            commands::list_categories,
            commands::catalog_meta,
            commands::list_installed,
            commands::sync_catalog,
            commands::get_addons_dir,
            commands::set_addons_dir,
            commands::install_addon,
            commands::resolve_install_conflict,
            commands::preview_uninstall_addon,
            commands::uninstall_addon,
            commands::uninstall_legacy_addon,
            commands::get_addon_by_id,
            commands::get_addon_details,
            commands::list_savedvars_snapshots,
            commands::create_savedvars_snapshot,
            commands::restore_savedvars_snapshot,
            commands::delete_savedvars_snapshot,
            commands::get_close_to_tray,
            commands::set_close_to_tray,
            commands::get_auto_update_interval,
            commands::set_auto_update_interval,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{wait_for_auto_update_tick, Duration, POLL_WHEN_DISABLED};
    use std::cell::Cell;

    #[tokio::test]
    async fn auto_update_tick_uses_the_setting_at_the_end_of_the_wait() {
        for (before, after, wait, expected) in [
            (60, 0, Duration::from_secs(3600), None),
            (60, 120, Duration::from_secs(3600), None),
            (60, 60, Duration::from_secs(3600), Some(60)),
            (0, 60, POLL_WHEN_DISABLED, None),
        ] {
            let interval = &Cell::new(before);
            let tick = wait_for_auto_update_tick(
                || interval.get(),
                |delay| async move {
                    assert_eq!(delay, wait);
                    interval.set(after);
                },
            )
            .await;
            assert_eq!(tick, expected, "interval changed from {before} to {after}");
        }
    }
}
