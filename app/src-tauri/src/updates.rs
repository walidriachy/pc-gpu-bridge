//! Automatic updates. The app checks GitHub releases for a newer signed version (`latest.json`)
//! a couple of minutes after start and then every hour, installs it (once no PC job is running) and
//! restarts itself.
//! Every update is signed with the project's key; unsigned or tampered downloads are refused.

use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

const FIRST_CHECK: Duration = Duration::from_secs(120);
const EVERY: Duration = Duration::from_secs(60 * 60);
/// After downloading, how long to wait for the PC's jobs to finish before giving up until next time.
const MAX_IDLE_WAIT: Duration = Duration::from_secs(3 * 60 * 60);
const IDLE_POLL: Duration = Duration::from_secs(20);

/// Don't restart the PC side while it has work: the helper would stop with it. Running jobs,
/// installs and jobs still waiting in the queue all count.
fn pc_is_busy() -> bool {
    if !cfg!(windows) {
        return false;
    }
    crate::win::local_client()
        .and_then(|c| c.status().ok())
        .map(|s| {
            let running = s["running"].as_array().is_some_and(|r| !r.is_empty());
            let waiting = s["waiting"].as_u64().unwrap_or(0) > 0;
            running || waiting
        })
        .unwrap_or(false)
}

/// Wait (blocking) until the PC has no work, up to `max`. True when it is idle.
fn wait_until_idle(max: Duration) -> bool {
    let start = Instant::now();
    while pc_is_busy() {
        if start.elapsed() >= max {
            return false;
        }
        std::thread::sleep(IDLE_POLL);
    }
    true
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
    let bytes =
        update.download(|_, _| {}, || {}).await.map_err(|e| format!("Couldn't download version {version}: {e}"))?;
    // A job may have started during the download: wait for it instead of cutting it off.
    let idle = tauri::async_runtime::spawn_blocking(|| wait_until_idle(MAX_IDLE_WAIT)).await.unwrap_or(false);
    if !idle {
        return Ok(json!({"available": true, "current": current, "version": version, "waiting": "a job is running"}));
    }
    update.install(bytes).map_err(|e| format!("Couldn't install version {version}: {e}"))?;
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
