//! Commands used by the Windows app: dependency installs, agent + Syncthing services, Wake-on-LAN,
//! power settings, pairing code. Compiled everywhere (so CI checks it), but most commands only do
//! real work on Windows.

use crate::mac::{check, tailscale_status};
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
        // Copy the bundled agent source into the PCBridge folder and install it into a private venv.
        let src = r.join("config").join("agent-src");
        let _ = std::fs::remove_dir_all(&src);
        copy_dir(&resources, &src)?;
        refresh_path();
        if !venv_python().exists() {
            let o = run("python", &["-m", "venv", &r.join("config").join("venv").display().to_string()]);
            if !o.ok() {
                return Err(format!("Couldn't prepare Python: {}", o.stderr));
            }
        }
        let py = venv_python().display().to_string();
        let o = run(&py, &["-m", "pip", "install", "--disable-pip-version-check", "-q", &src.display().to_string()]);
        if !o.ok() {
            return Err(format!("Couldn't install the PC Bridge agent: {}", o.stderr.lines().last().unwrap_or("")));
        }
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

/// Start (or restart) the agent and Syncthing as hidden background processes.
pub fn start_services(state: &AppState) -> Result<(), String> {
    let ws = load_state();
    let mut procs = state.procs.lock().unwrap();
    let alive =
        |c: &mut Option<std::process::Child>| c.as_mut().map(|c| matches!(c.try_wait(), Ok(None))).unwrap_or(false);
    if !alive(&mut procs.agent) && venv_python().exists() {
        let child = command(&venv_python().display().to_string())
            .args(["-m", "pcbridge_agent", "--root", &ws.root])
            .spawn()
            .map_err(|e| format!("Couldn't start the agent: {e}"))?;
        procs.agent = Some(child);
    }
    if !alive(&mut procs.syncthing) && !ws.syncthing_api_key.is_empty() {
        if let Some(bin) = which("syncthing") {
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

/// Background watchdog: keep services running (Windows only, after setup).
pub fn spawn_watchdog(state: AppState) {
    std::thread::spawn(move || loop {
        if cfg!(windows) && load_state().setup_complete {
            let _ = start_services(&state);
        }
        std::thread::sleep(Duration::from_secs(15));
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
        let status = c.as_ref().and_then(|c| c.status().ok());
        let gpu = c.as_ref().and_then(|c| c.gpu().ok());
        let jobs = c.as_ref().and_then(|c| c.jobs().ok());
        Ok(json!({"agent_running": status.is_some(), "status": status, "gpu": gpu, "jobs": jobs, "root": ws.root,
                  "tailscale": tailscale_status()}))
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
            "Install the latest NVIDIA driver (Setup → step 2).",
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
            "Restart PC Bridge; make sure Syncthing is installed (Setup → install step).",
        ));
        out.push(check(
            "Wake-on-LAN set up",
            !ws.mac_address.is_empty(),
            ws.mac_address.clone(),
            "Setup → Wake-on-LAN → “Do it for me”.",
        ));
        out.push(check("Starts with Windows", autostart, "", "Settings → turn on “Start with Windows”."));
        Ok(out)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

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
