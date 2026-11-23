//! Commands used by the Windows app: dependency installs, agent + Syncthing services, Wake-on-LAN,
//! power settings, pairing code. Compiled everywhere (so CI checks it), but most commands only do
//! real work on Windows.

use crate::mac::{check, keep_tailscale_up, tailscale_status, tailscale_unattended};
use crate::state::AppState;
use crate::util::{blocking, command, powershell, random_key, run, which};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{Manager, State};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WinState {
    pub root: String,
    pub wizard_step: u32,
    pub setup_complete: bool,
    pub syncthing_api_key: String,
    pub mac_address: String,
    pub want_ollama: bool,
    pub want_comfy: bool,
    pub want_pytorch: bool,
    pub use_wsl: bool,
}

fn state_path() -> PathBuf {
    bridge_core::config::home_dir().join("windows.json")
}

pub fn load_state() -> WinState {
    let mut s: WinState =
        std::fs::read_to_string(state_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if s.root.is_empty() {
        s.root = if Path::new("D:\\").exists() { r"D:\PCBridge".into() } else { r"C:\PCBridge".into() };
    }
    s
}

fn save_state(s: &WinState) -> Result<(), String> {
    let p = state_path();
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(p, serde_json::to_string_pretty(s).unwrap()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn win_get_state() -> WinState {
    load_state()
}

#[tauri::command]
pub fn win_save_state(state: WinState) -> Result<(), String> {
    save_state(&state)
}

fn root() -> PathBuf {
    PathBuf::from(load_state().root)
}

/// Merge keys into PCBridge\config\settings.json (read by the agent at start).
fn update_agent_settings(patch: Value) -> Result<(), String> {
    let p = root().join("config").join("settings.json");
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut v: Value =
        std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    if let (Some(obj), Some(add)) = (v.as_object_mut(), patch.as_object()) {
        for (k, val) in add {
            obj.insert(k.clone(), val.clone());
        }
    }
    std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).map_err(|e| e.to_string())
}

/// After installing something, pick up the new PATH from the registry for this process.
fn refresh_path() {
    if !cfg!(windows) {
        return;
    }
    let o = powershell(
        "[Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User')",
    );
    if o.ok() && !o.stdout.trim().is_empty() {
        std::env::set_var("PATH", o.stdout.trim());
    }
}

// ---------------------------------------------------------------- Claude Code location

/// Claude Code's native Windows installer puts `claude.exe` in `%USERPROFILE%\.local\bin`,
/// which is often not on PATH. Find it either way.
pub fn claude_exe() -> Option<String> {
    which("claude").or_else(|| {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        let dir = PathBuf::from(home).join(".local").join("bin");
        ["claude.exe", "claude"].iter().map(|n| dir.join(n)).find(|p| p.is_file()).map(|p| p.display().to_string())
    })
}

/// PowerShell that appends `dir` to the *user* PATH if missing (no admin), keeping %VARS% intact,
/// and tells Windows so new windows see it.
fn add_to_user_path_script(dir: &str) -> String {
    let d = dir.replace('\'', "''");
    format!(
        "$d='{d}'; $k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment',$true); \
         $p=[string]$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames); \
         if (-not (($p -split ';') -contains $d)) {{ \
           $n = if ([string]::IsNullOrWhiteSpace($p)) {{ $d }} else {{ $p.TrimEnd(';') + ';' + $d }}; \
           $k.SetValue('Path',$n,[Microsoft.Win32.RegistryValueKind]::ExpandString) }}; $k.Close(); \
         [Environment]::SetEnvironmentVariable('PCBRIDGE_PATH_REFRESH','1','User'); \
         [Environment]::SetEnvironmentVariable('PCBRIDGE_PATH_REFRESH',$null,'User')"
    )
}

/// Make sure Claude Code is usable: put its folder on the user PATH and tell the agent its full path.
fn ensure_claude_ready() -> Option<String> {
    let exe = claude_exe()?;
    if cfg!(windows) {
        if let Some(dir) = Path::new(&exe).parent() {
            if which("claude").is_none() {
                powershell(&add_to_user_path_script(&dir.display().to_string()));
                refresh_path();
            }
        }
        let _ = update_agent_settings(json!({ "claude_cmd": exe }));
    }
    Some(exe)
}

// ---------------------------------------------------------------- GPU

pub fn parse_smi(out: &str) -> Option<(String, String, f64)> {
    let line = out.lines().next()?;
    let p: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if p.len() < 3 {
        return None;
    }
    Some((p[0].to_string(), p[1].to_string(), p[2].parse().unwrap_or(0.0)))
}

#[tauri::command]
pub async fn win_check_gpu() -> Result<Value, String> {
    blocking(|| {
        let o = run("nvidia-smi", &["--query-gpu=name,driver_version,memory.total", "--format=csv,noheader,nounits"]);
        match parse_smi(&o.stdout).filter(|_| o.ok()) {
            Some((name, driver, vram)) => {
                let major: f64 = driver.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
                Ok(json!({"found": true, "name": name, "driver": driver, "vram_mb": vram, "driver_ok": major >= 550.0}))
            }
            None => Ok(json!({"found": false})),
        }
    })
    .await
}

// ---------------------------------------------------------------- Installs

/// (id, winget package, check command)
const PACKAGES: &[(&str, &str, &str)] = &[
    ("tailscale", "Tailscale.Tailscale", "tailscale"),
    ("ffmpeg", "Gyan.FFmpeg", "ffmpeg"),
    ("python", "Python.Python.3.12", "python"),
    ("syncthing", "Syncthing.Syncthing", "syncthing"),
    ("ollama", "Ollama.Ollama", "ollama"),
];

fn tool_present(id: &str) -> (bool, String) {
    match id {
        "tailscale" => (crate::mac::tailscale_bin().is_some(), String::new()),
        "ffmpeg" => {
            let o = run("ffmpeg", &["-hide_banner", "-encoders"]);
            let nvenc = o.stdout.contains("h264_nvenc");
            (o.ok(), if o.ok() && !nvenc { "installed, but without the NVIDIA encoder".into() } else { String::new() })
        }
        "python" => {
            let o = run("python", &["--version"]);
            // The Microsoft Store stub prints nothing and fails.
            (o.ok() && o.stdout.contains("Python 3"), o.stdout.trim().to_string())
        }
        "claude" => match ensure_claude_ready() {
            Some(exe) => {
                let o = run(&exe, &["--version"]);
                (o.ok(), o.stdout.trim().to_string())
            }
            None => (false, String::new()),
        },
        other => {
            let check = PACKAGES.iter().find(|p| p.0 == other).map(|p| p.2).unwrap_or(other);
            (which(check).is_some(), String::new())
        }
    }
}

#[tauri::command]
pub async fn win_check_tool(id: String) -> Result<Value, String> {
    blocking(move || {
        refresh_path();
        let (ok, detail) = tool_present(&id);
        Ok(json!({"id": id, "installed": ok, "detail": detail}))
    })
    .await
}

#[tauri::command]
pub async fn win_install(id: String) -> Result<Value, String> {
    blocking(move || {
        if !cfg!(windows) {
            return Err("Installing is only available on the Windows PC".into());
        }
        let log = if id == "claude" {
            // Official native installer for Claude Code on Windows.
            powershell("irm https://claude.ai/install.ps1 | iex")
        } else {
            let pkg = PACKAGES.iter().find(|p| p.0 == id).ok_or(format!("Unknown item: {id}"))?.1;
            run(
                "winget",
                &["install", "--id", pkg, "-e", "--silent", "--accept-package-agreements", "--accept-source-agreements"],
            )
        };
        refresh_path();
        let (ok, detail) = tool_present(&id);
        if !ok {
            let combined = log.stdout.clone() + &log.stderr;
            let tail: Vec<&str> = combined.lines().rev().take(8).collect();
            return Err(format!(
                "The install didn't finish. {}\nDetails:\n{}",
                if which("winget").is_none() {
                    "“winget” (App Installer) is missing — install “App Installer” from the Microsoft Store, then try again."
                } else {
                    ""
                },
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            ));
        }
        Ok(json!({"id": id, "installed": true, "detail": detail}))
    })
    .await
}

#[tauri::command]
pub fn win_open_tailscale() -> Result<(), String> {
    let gui = r"C:\Program Files\Tailscale\tailscale-ipn.exe";
    command(gui).spawn().map(|_| ()).map_err(|e| format!("Couldn't open Tailscale: {e}"))
}

#[tauri::command]
pub async fn win_tailscale_check() -> Result<Value, String> {
    blocking(|| Ok(tailscale_status())).await
}

/// Open a visible console running `claude` so the user can log in in their browser.
#[tauri::command]
pub fn win_claude_login() -> Result<(), String> {
    let exe = ensure_claude_ready().ok_or("Claude Code isn't installed yet. Click “Do it for me” first.")?;
    // Run `claude` by name with its folder on PATH: avoids quoting problems with spaces in user names.
    let dir = Path::new(&exe).parent().map(|d| d.display().to_string()).unwrap_or_default();
    let path = format!("{dir};{}", std::env::var("PATH").unwrap_or_default());
    std::process::Command::new("cmd")
        .env("PATH", path)
        .args(["/c", "start", "Claude login", "cmd", "/k", "claude"])
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn win_claude_check() -> Result<Value, String> {
    blocking(|| {
        refresh_path();
        let Some(exe) = ensure_claude_ready() else {
            return Ok(json!({"installed": false, "logged_in": false, "detail": ""}));
        };
        let o = run(&exe, &["-p", "Reply with just the word: ready", "--output-format", "text"]);
        let ready = o.ok() && o.stdout.to_lowercase().contains("ready");
        Ok(json!({"installed": true, "logged_in": ready, "detail": o.stderr.lines().next().unwrap_or("")}))
    })
    .await
}

// ---------------------------------------------------------------- Agent + Syncthing services

fn venv_python() -> PathBuf {
    root().join("config").join("venv").join("Scripts").join("python.exe")
}

/// The helper has been installed (wizard step 4 ran at least once).
fn agent_installed() -> bool {
    venv_python().exists()
}

#[tauri::command]
pub async fn win_setup_agent(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let state = state.inner().clone();
    let resources = app.path().resource_dir().map_err(|e| e.to_string())?.join("agent");
    blocking(move || {
        let mut ws = load_state();
        let r = PathBuf::from(&ws.root);
        for sub in ["workspace", "jobs", "worker", "cache", "models", "logs", "config"] {
            std::fs::create_dir_all(r.join(sub)).map_err(|e| format!("Couldn't create {}: {e}", r.display()))?;
        }
        stop_agent(&state);
        install_agent_files(&resources, &r)?;
        if ws.syncthing_api_key.is_empty() {
            ws.syncthing_api_key = random_key(32);
        }
        save_state(&ws)?;
        let mut patch = json!({"syncthing_api_key": ws.syncthing_api_key});
        if !ws.mac_address.is_empty() {
            patch["mac_address"] = json!(ws.mac_address);
        }
        update_agent_settings(patch)?;
        start_services(&state)?;
        // Wait for the agent to answer.
        for _ in 0..30 {
            if local_client().and_then(|c| c.health().ok()).is_some() {
                return Ok(json!({"ok": true, "root": ws.root}));
            }
            std::thread::sleep(Duration::from_millis(700));
        }
        Err("The agent was installed but didn't start. Check PCBridge\\logs\\agent.log".into())
    })
    .await
}

/// `__version__` from a `pcbridge_agent/__init__.py`.
pub fn parse_agent_version(init_py: &str) -> Option<String> {
    init_py.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("__version__")?.trim_start().strip_prefix('=')?.trim();
        let v = rest.trim_matches(|c| c == '"' || c == '\'');
        (!v.is_empty()).then(|| v.to_string())
    })
}

fn agent_stamp() -> PathBuf {
    root().join("config").join("agent-version")
}

/// Copy the bundled agent source into the PCBridge folder and install it into the private venv.
fn install_agent_files(resources: &Path, r: &Path) -> Result<(), String> {
    let src = r.join("config").join("agent-src");
    let _ = std::fs::remove_dir_all(&src);
    copy_dir(resources, &src)?;
    refresh_path();
    if !venv_python().exists() {
        let o = run("python", &["-m", "venv", &r.join("config").join("venv").display().to_string()]);
        if !o.ok() {
            return Err(format!("Couldn't prepare Python: {}", o.stderr));
        }
    }
    let py = venv_python().display().to_string();
    let o = run(
        &py,
        &["-m", "pip", "install", "--disable-pip-version-check", "-q", "--upgrade", &src.display().to_string()],
    );
    if !o.ok() {
        return Err(format!("Couldn't install the PC Bridge agent: {}", o.stderr.lines().last().unwrap_or("")));
    }
    if let Some(v) = std::fs::read_to_string(resources.join("pcbridge_agent").join("__init__.py"))
        .ok()
        .and_then(|t| parse_agent_version(&t))
    {
        let _ = std::fs::write(r.join("config").join("agent-version"), v);
    }
    Ok(())
}

/// Version the agent on this PC answers with, if one is running.
fn running_agent_version() -> Option<String> {
    let c = bridge_core::client::AgentClient::new("http://127.0.0.1:8765", None);
    c.health().ok()?["version"].as_str().map(str::to_string)
}

/// Stops whichever PC Bridge agent is listening on port 8765, including one left running by an
/// older copy of the app (an update replaces the app but its helper process keeps running).
const STOP_AGENT_PS: &str = r#"
$c = Get-NetTCPConnection -LocalPort 8765 -State Listen -ErrorAction SilentlyContinue
foreach ($id in ($c.OwningProcess | Sort-Object -Unique)) {
  $p = Get-Process -Id $id -ErrorAction SilentlyContinue
  if ($p -and $p.ProcessName -like 'python*') { Stop-Process -Id $id -Force }
}
"#;

fn stop_agent(state: &AppState) {
    if let Some(mut c) = state.procs.lock().unwrap().agent.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    if cfg!(windows) {
        let _ = powershell(STOP_AGENT_PS);
        for _ in 0..20 {
            if running_agent_version().is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

/// What to do at start-up about the installed agent.
#[derive(Debug, PartialEq)]
pub enum AgentAction {
    Nothing,
    Reinstall,
}

pub fn agent_action(bundled: &str, running: Option<&str>, stamp: Option<&str>) -> AgentAction {
    match running.or(stamp) {
        Some(v) if v == bundled => AgentAction::Nothing,
        _ => AgentAction::Reinstall,
    }
}

/// After an app update: if the helper is older than the one bundled with this app, stop it,
/// install the new one and start it again. Runs once at start-up, before the watchdog.
pub fn ensure_agent_current(app: &tauri::AppHandle, state: &AppState) -> Result<bool, String> {
    if !cfg!(windows) || !agent_installed() {
        return Ok(false);
    }
    let resources = app.path().resource_dir().map_err(|e| e.to_string())?.join("agent");
    let bundled = std::fs::read_to_string(resources.join("pcbridge_agent").join("__init__.py"))
        .ok()
        .and_then(|t| parse_agent_version(&t))
        .ok_or("The bundled helper is missing")?;
    let running = running_agent_version();
    let stamp = std::fs::read_to_string(agent_stamp()).ok().map(|s| s.trim().to_string());
    if agent_action(&bundled, running.as_deref(), stamp.as_deref()) == AgentAction::Nothing {
        return Ok(false);
    }
    stop_agent(state);
    install_agent_files(&resources, &root())?;
    start_services(state)?;
    Ok(true)
}

/// Settings → "Restart the helper": stop it and start it again (also picks up new programs).
#[tauri::command]
pub async fn win_restart_helper(state: State<'_, AppState>) -> Result<Value, String> {
    let state = state.inner().clone();
    blocking(move || {
        stop_agent(&state);
        refresh_path();
        start_services(&state)?;
        for _ in 0..30 {
            if let Some(v) = running_agent_version() {
                return Ok(json!({"ok": true, "version": v}));
            }
            std::thread::sleep(Duration::from_millis(700));
        }
        Err("The helper didn't start again. Check PCBridge\\logs\\agent.log".into())
    })
    .await
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("Missing bundled files ({}): {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Where Syncthing is, looked up with `where.exe` at most once every 10 minutes. The watchdog calls
/// this every 15 s; running `where.exe` that often meant an error window every 15 s whenever Windows
/// was short of memory and couldn't start it (0xc0000017).
fn syncthing_bin() -> Option<String> {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<(Option<String>, Instant)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    if let Some((found, at)) = cache.as_ref() {
        let still_there = found.as_deref().map(|p| Path::new(p).is_file()).unwrap_or(false);
        if still_there || (found.is_none() && at.elapsed() < Duration::from_secs(600)) {
            return found.clone();
        }
    }
    let found = which("syncthing");
    *cache = Some((found.clone(), Instant::now()));
    found
}

/// Something already answers on this local address (the agent on 8765, Syncthing on 8385).
fn listening(addr: &str) -> bool {
    addr.parse().map(|a| std::net::TcpStream::connect_timeout(&a, Duration::from_millis(500)).is_ok()).unwrap_or(false)
}

/// Start (or restart) the agent and Syncthing as hidden background processes.
pub fn start_services(state: &AppState) -> Result<(), String> {
    let ws = load_state();
    let mut procs = state.procs.lock().unwrap();
    let alive =
        |c: &mut Option<std::process::Child>| c.as_mut().map(|c| matches!(c.try_wait(), Ok(None))).unwrap_or(false);
    // A helper this copy of the app didn't start (an older copy's, left running across an update)
    // is still a running helper: starting a second one would only fail and be retried every 15 s.
    if !alive(&mut procs.agent) && venv_python().exists() && !listening("127.0.0.1:8765") {
        refresh_path();
        let child = command(&venv_python().display().to_string())
            .args(["-m", "pcbridge_agent", "--root", &ws.root])
            .spawn()
            .map_err(|e| format!("Couldn't start the agent: {e}"))?;
        procs.agent = Some(child);
    }
    if !alive(&mut procs.syncthing) && !ws.syncthing_api_key.is_empty() && !listening(bridge_core::syncthing::API_ADDR)
    {
        if let Some(bin) = syncthing_bin() {
            let home = PathBuf::from(&ws.root).join("config").join("syncthing");
            let _ = std::fs::create_dir_all(&home);
            let child = command(&bin)
                .arg("serve")
                .arg("--no-browser")
                .arg(format!("--home={}", home.display()))
                .arg(format!("--gui-address={}", bridge_core::syncthing::API_ADDR))
                .arg(format!("--gui-apikey={}", ws.syncthing_api_key))
                .spawn()
                .map_err(|e| format!("Couldn't start Syncthing: {e}"))?;
            procs.syncthing = Some(child);
        }
    }
    Ok(())
}

/// How many helpers this copy of the app started and are still running (0–2).
fn running_children(state: &AppState) -> u8 {
    let mut guard = state.procs.lock().unwrap();
    let procs = &mut *guard;
    let mut n = 0;
    for c in [&mut procs.agent, &mut procs.syncthing] {
        if c.as_mut().map(|c| matches!(c.try_wait(), Ok(None))).unwrap_or(false) {
            n += 1;
        }
    }
    n
}

/// Programs this app starts inherit its error mode. With these flags, a program Windows can't start
/// (for example when memory runs out) fails quietly instead of opening an error window.
#[cfg(windows)]
fn quiet_child_errors() {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetErrorMode(mode: u32) -> u32;
    }
    const SEM_FAILCRITICALERRORS: u32 = 0x0001;
    const SEM_NOGPFAULTERRORBOX: u32 = 0x0002;
    const SEM_NOOPENFILEERRORBOX: u32 = 0x8000;
    // SAFETY: SetErrorMode only changes this process's error-mode flags.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    }
}

/// Background watchdog: keep services running (Windows only, after setup).
pub fn spawn_watchdog(app: tauri::AppHandle, state: AppState) {
    std::thread::spawn(move || {
        if !cfg!(windows) {
            return;
        }
        #[cfg(windows)]
        quiet_child_errors();
        if let Err(e) = ensure_agent_current(&app, &state) {
            eprintln!("PC Bridge: couldn't update the helper: {e}");
        }
        tailscale_unattended();
        let mut tick: u64 = 0;
        // Wait longer between restarts while a helper keeps dying (15 s, 30 s, 1 min … up to 10 min),
        // so a helper that can't start never turns into a stream of processes and error windows.
        let mut wait_ticks: u64 = 1;
        let mut next_try: u64 = 0;
        loop {
            // Start the helper whenever it's installed, even if the wizard was never closed with Finish.
            if agent_installed() && tick >= next_try {
                let before = running_children(&state);
                let _ = start_services(&state);
                if running_children(&state) > before {
                    next_try = tick + wait_ticks;
                    wait_ticks = (wait_ticks * 2).min(40);
                } else if before == 2 {
                    wait_ticks = 1;
                }
            }
            if tick.is_multiple_of(4) {
                let _ = keep_tailscale_up();
            }
            tick += 1;
            std::thread::sleep(Duration::from_secs(15));
        }
    });
}

fn local_token() -> Option<String> {
    std::fs::read_to_string(root().join("config").join("local_token")).ok().map(|s| s.trim().to_string())
}

pub fn local_client() -> Option<bridge_core::client::AgentClient> {
    Some(bridge_core::client::AgentClient::new("http://127.0.0.1:8765", Some(local_token()?)))
}

#[tauri::command]
pub async fn win_pairing_code(fresh: bool) -> Result<Value, String> {
    blocking(move || {
        let c = local_client().ok_or("The agent isn't installed yet")?;
        let v = if fresh { c.post_json("/pair/new", &json!({})) } else { c.get_json("/pair/code", 8) };
        let code = v.map_err(|e| e.to_string())?["code"].as_str().unwrap_or("").to_string();
        let ts = tailscale_status();
        Ok(json!({"code": code, "pc_name": ts["self_name"], "pc_ip": ts["self_ip"]}))
    })
    .await
}

/// Macs paired with this PC (name, when, and a number to remove one).
#[tauri::command]
pub async fn win_paired_macs() -> Result<Value, String> {
    blocking(|| {
        let c = local_client().ok_or(HELPER_MISSING)?;
        c.get_json("/pair/devices", 8).map_err(friendly_error)
    })
    .await
}

/// Remove a paired Mac: its key stops working right away.
#[tauri::command]
pub async fn win_remove_mac(index: u32) -> Result<Value, String> {
    blocking(move || {
        let c = local_client().ok_or(HELPER_MISSING)?;
        c.delete_json(&format!("/pair/devices/{index}")).map_err(friendly_error)
    })
    .await
}

// ---------------------------------------------------------------- Programs (installed by the helper)

const HELPER_MISSING: &str =
    "The PC Bridge helper isn't installed yet. Finish the setup's “Install the tools” step first.";

/// Turn a client error into one plain sentence for the Programs card.
fn friendly_error(e: bridge_core::client::ClientError) -> String {
    use bridge_core::client::ClientError;
    match e {
        ClientError::Unreachable(_) => {
            "The PC Bridge helper isn't running. Click “Restart the helper” in Settings, then try again.".into()
        }
        ClientError::Unauthorized => {
            "The helper didn't accept this app's key. Click “Restart the helper” in Settings, then try again.".into()
        }
        // An older helper has no /programs page yet.
        ClientError::Refused(d) if d == "Not Found" => {
            "The helper on this PC is out of date. Click “Restart the helper” in Settings to update it.".into()
        }
        ClientError::Refused(d) => d,
        ClientError::Other(m) => error_detail(&m).to_string(),
    }
}

/// "HTTP 507 Insufficient Storage: Not enough space…" -> "Not enough space…"
pub fn error_detail(msg: &str) -> &str {
    match msg.strip_prefix("HTTP ").and_then(|rest| rest.split_once(": ")) {
        Some((_, detail)) if !detail.trim().is_empty() => detail,
        _ => msg,
    }
}

/// winget exit codes that mean "it's already there" or "installed, restart to finish": a success.
/// (The helper already turns these into 0; this covers an older helper.)
pub fn install_succeeded(code: i64) -> bool {
    // 0x8A15002B: no newer version to upgrade to; 0x8A150061: already installed;
    // 0x8A150109: installed, the PC must restart to finish.
    matches!(code as u32, 0 | 0x8A15_002B | 0x8A15_0061 | 0x8A15_0109)
}

/// The `{ok, restart_needed, exit_code, log_tail}` the UI shows, from a finished install
/// (`exit_code`, `log` and `restart_needed`).
pub fn install_result(job: &Value) -> Value {
    let code = job["exit_code"].as_i64().unwrap_or(-1);
    let log = job["log"].as_str().unwrap_or("");
    let lines: Vec<&str> = log.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(12)..].join("\n");
    let restart = job["restart_needed"] == true || code as u32 == 0x8A15_0109;
    json!({"ok": install_succeeded(code), "restart_needed": restart, "exit_code": code, "log_tail": tail})
}

/// What the helper can install, which are already there, and the free space on the C: drive.
#[tauri::command]
pub async fn win_programs() -> Result<Value, String> {
    blocking(|| {
        let c = local_client().ok_or(HELPER_MISSING)?;
        c.get_json("/programs", 30).map_err(friendly_error)
    })
    .await
}

/// Install one program from the helper's list (winget or pip, never an arbitrary command).
#[tauri::command]
pub async fn win_install_program(key: String) -> Result<Value, String> {
    blocking(move || {
        let c = local_client().ok_or(HELPER_MISSING)?;
        // Follow the install's own output until it ends, however long it takes. If the same
        // program is already being installed (Claude asked too), the helper streams that one.
        let mut log = String::new();
        let o = c
            .install_program(&key, |ev| {
                if let Some(d) = ev["data"].as_str() {
                    log.push_str(d);
                }
            })
            .map_err(friendly_error)?;
        Ok(install_result(&json!({"exit_code": o.exit_code, "log": log, "restart_needed": o.restart_needed})))
    })
    .await
}

// ---------------------------------------------------------------- Wake-on-LAN & power

const WOL_SCRIPT: &str = r#"
$out = $args[0]
$a = Get-NetAdapter -Physical | Where-Object { $_.Status -eq 'Up' -and $_.MediaType -eq '802.3' } | Select-Object -First 1
if (-not $a) { @{ ok = $false; error = 'No wired (Ethernet) network adapter is connected.' } | ConvertTo-Json | Set-Content -Path $out; exit }
try { Set-NetAdapterPowerManagement -Name $a.Name -WakeOnMagicPacket Enabled -ErrorAction Stop } catch {}
try { Set-NetAdapterAdvancedProperty -Name $a.Name -RegistryKeyword '*WakeOnMagicPacket' -RegistryValue 1 -ErrorAction Stop } catch {}
try { powercfg /deviceenablewake "$($a.InterfaceDescription)" | Out-Null } catch {}
# Windows "Fast startup" blocks waking from shut down; turn it off.
reg add "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Power" /v HiberbootEnabled /t REG_DWORD /d 0 /f | Out-Null
@{ ok = $true; adapter = $a.Name; mac = $a.MacAddress } | ConvertTo-Json | Set-Content -Path $out
"#;

#[tauri::command]
pub async fn win_configure_wol() -> Result<Value, String> {
    blocking(|| {
        if !cfg!(windows) {
            return Err("Only available on the Windows PC".into());
        }
        let dir = root().join("config");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let script = dir.join("wol.ps1");
        let out = dir.join("wol.json");
        let _ = std::fs::remove_file(&out);
        std::fs::write(&script, WOL_SCRIPT).map_err(|e| e.to_string())?;
        // Runs as administrator: Windows shows a "Do you want to allow…" prompt.
        let elevate = format!(
            "Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','\"{}\"','\"{}\"'",
            script.display(),
            out.display()
        );
        powershell(&elevate);
        let v: Value = std::fs::read_to_string(&out)
            .ok()
            .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
            .ok_or("The settings change was cancelled (you need to click “Yes” on the Windows prompt).")?;
        if v["ok"] != true {
            return Err(v["error"].as_str().unwrap_or("Couldn't configure Wake-on-LAN").to_string());
        }
        let mac = v["mac"].as_str().unwrap_or("").replace('-', ":");
        let mut ws = load_state();
        ws.mac_address = mac.clone();
        save_state(&ws)?;
        update_agent_settings(json!({"mac_address": mac}))?;
        Ok(json!({"adapter": v["adapter"], "mac": mac}))
    })
    .await
}

#[tauri::command]
pub async fn win_power() -> Result<Value, String> {
    blocking(|| {
        if !cfg!(windows) {
            return Err("Only available on the Windows PC".into());
        }
        // Sleep after 30 idle minutes on mains power; allow wake timers. The agent keeps the PC
        // awake while a job is running (SetThreadExecutionState) and lets it sleep afterwards.
        let cmds: [&[&str]; 3] = [
            &["/change", "standby-timeout-ac", "30"],
            &["/setacvalueindex", "SCHEME_CURRENT", "SUB_SLEEP", "RTCWAKE", "1"],
            &["/setactive", "SCHEME_CURRENT"],
        ];
        for c in cmds {
            let o = run("powercfg", c);
            if !o.ok() {
                return Err(format!("powercfg failed: {}", o.stderr.trim()));
            }
        }
        Ok(json!({"sleep_after_minutes": 30}))
    })
    .await
}

#[tauri::command]
pub fn win_set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let al = app.autolaunch();
    if enabled { al.enable() } else { al.disable() }.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- Status & diagnostics

#[tauri::command]
pub async fn win_status() -> Result<Value, String> {
    blocking(|| {
        let ws = load_state();
        let c = local_client();
        let c = c.as_ref();
        // All four at once: one after another, a slow one (nvidia-smi while the GPU is busy) made
        // the whole page wait.
        let (status, gpu, jobs, tailscale) = std::thread::scope(|s| {
            let status = s.spawn(|| c.and_then(|c| c.status().ok()));
            let gpu = s.spawn(|| c.and_then(|c| c.gpu().ok()));
            let jobs = s.spawn(|| c.and_then(|c| c.jobs().ok()));
            let tailscale = s.spawn(tailscale_status);
            (
                status.join().ok().flatten(),
                gpu.join().ok().flatten(),
                jobs.join().ok().flatten(),
                tailscale.join().unwrap_or(Value::Null),
            )
        });
        Ok(json!({"agent_running": status.is_some(), "status": status, "gpu": gpu, "jobs": jobs, "root": ws.root,
                  "tailscale": tailscale}))
    })
    .await
}

/// Just the PC's job list, for the Jobs page (no GPU, Tailscale or status checks to wait for).
#[tauri::command]
pub async fn win_jobs() -> Result<Value, String> {
    blocking(|| {
        let c = local_client().ok_or("PC Bridge helper isn't set up yet")?;
        c.jobs().map_err(friendly_error)
    })
    .await
}

#[tauri::command]
pub async fn run_diagnostics_win(app: tauri::AppHandle) -> Result<Vec<Value>, String> {
    let autostart = {
        use tauri_plugin_autostart::ManagerExt;
        app.autolaunch().is_enabled().unwrap_or(false)
    };
    blocking(move || {
        refresh_path();
        let ws = load_state();
        let mut out = vec![];
        let smi = run("nvidia-smi", &["--query-gpu=name,driver_version,memory.total", "--format=csv,noheader,nounits"]);
        out.push(check(
            "NVIDIA graphics card",
            smi.ok(),
            smi.stdout.trim().to_string(),
            "Install the latest NVIDIA driver (Settings → “Show a pairing code / run setup again” → “Check your graphics card”).",
        ));
        let ts = tailscale_status();
        out.push(check(
            "Tailscale connected",
            ts["running"] == true,
            "",
            "Open Tailscale from the Start menu and log in with the same account as your Mac.",
        ));
        for (id, label) in
            [("ffmpeg", "FFmpeg"), ("python", "Python"), ("syncthing", "Syncthing"), ("claude", "Claude Code")]
        {
            let (ok, detail) = tool_present(id);
            out.push(check(label, ok, detail, "Open Setup and run the install step again."));
        }
        let agent = local_client().and_then(|c| c.status().ok());
        out.push(check(
            "PC Bridge agent running",
            agent.is_some(),
            ws.root.clone(),
            "Restart PC Bridge. If it still fails, open PCBridge\\logs\\agent.log.",
        ));
        out.push(check(
            "Folder syncing running",
            bridge_core::syncthing::Syncthing::new(ws.syncthing_api_key.clone()).alive(),
            "",
            "Restart PC Bridge. If that doesn't help: Settings → “Show a pairing code / run setup again” → “Install the tools”.",
        ));
        out.push(check(
            "Wake-on-LAN set up",
            !ws.mac_address.is_empty(),
            ws.mac_address.clone(),
            "Settings → “Show a pairing code / run setup again” → “Wake-on-LAN” → “Do it for me”.",
        ));
        out.push(check("Starts with Windows", autostart, "", "Settings → Startup → turn on “Start PC Bridge with Windows”."));
        Ok(out)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_agent_version_from_init_py() {
        let init = "\"\"\"PC Bridge agent.\"\"\"\n\n__version__ = \"0.1.2\"\n";
        assert_eq!(parse_agent_version(init).as_deref(), Some("0.1.2"));
        assert_eq!(parse_agent_version("__version__='1.0'").as_deref(), Some("1.0"));
        assert_eq!(parse_agent_version("nothing here"), None);
    }

    #[test]
    fn reinstalls_the_helper_only_when_it_is_out_of_date() {
        // An old helper is still running after an app update.
        assert_eq!(agent_action("0.1.2", Some("0.1.0"), Some("0.1.0")), AgentAction::Reinstall);
        // Up to date and running.
        assert_eq!(agent_action("0.1.2", Some("0.1.2"), None), AgentAction::Nothing);
        // Not running yet, but the installed copy is current: let the watchdog just start it.
        assert_eq!(agent_action("0.1.2", None, Some("0.1.2")), AgentAction::Nothing);
        // Not running and never stamped (installed by 0.1.1 or older).
        assert_eq!(agent_action("0.1.2", None, None), AgentAction::Reinstall);
    }

    #[test]
    fn install_errors_keep_only_the_helpers_sentence() {
        assert_eq!(
            error_detail("HTTP 507 Insufficient Storage: Not enough space on the PC's C:\\ drive"),
            "Not enough space on the PC's C:\\ drive"
        );
        assert_eq!(error_detail("HTTP 503 Service Unavailable: "), "HTTP 503 Service Unavailable: ");
        assert_eq!(error_detail("something else"), "something else");
        use bridge_core::client::ClientError;
        assert!(friendly_error(ClientError::Unreachable("refused".into())).contains("Restart the helper"));
        assert!(friendly_error(ClientError::Refused("Not Found".into())).contains("out of date"));
        assert_eq!(friendly_error(ClientError::Refused("PC Bridge doesn't know".into())), "PC Bridge doesn't know");
    }

    #[test]
    fn install_result_reports_success_and_the_last_log_lines() {
        let log: String = (1..=20).map(|i| format!("line {i}\r\n\n")).collect();
        let r = install_result(&json!({"exit_code": 0, "log": log}));
        assert_eq!(r["ok"], true);
        let tail = r["log_tail"].as_str().unwrap();
        assert_eq!(tail.lines().count(), 12);
        assert!(tail.starts_with("line 9") && tail.ends_with("line 20"), "{tail}");
        assert_eq!(install_result(&json!({"exit_code": 1, "log": ""}))["ok"], false);
        assert_eq!(install_result(&json!({"exit_code": null}))["exit_code"], -1);
        // winget "already installed / nothing to upgrade", signed or unsigned.
        assert!(install_succeeded(-1978335189) && install_succeeded(0x8A15_002B) && install_succeeded(0x8A15_0061));
        // Installed, restart needed: a success the card reports as "restart the PC".
        let r = install_result(&json!({"exit_code": 0, "log": "", "restart_needed": true}));
        assert_eq!((r["ok"].clone(), r["restart_needed"].clone()), (json!(true), json!(true)));
        assert_eq!(install_result(&json!({"exit_code": 0x8A15_0109u32}))["restart_needed"], true);
        assert_eq!(install_result(&json!({"exit_code": 0}))["restart_needed"], false);
        assert!(!install_succeeded(0x8A15_010A)); // restart first, then install again
        assert!(!install_succeeded(-1) && !install_succeeded(127));
    }

    #[test]
    fn user_path_script_quotes_user_names_with_spaces_and_apostrophes() {
        let s = add_to_user_path_script(r"C:\Users\Anigma PC\.local\bin");
        assert!(s.starts_with(r"$d='C:\Users\Anigma PC\.local\bin';"), "{s}");
        assert!(s.contains("ExpandString") && s.contains("DoNotExpandEnvironmentNames"));
        let s = add_to_user_path_script(r"C:\Users\O'Neil\.local\bin");
        assert!(s.starts_with(r"$d='C:\Users\O''Neil\.local\bin';"), "{s}");
        // Balanced braces after format! escaping.
        assert_eq!(s.matches('{').count(), s.matches('}').count());
    }

    /// Runs the real script on Windows (CI): the folder must land in the user PATH exactly once.
    #[cfg(windows)]
    #[test]
    fn user_path_script_really_updates_user_path() {
        let dir = std::env::temp_dir().join("pcbridge path test");
        let d = dir.display().to_string();
        let read = || powershell("[Environment]::GetEnvironmentVariable('Path','User')").stdout;
        let before = read();
        for _ in 0..2 {
            let o = powershell(&add_to_user_path_script(&d));
            assert!(o.ok(), "script failed: {} {}", o.stdout, o.stderr);
        }
        let after = read();
        assert_eq!(after.split(';').filter(|p| p.trim() == d).count(), 1, "{after}");
        // Put the runner's PATH back the way it was.
        let restore =
            format!("[Environment]::SetEnvironmentVariable('Path','{}','User')", before.trim().replace('\'', "''"));
        assert!(powershell(&restore).ok());
        assert!(!before.contains(&d));
    }
}
