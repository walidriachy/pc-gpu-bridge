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

// ---------------------------------------------------------------- Install location (Gatekeeper)

/// Where the Mac app should live. Updates, the Claude connection and the helper commands use this path.
pub const APPLICATIONS_APP: &str = "/Applications/PC Bridge.app";

/// What to do about where the Mac app is running from.
///
/// When a downloaded app is opened while it still carries the "downloaded from the internet" mark
/// (`com.apple.quarantine`), macOS runs a read-only copy from a random hidden folder
/// (".../AppTranslocation/..."). Updates can't replace that copy and the paths given to Claude stop
/// working, so PC Bridge moves itself back to Applications.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallFix {
    Nothing,
    /// Running from Applications, but the download mark is still on it: remove the mark.
    ClearQuarantine,
    /// Running from the hidden read-only copy and PC Bridge is in Applications: remove the mark from
    /// the Applications copy, open that one and quit this one.
    RelaunchFromApplications,
    /// PC Bridge isn't in Applications: ask the user to drag it there.
    AskToMove,
}

/// Decide what to do from the running program's path, whether `/Applications/PC Bridge.app` carries
/// the download mark, whether `/Applications/PC Bridge.app` exists, and whether it is this same
/// version. No side effects.
pub fn install_fix(
    exe: &str,
    has_quarantine: bool,
    applications_copy_exists: bool,
    applications_is_this_version: bool,
) -> InstallFix {
    if cc::is_translocated(exe) {
        // The user already said "Open" in the macOS security prompt for this version, so removing
        // the mark from the same version in Applications is safe. A different (older) version
        // there, for example when the new download was opened straight from the .dmg: ask the
        // user to drag this one into Applications instead of starting the old one.
        return if applications_copy_exists && applications_is_this_version {
            InstallFix::RelaunchFromApplications
        } else {
            InstallFix::AskToMove
        };
    }
    if exe.starts_with(&format!("{APPLICATIONS_APP}/")) {
        return if has_quarantine { InstallFix::ClearQuarantine } else { InstallFix::Nothing };
    }
    if in_an_applications_folder(exe) {
        // Another copy in /Applications or ~/Applications: updates work there, leave it alone.
        return InstallFix::Nothing;
    }
    // Downloads, Desktop, the opened .dmg (/Volumes/...), ...
    InstallFix::AskToMove
}

fn in_an_applications_folder(exe: &str) -> bool {
    if exe.starts_with("/Applications/") {
        return true;
    }
    // ~/Applications/...: /Users/<name>/Applications/...
    let mut parts = exe.trim_start_matches('/').split('/');
    parts.next() == Some("Users") && parts.next().is_some() && parts.next() == Some("Applications")
}

/// Shown when PC Bridge isn't in Applications.
pub const MOVE_TO_APPLICATIONS_MSG: &str = "PC Bridge needs to be in your Applications folder.\n\n\
1. Quit PC Bridge (click the dot in the menu bar, then “Quit PC Bridge”).\n\
2. Open the file you downloaded (PC Bridge.dmg) and drag PC Bridge onto the Applications folder \
(if your Mac asks, choose “Replace”).\n\
3. Open PC Bridge from Applications.\n\n\
This lets PC Bridge update itself and lets Claude find it.";

/// Shown when PC Bridge couldn't move itself back to Applications.
pub const REOPEN_FROM_APPLICATIONS_MSG: &str =
    "PC Bridge couldn't finish setting itself up in your Applications folder.\n\n\
Please quit PC Bridge (menu bar dot → “Quit PC Bridge”) and open it again from Applications. \
If this message comes back, drag PC Bridge from Applications to the Trash, then drag it into \
Applications again from the file you downloaded (PC Bridge.dmg).";

/// `CFBundleShortVersionString` from an app's `Info.plist` (XML, as Tauri writes it).
pub fn plist_version(plist: &str) -> Option<String> {
    let after = &plist[plist.find("<key>CFBundleShortVersionString</key>")?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_string())
}

/// Whether the app in Applications is this same version (an unreadable one counts as different).
fn applications_is_this_version(bundle: &Path) -> bool {
    std::fs::read_to_string(bundle.join("Contents/Info.plist"))
        .ok()
        .and_then(|s| plist_version(&s))
        .is_some_and(|v| v == env!("CARGO_PKG_VERSION"))
}

/// Claude's connection and the helper commands may still point at the hidden temporary copy that
/// 0.1.2 ran from (it vanishes at the next restart): point them at this copy again.
fn repair_claude_paths() {
    let exe = exe_path();
    if !cc::is_translocated(&exe) {
        let _ = cc::repair_translocated(&user_home(), &home_dir().join("bin"), &exe, detect::SHIM_TOOLS);
    }
}

fn has_quarantine(bundle: &Path) -> bool {
    bundle.exists() && run("/usr/bin/xattr", &["-p", "com.apple.quarantine", &bundle.display().to_string()]).ok()
}

fn clear_quarantine(bundle: &Path) {
    let _ = run("/usr/bin/xattr", &["-dr", "com.apple.quarantine", &bundle.display().to_string()]);
}

fn ask_to_move(app: &tauri::AppHandle, msg: &str) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    app.dialog()
        .message(msg)
        .title("Move PC Bridge to Applications")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom("Open Applications folder".into(), "Later".into()))
        .show(|open| {
            if open {
                let _ = run("open", &["/Applications"]);
            }
        });
}

/// Mac release builds only: make sure PC Bridge runs from `/Applications/PC Bridge.app`.
/// Called first thing at startup. May quit this process (after starting the Applications copy).
pub fn heal_install_location(app: &tauri::AppHandle, minimized: bool) {
    if !cfg!(target_os = "macos") || cfg!(debug_assertions) {
        return;
    }
    let target = Path::new(APPLICATIONS_APP);
    let same = applications_is_this_version(target);
    match install_fix(&exe_path(), has_quarantine(target), target.exists(), same) {
        InstallFix::Nothing => repair_claude_paths(),
        InstallFix::ClearQuarantine => {
            clear_quarantine(target);
            repair_claude_paths();
        }
        InstallFix::RelaunchFromApplications => {
            clear_quarantine(target);
            if has_quarantine(target) {
                // Opening it again would land in the hidden copy again (and loop): explain instead.
                ask_to_move(app, REOPEN_FROM_APPLICATIONS_MSG);
                return;
            }
            // Wait a moment so this copy has quit before the Applications copy starts (only one
            // PC Bridge can run at a time).
            let script = if minimized {
                "sleep 1; exec /usr/bin/open -n \"$0\" --args --minimized"
            } else {
                "sleep 1; exec /usr/bin/open -n \"$0\""
            };
            if crate::util::command("/bin/sh").args(["-c", script, APPLICATIONS_APP]).spawn().is_ok() {
                std::process::exit(0);
            }
            ask_to_move(app, REOPEN_FROM_APPLICATIONS_MSG);
        }
        InstallFix::AskToMove => ask_to_move(app, MOVE_TO_APPLICATIONS_MSG),
    }
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

/// What to do so Tailscale stays connected (PC Bridge can't reach the PC without it).
#[derive(Debug, PartialEq)]
pub enum TailscaleFix {
    Nothing,
    /// The Tailscale app/service isn't answering: open it.
    OpenApp,
    /// Tailscale is open but disconnected ("Stopped"): connect again.
    Connect,
}

/// A signed-out Tailscale (`NeedsLogin`) is left alone: only the user can sign in.
pub fn tailscale_fix(backend_state: Option<&str>, unreadable: bool) -> TailscaleFix {
    if unreadable {
        return TailscaleFix::OpenApp;
    }
    match backend_state {
        Some("Stopped") => TailscaleFix::Connect,
        _ => TailscaleFix::Nothing,
    }
}

/// Keep Tailscale running and connected. Called every minute by the background watcher.
pub fn keep_tailscale_up() -> TailscaleFix {
    let Some(bin) = tailscale_bin() else {
        return TailscaleFix::Nothing;
    };
    let out = tailscale_cli(&bin, &["status", "--json"]);
    let v: Value = serde_json::from_str(&out.stdout).unwrap_or(Value::Null);
    let fix = tailscale_fix(v["BackendState"].as_str(), v.is_null());
    match fix {
        TailscaleFix::OpenApp => {
            if cfg!(target_os = "macos") {
                let _ = run("open", &["-g", "-a", "Tailscale"]);
            } else if cfg!(windows) {
                let gui = r"C:\Program Files\Tailscale\tailscale-ipn.exe";
                if Path::new(gui).exists() {
                    let _ = crate::util::command(gui).spawn();
                }
            }
        }
        TailscaleFix::Connect => {
            let _ = tailscale_cli(&bin, &["up"]);
        }
        TailscaleFix::Nothing => {}
    }
    fix
}

/// Windows: keep Tailscale connected even when nobody is signed in to Windows (best effort).
pub fn tailscale_unattended() {
    if cfg!(windows) {
        if let Some(bin) = tailscale_bin() {
            let _ = tailscale_cli(&bin, &["set", "--unattended=true"]);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_tailscale_connected_but_never_signs_in() {
        assert_eq!(tailscale_fix(None, true), TailscaleFix::OpenApp);
        assert_eq!(tailscale_fix(Some("Stopped"), false), TailscaleFix::Connect);
        assert_eq!(tailscale_fix(Some("Running"), false), TailscaleFix::Nothing);
        assert_eq!(tailscale_fix(Some("NeedsLogin"), false), TailscaleFix::Nothing);
        assert_eq!(tailscale_fix(Some("Starting"), false), TailscaleFix::Nothing);
    }

    const TRANSLOCATED: &str =
        "/private/var/folders/xy/abc123/T/AppTranslocation/0F1E2D3C-4B5A/d/PC Bridge.app/Contents/MacOS/pc-bridge";
    const INSTALLED: &str = "/Applications/PC Bridge.app/Contents/MacOS/pc-bridge";

    #[test]
    fn translocated_app_relaunches_from_applications() {
        // Quarantined Applications copy (the 0.1.2 "Open Anyway" case) and an already clean one.
        assert_eq!(install_fix(TRANSLOCATED, true, true, true), InstallFix::RelaunchFromApplications);
        assert_eq!(install_fix(TRANSLOCATED, false, true, true), InstallFix::RelaunchFromApplications);
    }

    #[test]
    fn translocated_app_without_applications_copy_asks_to_move() {
        assert_eq!(install_fix(TRANSLOCATED, false, false, false), InstallFix::AskToMove);
    }

    #[test]
    fn translocated_new_version_never_starts_an_older_copy_in_applications() {
        // 0.1.3 opened straight from the .dmg while 0.1.2 is in Applications.
        assert_eq!(install_fix(TRANSLOCATED, true, true, false), InstallFix::AskToMove);
        assert_eq!(install_fix(TRANSLOCATED, false, true, false), InstallFix::AskToMove);
    }

    #[test]
    fn reads_the_version_from_info_plist() {
        let plist = "<plist><dict><key>CFBundleName</key><string>PC Bridge</string>\n\
                     <key>CFBundleShortVersionString</key>\n  <string>0.1.3</string></dict></plist>";
        assert_eq!(plist_version(plist).as_deref(), Some("0.1.3"));
        assert_eq!(plist_version("<plist><dict></dict></plist>"), None);
    }

    #[test]
    fn applications_copy_with_download_mark_is_cleaned_in_place() {
        assert_eq!(install_fix(INSTALLED, true, true, true), InstallFix::ClearQuarantine);
        assert_eq!(install_fix(INSTALLED, false, true, true), InstallFix::Nothing);
    }

    #[test]
    fn other_applications_folders_are_left_alone() {
        assert_eq!(
            install_fix("/Users/sam/Applications/PC Bridge.app/Contents/MacOS/pc-bridge", true, false, false),
            InstallFix::Nothing
        );
        assert_eq!(
            install_fix("/Applications/Utilities/PC Bridge.app/Contents/MacOS/pc-bridge", false, false, false),
            InstallFix::Nothing
        );
    }

    #[test]
    fn running_from_downloads_or_the_dmg_asks_to_move() {
        for exe in [
            "/Users/sam/Downloads/PC Bridge.app/Contents/MacOS/pc-bridge",
            "/Volumes/PC Bridge/PC Bridge.app/Contents/MacOS/pc-bridge",
            "/Users/sam/Downloads/Applications/PC Bridge.app/Contents/MacOS/pc-bridge",
        ] {
            assert_eq!(install_fix(exe, false, false, false), InstallFix::AskToMove, "{exe}");
        }
        // A look-alike folder name must not count as /Applications/PC Bridge.app.
        assert_eq!(
            install_fix("/Applications/PC Bridge.app.old/Contents/MacOS/pc-bridge", true, true, true),
            InstallFix::Nothing
        );
    }
}
