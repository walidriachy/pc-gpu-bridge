//! Commands used by the Mac app (and shared commands such as status, jobs and diagnostics).

use crate::state::AppState;
use crate::util::{blocking, run, which};
use bridge_core::claude_config as cc;
use bridge_core::client::AgentClient;
use bridge_core::config::{home_dir, BridgeConfig, SharedFolder};
use bridge_core::memory;
use bridge_core::paths::PathMapper;
use bridge_core::router::Target;
use bridge_core::shim::{Capture, Engine};
use bridge_core::{detect, secrets, syncthing};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::State;

fn client(cfg: &BridgeConfig) -> Option<AgentClient> {
    cfg.is_paired().then(|| AgentClient::new(cfg.base_url(), secrets::get_token()))
}

fn user_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn exe_path() -> String {
    std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "pc-bridge".into())
}

#[tauri::command]
pub fn app_info() -> Value {
    json!({
        "platform": if cfg!(windows) { "windows" } else if cfg!(target_os = "macos") { "macos" } else { "linux" },
        "version": env!("CARGO_PKG_VERSION"),
        "binary": exe_path(),
    })
}

#[tauri::command]
pub fn get_config() -> BridgeConfig {
    BridgeConfig::load()
}

#[tauri::command]
pub fn save_config(cfg: BridgeConfig) -> Result<(), String> {
    cfg.save().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_redirect(enabled: bool, app: tauri::AppHandle) -> Result<(), String> {
    let mut cfg = BridgeConfig::load();
    cfg.redirect_enabled = enabled;
    cfg.save().map_err(|e| e.to_string())?;
    crate::tray::refresh_pause_item(&app, enabled);
    Ok(())
}

// ---------------------------------------------------------------- Tailscale

pub fn tailscale_bin() -> Option<String> {
    let candidates = if cfg!(windows) {
        vec![r"C:\Program Files\Tailscale\tailscale.exe".to_string()]
    } else {
        vec!["/Applications/Tailscale.app/Contents/MacOS/Tailscale".to_string(), "/usr/local/bin/tailscale".into()]
    };
    candidates.into_iter().find(|p| Path::new(p).exists()).or_else(|| which("tailscale"))
}

/// Run the Tailscale CLI. On macOS the CLI is the Tailscale app binary itself; started from another
/// app (no terminal) it tries to open its GUI and prints nothing useful unless TAILSCALE_BE_CLI is set.
fn tailscale_cli(bin: &str, args: &[&str]) -> crate::util::Out {
    match crate::util::command(bin).args(args).env("TAILSCALE_BE_CLI", "1").output() {
        Ok(o) => crate::util::Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into(),
            stderr: String::from_utf8_lossy(&o.stderr).into(),
        },
        Err(e) => crate::util::Out { code: -1, stdout: String::new(), stderr: e.to_string() },
    }
}

pub fn tailscale_status() -> Value {
    let Some(bin) = tailscale_bin() else {
        return json!({"installed": false, "running": false, "peers": []});
    };
    let out = tailscale_cli(&bin, &["status", "--json"]);
    let v: Value = serde_json::from_str(&out.stdout).unwrap_or(Value::Null);
    let state = v["BackendState"].as_str().unwrap_or("");
    if v.is_null() {
        // Couldn't read Tailscale's answer at all: report that instead of "not connected".
        let why = format!("{} {}", out.stdout.trim(), out.stderr.trim());
        return json!({"installed": true, "running": false, "unreadable": true, "detail": why.trim(), "peers": []});
    }
    let peers: Vec<Value> = v["Peer"]
        .as_object()
        .map(|m| {
            m.values()
                .map(|p| {
                    json!({
                        "name": p["HostName"], "dns": p["DNSName"].as_str().unwrap_or("").trim_end_matches('.'),
                        "os": p["OS"], "online": p["Online"],
                        "ip": p["TailscaleIPs"].as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null)
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "installed": true,
        "running": state == "Running",
        "needs_login": state == "NeedsLogin",
        "self_name": v["Self"]["HostName"],
        "self_ip": v["Self"]["TailscaleIPs"].as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null),
        "peers": peers,
    })
}

#[tauri::command]
pub async fn tailscale_check() -> Result<Value, String> {
    blocking(|| Ok(tailscale_status())).await
}

// ---------------------------------------------------------------- Pairing

fn computer_name() -> String {
    if cfg!(target_os = "macos") {
        let o = run("scutil", &["--get", "ComputerName"]);
        if o.ok() {
            return o.stdout.trim().to_string();
        }
    }
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Mac".into())
}

#[tauri::command]
pub async fn pair_pc(host: String, code: String) -> Result<Value, String> {
    blocking(move || {
        let host = host.trim().trim_end_matches('.').to_string();
        let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
        if code.len() != 6 {
            return Err("The pairing code has 6 digits — please check it on the PC screen.".into());
        }
        let c = AgentClient::new(format!("http://{host}:8765"), None);
        c.health().map_err(|e| {
            format!(
                "Can't reach PC Bridge on '{host}'. Is the PC on, with PC Bridge open and Tailscale connected? ({e})"
            )
        })?;
        let r = c.pair(&code, &computer_name()).map_err(|e| e.to_string())?;
        let token = r["token"].as_str().ok_or("The PC didn't send a token")?;
        secrets::set_token(token)?;
        let mut cfg = BridgeConfig::load();
        cfg.pc_host = host;
        cfg.pc_root = r["root"].as_str().unwrap_or(r"D:\PCBridge").to_string();
        cfg.pc_hostname = r["hostname"].as_str().unwrap_or("").to_string();
        cfg.pc_mac_address = r["mac_address"].as_str().unwrap_or("").to_string();
        cfg.save().map_err(|e| e.to_string())?;
        Ok(json!({"hostname": cfg.pc_hostname, "root": cfg.pc_root}))
    })
    .await
}

#[tauri::command]
pub fn unpair() -> Result<(), String> {
    secrets::clear_token();
    let mut cfg = BridgeConfig::load();
    cfg.pc_host.clear();
    cfg.save().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- Syncthing (shared folders)

pub fn ensure_local_syncthing(state: &AppState, cfg: &mut BridgeConfig) -> Result<syncthing::Syncthing, String> {
    if cfg.syncthing_api_key.is_empty() {
        cfg.syncthing_api_key = crate::util::random_key(32);
        cfg.save().map_err(|e| e.to_string())?;
    }
    let st = syncthing::Syncthing::new(cfg.syncthing_api_key.clone());
    if st.alive() {
        return Ok(st);
    }
    let bin = if cfg!(target_os = "macos") {
        syncthing::download_macos()?
    } else {
        which("syncthing").map(PathBuf::from).ok_or("Syncthing isn't installed")?
    };
    let child = syncthing::start(&bin, &syncthing::home().join("data"), &cfg.syncthing_api_key)?;
    state.procs.lock().unwrap().syncthing = Some(child);
    for _ in 0..40 {
        if st.alive() {
            return Ok(st);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("Syncthing didn't start".into())
}

#[tauri::command]
pub async fn share_folders(folders: Vec<String>, state: State<'_, AppState>) -> Result<Value, String> {
    let state = state.inner().clone();
    blocking(move || {
        let mut cfg = BridgeConfig::load();
        let mut mapper = PathMapper::new(vec![], cfg.pc_root.clone());
        let mut list: Vec<SharedFolder> = vec![];
        for f in folders {
            let p = PathBuf::from(&f);
            if !p.is_dir() {
                return Err(format!("{f} isn't a folder"));
            }
            if let Some(existing) = cfg.shared_folders.iter().find(|s| s.mac_path == f) {
                list.push(existing.clone());
            } else {
                list.push(SharedFolder { mac_path: f.clone(), name: mapper.suggest_name(&p) });
            }
            mapper.folders = list.clone();
        }
        cfg.shared_folders = list.clone();
        cfg.save().map_err(|e| e.to_string())?;

        let st = ensure_local_syncthing(&state, &mut cfg)?;
        let mac_id = st.device_id()?;
        let c = client(&cfg).ok_or("Pair with the PC first")?;
        let names: Vec<String> = list.iter().map(|f| f.name.clone()).collect();
        let r = c
            .post_json(
                "/syncthing/share",
                &json!({"mac_device_id": mac_id, "mac_name": computer_name(), "folders": names}),
            )
            .map_err(|e| e.to_string())?;
        let pc_id = r["pc_device_id"].as_str().ok_or("The PC didn't return its Syncthing ID")?.to_string();
        st.add_device(&pc_id, "PC", &format!("tcp://{}:22000", cfg.pc_host))?;
        for f in &list {
            st.share_folder(&f.name, Path::new(&f.mac_path), std::slice::from_ref(&pc_id))?;
        }
        Ok(json!({"folders": list, "mac_device_id": mac_id, "pc_device_id": pc_id}))
    })
    .await
}

// ---------------------------------------------------------------- Claude connection

#[tauri::command]
pub fn claude_preview() -> Value {
    let home = user_home();
    let bin_dir = home_dir().join("bin");
    let desktop = cc::claude_desktop_config(&home);
    json!({
        "claude_md_snippet": cc::CLAUDE_MD_SNIPPET,
        "changes": [
            {"what": "Claude Code: add the PC Bridge connection", "path": cc::claude_code_config(&home), "exists": cc::claude_code_config(&home).exists()},
            {"what": "Claude Desktop: add the PC Bridge connection", "path": desktop, "exists": desktop.exists()},
            {"what": "Your Claude instructions (CLAUDE.md): add the routing rules shown below", "path": cc::claude_md(&home), "exists": cc::claude_md(&home).exists()},
            {"what": "Install helper commands (ffmpeg, python, blender, docker, npm…) that forward heavy jobs", "path": bin_dir, "exists": bin_dir.exists()},
            {"what": "Add those helpers to your Terminal's search path", "path": home.join(".zshrc"), "exists": home.join(".zshrc").exists()},
        ],
        "installed": {
            "claude_code": cc::mcp_installed(&cc::claude_code_config(&home)),
            "claude_desktop": cc::mcp_installed(&desktop),
        }
    })
}

#[tauri::command]
pub fn claude_install(include_claude_md: bool, include_shims: bool) -> Result<Vec<String>, String> {
    let home = user_home();
    let bin = exe_path();
    let mut done = vec![];
    cc::install_mcp(&cc::claude_code_config(&home), &bin)?;
    done.push("Connected Claude Code".to_string());
    cc::install_mcp(&cc::claude_desktop_config(&home), &bin)?;
    done.push("Connected Claude Desktop (restart it to see PC Bridge)".into());
    if include_claude_md {
        cc::install_claude_md(&cc::claude_md(&home))?;
        done.push("Added routing rules to CLAUDE.md".into());
    }
    if include_shims {
        let bin_dir = home_dir().join("bin");
        cc::install_shims(&bin_dir, &bin, detect::SHIM_TOOLS)?;
        cc::install_path(&home, &bin_dir)?;
        done.push(format!("Installed {} helper commands", detect::SHIM_TOOLS.len()));
    }
    Ok(done)
}

#[tauri::command]
pub fn claude_uninstall() -> Result<(), String> {
    let home = user_home();
    let _ = cc::uninstall_mcp(&cc::claude_code_config(&home));
    let _ = cc::uninstall_mcp(&cc::claude_desktop_config(&home));
    let _ = cc::uninstall_claude_md(&cc::claude_md(&home));
    cc::uninstall_path(&home);
    let _ = std::fs::remove_dir_all(home_dir().join("bin"));
    Ok(())
}

// ---------------------------------------------------------------- Network check

fn default_interface_kind() -> (String, String) {
    if !cfg!(target_os = "macos") {
        return ("unknown".into(), String::new());
    }
    let route = run("route", &["-n", "get", "default"]).stdout;
    let iface = route
        .lines()
        .find_map(|l| l.trim().strip_prefix("interface:"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let ports = run("networksetup", &["-listallhardwareports"]).stdout;
    let mut last_port = String::new();
    for line in ports.lines() {
        if let Some(p) = line.strip_prefix("Hardware Port: ") {
            last_port = p.to_string();
        }
        if line.strip_prefix("Device: ").map(|d| d.trim() == iface).unwrap_or(false) {
            let kind = if last_port.contains("Wi-Fi") { "wifi" } else { "ethernet" };
            return (kind.into(), last_port);
        }
    }
    ("unknown".into(), iface)
}

pub fn network_advice(kind: &str, mbps: f64) -> String {
    let speed = if mbps >= 500.0 {
        "fast — great for big video files"
    } else if mbps >= 150.0 {
        "good — fine for most jobs"
    } else if mbps >= 50.0 {
        "OK — small and medium files are fine; huge files will take a while to copy"
    } else {
        "slow — big files will take a long time to copy, so PC Bridge will keep more jobs on the Mac"
    };
    let mut s = format!("Your Mac ↔ PC speed is about {mbps:.0} Mbit/s: {speed}.");
    if kind == "wifi" {
        s.push_str(" Your Mac is on Wi-Fi. If you often send large videos, a USB-C Ethernet adapter (about $20–30) plugged into your router can make transfers several times faster.");
    } else if kind == "ethernet" {
        s.push_str(" Your Mac is on a wired connection — the best you can do.");
    }
    s
}

#[tauri::command]
pub async fn network_check() -> Result<Value, String> {
    blocking(|| {
        let mut cfg = BridgeConfig::load();
        let c = client(&cfg).ok_or("Pair with the PC first")?;
        let (kind, port) = default_interface_kind();
        let (up, down) = c.speedtest(20).map_err(|e| e.to_string())?;
        let mbps = up.min(down);
        cfg.measured_mbps = mbps;
        cfg.save().map_err(|e| e.to_string())?;
        Ok(json!({"kind": kind, "port": port, "up_mbps": up, "down_mbps": down, "advice": network_advice(&kind, mbps)}))
    })
    .await
}

// ---------------------------------------------------------------- Final test

#[tauri::command]
pub async fn final_test() -> Result<Value, String> {
    blocking(|| {
        let cfg = BridgeConfig::load();
        let share = cfg.shared_folders.first().cloned().ok_or("Share at least one folder first")?;
        let cwd = PathBuf::from(&share.mac_path);
        let engine = Engine::from_config(cfg.clone());
        let args: Vec<String> = [
            "-y", "-hide_banner", "-f", "lavfi", "-i", "testsrc2=duration=20:size=1920x1080:rate=30",
            "-c:v", "h264_nvenc", "-preset", "p5", "pcbridge-test-encode.mp4",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut io = Capture::default();
        let r = engine.run("ffmpeg", &args, &cwd, Some(Target::Pc), &mut io);
        if r.exit_code != 0 {
            return Err(format!(
                "The test encode didn't work (code {}). {}\n{}",
                r.exit_code,
                r.reason,
                io.stderr.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
            ));
        }
        // Rough Mac estimate: the M1 Pro's CPU encoder is ~4x slower than NVENC for 1080p H.264.
        let mac_est = r.seconds * bridge_core::router::default_speedup("ffmpeg", detect::JobKind::Gpu);
        let mut gpu_io = Capture::default();
        let code = "import torch;print('CUDA', torch.cuda.is_available(), torch.cuda.get_device_name(0) if torch.cuda.is_available() else '')";
        let py = engine.run("python", &["-c".into(), code.into()], &cwd, Some(Target::Pc), &mut gpu_io);
        Ok(json!({
            "encode_secs": r.seconds,
            "mac_est_secs": mac_est,
            "output": r.outputs.first().map(|p| p.display().to_string()).unwrap_or_else(|| cwd.join("pcbridge-test-encode.mp4").display().to_string()),
            "gpu_python": {"ok": py.exit_code == 0, "text": if py.exit_code == 0 { gpu_io.stdout.trim().to_string() } else {
                "Python GPU libraries (PyTorch) aren't installed on the PC yet. That's optional — video jobs already work.".to_string() }},
        }))
    })
    .await
}

// ---------------------------------------------------------------- Status & jobs

#[tauri::command]
pub async fn status_snapshot() -> Result<Value, String> {
    blocking(|| {
        let cfg = BridgeConfig::load();
        let mac = memory::local_memory();
        let c = client(&cfg);
        let pc = c.as_ref().map(|c| c.status());
        if let (Some(c), Some(Ok(_))) = (c.as_ref(), pc.as_ref()) {
            let _ = c.post_json("/peer/memory", &json!(mac));
        }
        let (reachable, pc_status, err) = match pc {
            Some(Ok(s)) => (true, s, Value::Null),
            Some(Err(e)) => (false, Value::Null, json!(e.to_string())),
            None => (false, Value::Null, json!("not paired")),
        };
        let gpu = if reachable { c.as_ref().and_then(|c| c.gpu().ok()).unwrap_or(Value::Null) } else { Value::Null };
        Ok(json!({
            "paired": cfg.is_paired(),
            "reachable": reachable,
            "error": err,
            "redirect_enabled": cfg.redirect_enabled,
            "pc_hostname": cfg.pc_hostname,
            "mac_memory": mac,
            "pc": pc_status,
            "gpu": gpu,
            "shared_folders": cfg.shared_folders,
        }))
    })
    .await
}

#[tauri::command]
pub async fn jobs_list() -> Result<Value, String> {
    blocking(|| {
        let cfg = BridgeConfig::load();
        let mapper = PathMapper::new(cfg.shared_folders.clone(), cfg.pc_root.clone());
        let pc_jobs = client(&cfg).and_then(|c| c.jobs().ok()).unwrap_or(json!([]));
        let jobs: Vec<Value> = pc_jobs
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut j| {
                let outs: Vec<String> = j["outputs"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|o| o.as_str())
                            .filter_map(|o| mapper.rel_to_mac(o))
                            .map(|p| p.display().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                j["mac_outputs"] = json!(outs);
                j
            })
            .collect();
        let history = bridge_core::router::History::open(&home_dir().join("history.sqlite"))
            .map(|h| h.recent(100))
            .unwrap_or_default();
        Ok(json!({"pc_jobs": jobs, "history": history}))
    })
    .await
}

#[tauri::command]
pub async fn cancel_job(id: String) -> Result<(), String> {
    blocking(move || {
        let cfg = BridgeConfig::load();
        client(&cfg).ok_or("Not paired")?.cancel(&id).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn wake_pc() -> Result<String, String> {
    blocking(|| {
        let cfg = BridgeConfig::load();
        if cfg.pc_mac_address.is_empty() {
            return Err("The PC's network address for waking isn't known yet".into());
        }
        bridge_core::wol::wake(&cfg.pc_mac_address).map_err(|e| e.to_string())?;
        Ok("Wake signal sent — the PC usually needs 15–40 seconds.".into())
    })
    .await
}

// ---------------------------------------------------------------- Diagnostics

pub fn check(name: &str, ok: bool, detail: impl Into<String>, fix: &str) -> Value {
    json!({"name": name, "ok": ok, "detail": detail.into(), "fix": if ok { "" } else { fix }})
}

#[tauri::command]
pub async fn run_diagnostics_mac() -> Result<Vec<Value>, String> {
    blocking(|| {
        let cfg = BridgeConfig::load();
        let home = user_home();
        let mut out = vec![];
        let ts = tailscale_status();
        out.push(check("Tailscale installed", ts["installed"] == true, "", "Open the Setup again and use “Install Tailscale”."));
        out.push(check("Tailscale connected", ts["running"] == true, "", "Click the Tailscale icon in the menu bar and choose “Connect” / log in."));
        out.push(check("Paired with a PC", cfg.is_paired(), cfg.pc_hostname.clone(), "Go to Settings → “Pair again” and enter the code shown on the PC."));
        out.push(check("Pairing key saved in Keychain", secrets::get_token().is_some(), "", "Pair again — this saves a fresh key."));
        let c = client(&cfg);
        let status = c.as_ref().map(|c| c.status());
        let reachable = matches!(status, Some(Ok(_)));
        out.push(check("PC reachable", reachable,
            match &status { Some(Err(e)) => e.to_string(), _ => String::new() },
            "Make sure the PC is switched on, PC Bridge is running there (green icon by the clock), and Tailscale is connected on both computers. If the PC is asleep, press “Wake PC”."));
        if let Some(Ok(s)) = &status {
            for t in ["ffmpeg", "claude"] {
                out.push(check(&format!("PC has {t}"), s["tools"][t] == true, "", "On the PC, open PC Bridge → Setup and re-run the install step."));
            }
            let gpu = c.as_ref().and_then(|c| c.gpu().ok()).unwrap_or(Value::Null);
            out.push(check("PC graphics card detected", gpu["available"] == true,
                gpu["gpus"][0]["name"].as_str().unwrap_or("").to_string(),
                "On the PC, install the latest NVIDIA driver (Setup → step 2)."));
        }
        let st = syncthing::Syncthing::new(cfg.syncthing_api_key.clone());
        out.push(check("Folder syncing running", !cfg.syncthing_api_key.is_empty() && st.alive(), "",
            "Open Setup → “Choose folders” and click “Share these folders” again."));
        for f in &cfg.shared_folders {
            out.push(check(&format!("Shared folder “{}”", f.name), Path::new(&f.mac_path).is_dir(), f.mac_path.clone(),
                "This folder was moved or deleted. Choose it again in Settings."));
        }
        out.push(check("Claude Code connected", cc::mcp_installed(&cc::claude_code_config(&home)), "", "Settings → “Connect to Claude”."));
        let zshrc = std::fs::read_to_string(home.join(".zshrc")).unwrap_or_default();
        out.push(check("Helper commands on PATH", zshrc.contains(cc::PATH_BEGIN), "", "Settings → “Connect to Claude” with helpers enabled, then open a new Terminal window."));
        let mem = memory::local_memory();
        out.push(check("Mac memory", mem.pressure != memory::Pressure::Critical,
            format!("{:.1} GB free, pressure {:?}", mem.available_bytes as f64 / (1u64 << 30) as f64, mem.pressure),
            "Your Mac is very short on memory right now. Close apps you aren't using; heavy jobs will automatically go to the PC."));
        Ok(out)
    })
    .await
}
