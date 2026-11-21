//! Menu bar (Mac) / system tray (Windows) icon: green = PC ready, yellow = asleep / waking,
//! red = not connected. The menu shows GPU, VRAM, Mac vs PC free RAM, the current job and quick actions.

use bridge_core::config::BridgeConfig;
use serde_json::Value;
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Light {
    Green,
    Yellow,
    Red,
}

pub struct TrayItems {
    status: MenuItem<Wry>,
    gpu: MenuItem<Wry>,
    ram: MenuItem<Wry>,
    job: MenuItem<Wry>,
    pause: Option<CheckMenuItem<Wry>>,
}

/// A 32x32 filled circle with a soft edge.
pub fn dot(light: Light) -> Image<'static> {
    let (r, g, b) = match light {
        Light::Green => (34u8, 197u8, 94u8),
        Light::Yellow => (234, 179, 8),
        Light::Red => (239, 68, 68),
    };
    let n = 32usize;
    let mut px = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            let d = (((x as f32 - 15.5).powi(2) + (y as f32 - 15.5).powi(2)).sqrt() - 11.0).clamp(0.0, 1.0);
            px.extend_from_slice(&[r, g, b, ((1.0 - d) * 255.0) as u8]);
        }
    }
    Image::new_owned(px, n as u32, n as u32)
}

fn gb(v: &Value) -> String {
    v.as_f64().map(|b| format!("{:.1} GB", b / (1u64 << 30) as f64)).unwrap_or_else(|| "–".into())
}

pub struct Snapshot {
    pub light: Light,
    pub status: String,
    pub gpu: String,
    pub ram: String,
    pub job: String,
}

pub fn summarize(
    pc: Option<&Value>,
    gpu: Option<&Value>,
    mac_free: Option<u64>,
    light: Light,
    status: &str,
) -> Snapshot {
    let g = gpu.and_then(|g| g["gpus"].get(0));
    let gpu_line = match g {
        Some(g) => format!(
            "GPU {}% · VRAM {:.1}/{:.0} GB",
            g["utilization"].as_f64().unwrap_or(0.0),
            g["vram_used_mb"].as_f64().unwrap_or(0.0) / 1024.0,
            g["vram_total_mb"].as_f64().unwrap_or(0.0) / 1024.0
        ),
        None => "GPU: –".into(),
    };
    let pc_free = pc.map(|p| &p["memory"]["available_bytes"]).cloned().unwrap_or(Value::Null);
    let ram = match mac_free {
        Some(m) => format!("Free RAM: Mac {} · PC {}", gb(&Value::from(m)), gb(&pc_free)),
        None => format!("Free RAM (PC): {}", gb(&pc_free)),
    };
    let job = pc
        .and_then(|p| p["running"].as_array())
        .and_then(|r| r.first())
        .map(|j| format!("Running: {}", j["name"].as_str().unwrap_or("job").chars().take(40).collect::<String>()))
        .unwrap_or_else(|| {
            let waiting = pc.and_then(|p| p["waiting"].as_u64()).unwrap_or(0);
            if waiting > 0 {
                format!("{waiting} job(s) waiting")
            } else {
                "No job running".into()
            }
        });
    Snapshot { light, status: status.into(), gpu: gpu_line, ram, job }
}

fn snapshot() -> Snapshot {
    if cfg!(windows) {
        let c = crate::win::local_client();
        let st = c.as_ref().and_then(|c| c.status().ok());
        let gpu = c.as_ref().and_then(|c| c.gpu().ok());
        let ts_ok = crate::mac::tailscale_status()["running"] == true;
        let (light, label) = match (st.is_some(), ts_ok) {
            (true, true) => (Light::Green, "Ready for jobs from your Mac"),
            (true, false) => (Light::Yellow, "Tailscale is off — the Mac can't reach this PC"),
            _ => (Light::Red, "PC Bridge agent isn't running"),
        };
        summarize(st.as_ref(), gpu.as_ref(), None, light, label)
    } else {
        let cfg = BridgeConfig::load();
        let mac_free = Some(bridge_core::memory::local_memory().available_bytes);
        if !cfg.is_paired() {
            return summarize(None, None, mac_free, Light::Red, "Not set up yet");
        }
        let c = bridge_core::client::AgentClient::new(cfg.base_url(), bridge_core::secrets::get_token());
        match c.status() {
            Ok(st) => {
                let _ = c.post_json("/peer/memory", &serde_json::json!(bridge_core::memory::local_memory()));
                let gpu = c.gpu().ok();
                let label = if cfg.redirect_enabled { "PC ready" } else { "PC ready (redirecting paused)" };
                summarize(Some(&st), gpu.as_ref(), mac_free, Light::Green, label)
            }
            Err(bridge_core::client::ClientError::Unauthorized) => {
                summarize(None, None, mac_free, Light::Red, "PC didn't accept this Mac's key — pair again")
            }
            Err(_) => summarize(None, None, mac_free, Light::Yellow, "PC asleep or offline — will wake for jobs"),
        }
    }
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let disabled = |id: &str, text: &str| MenuItem::with_id(app, id, text, false, None::<&str>);
    let status = disabled("status", "Checking…")?;
    let gpu = disabled("gpu", "GPU: –")?;
    let ram = disabled("ram", "Free RAM: –")?;
    let job = disabled("job", "No job running")?;
    let open_jobs = MenuItem::with_id(app, "open_jobs", "Open job folder", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open PC Bridge", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit PC Bridge", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let pause = if cfg!(windows) {
        None
    } else {
        Some(CheckMenuItem::with_id(
            app,
            "pause",
            "Pause redirecting",
            true,
            !BridgeConfig::load().redirect_enabled,
            None::<&str>,
        )?)
    };
    let menu = Menu::new(app)?;
    for item in [&status as &dyn tauri::menu::IsMenuItem<Wry>, &gpu, &ram, &job, &sep1, &open_jobs] {
        menu.append(item)?;
    }
    if let Some(p) = &pause {
        menu.append(p)?;
    }
    for item in [&sep2 as &dyn tauri::menu::IsMenuItem<Wry>, &open, &quit] {
        menu.append(item)?;
    }

    TrayIconBuilder::with_id("main")
        .icon(dot(Light::Red))
        .tooltip("PC Bridge")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            "pause" => {
                let mut cfg = BridgeConfig::load();
                cfg.redirect_enabled = !cfg.redirect_enabled;
                let _ = cfg.save();
                refresh_pause_item(app, cfg.redirect_enabled);
            }
            "open_jobs" => open_job_folder(app),
            _ => {}
        })
        .build(app)?;

    app.manage(TrayItems { status, gpu, ram, job, pause });
    let handle = app.clone();
    std::thread::spawn(move || loop {
        let s = snapshot();
        if let Some(items) = handle.try_state::<TrayItems>() {
            let _ = items.status.set_text(&s.status);
            let _ = items.gpu.set_text(&s.gpu);
            let _ = items.ram.set_text(&s.ram);
            let _ = items.job.set_text(&s.job);
        }
        if let Some(tray) = handle.tray_by_id("main") {
            let _ = tray.set_icon(Some(dot(s.light)));
            let _ = tray.set_tooltip(Some(format!("PC Bridge — {}", s.status)));
        }
        std::thread::sleep(Duration::from_secs(10));
    });
    Ok(())
}

pub fn refresh_pause_item(app: &AppHandle, redirect_enabled: bool) {
    if let Some(items) = app.try_state::<TrayItems>() {
        if let Some(p) = &items.pause {
            let _ = p.set_checked(!redirect_enabled);
        }
    }
}

pub fn show_main(app: &AppHandle) {
    let w = app.get_webview_window("main").or_else(|| {
        // Closed earlier (the Mac frees the window's memory): make it again from tauri.conf.json.
        let conf = app.config().app.windows.iter().find(|w| w.label == "main")?.clone();
        tauri::WebviewWindowBuilder::from_config(app, &conf).ok()?.build().ok()
    });
    if let Some(w) = w {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn open_job_folder(app: &AppHandle) {
    use tauri_plugin_opener::OpenerExt;
    let path = if cfg!(windows) {
        std::path::PathBuf::from(crate::win::load_state().root).join("jobs")
    } else {
        let cfg = BridgeConfig::load();
        let mapper = bridge_core::paths::PathMapper::new(cfg.shared_folders.clone(), cfg.pc_root.clone());
        let c = bridge_core::client::AgentClient::new(cfg.base_url(), bridge_core::secrets::get_token());
        c.jobs()
            .ok()
            .and_then(|j| j.as_array().cloned())
            .and_then(|jobs| {
                jobs.iter().find_map(|j| j["outputs"].as_array()?.first()?.as_str().and_then(|o| mapper.rel_to_mac(o)))
            })
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .or_else(|| cfg.shared_folders.first().map(|f| f.mac_path.clone().into()))
            .unwrap_or_else(bridge_core::config::home_dir)
    };
    let _ = app.opener().open_path(path.display().to_string(), None::<&str>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summary_lines() {
        let pc =
            json!({"memory": {"available_bytes": 27u64 << 30}, "running": [{"name": "ffmpeg encode"}], "waiting": 0});
        let gpu = json!({"gpus": [{"utilization": 87.0, "vram_used_mb": 4096.0, "vram_total_mb": 24576.0}]});
        let s = summarize(Some(&pc), Some(&gpu), Some(6u64 << 30), Light::Green, "PC ready");
        assert_eq!(s.gpu, "GPU 87% · VRAM 4.0/24 GB");
        assert_eq!(s.ram, "Free RAM: Mac 6.0 GB · PC 27.0 GB");
        assert_eq!(s.job, "Running: ffmpeg encode");
        let idle = summarize(None, None, None, Light::Red, "x");
        assert_eq!(idle.job, "No job running");
        assert_eq!(dot(Light::Green).rgba().len(), 32 * 32 * 4);
    }
}
