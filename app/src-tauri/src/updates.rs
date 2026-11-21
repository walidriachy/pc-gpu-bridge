//! Automatic updates. The app checks GitHub releases for a newer signed version (`latest.json`)
//! a couple of minutes after start and then every 6 hours, installs it and restarts itself.
//! Every update is signed with the project's key; unsigned or tampered downloads are refused.

use serde_json::{json, Value};
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

const FIRST_CHECK: Duration = Duration::from_secs(120);
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// Don't restart the PC side in the middle of a job: the helper would stop with it.
fn pc_is_busy() -> bool {
    if !cfg!(windows) {
        return false;
    }
    crate::win::local_client()
        .and_then(|c| c.status().ok())
        .and_then(|s| s["running"].as_array().map(|r| !r.is_empty()))
        .unwrap_or(false)
}

/// Check for an update; when `install` is true, download it, install it and restart.
pub async fn check(app: &AppHandle, install: bool) -> Result<Value, String> {
    let current = app.package_info().version.to_string();
    let updater = app.updater().map_err(|e| e.to_string())?;
    let found = updater.check().await.map_err(|e| format!("Couldn't check for updates: {e}"))?;
    let Some(update) = found else {
        return Ok(json!({"available": false, "current": current}));
    };
    let version = update.version.clone();
    if !install {
        return Ok(json!({"available": true, "current": current, "version": version}));
    }
    if pc_is_busy() {
        return Ok(json!({"available": true, "current": current, "version": version, "waiting": "a job is running"}));
    }
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| format!("Couldn't install version {version}: {e}"))?;
    app.restart();
}

/// Background checks (started from `setup`).
pub fn spawn_auto_update(app: AppHandle) {
    if cfg!(debug_assertions) {
        return; // never replace a development build
    }
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK);
        loop {
            if let Err(e) = tauri::async_runtime::block_on(check(&app, true)) {
                eprintln!("PC Bridge: {e}");
            }
            std::thread::sleep(EVERY);
        }
    });
}

/// Settings → "Check for updates".
#[tauri::command]
pub async fn check_for_update(app: AppHandle, install: bool) -> Result<Value, String> {
    check(&app, install).await
}
