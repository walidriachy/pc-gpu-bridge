//! A private Syncthing instance run by PC Bridge (GUI/API on 127.0.0.1:8385, own home folder),
//! so it never interferes with a Syncthing the user may already run.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub const API_ADDR: &str = "127.0.0.1:8385";
/// Never synced. `.git`: two computers writing one repository at once can corrupt it.
/// `.claude`: each computer's own Claude Code settings and session state.
pub const IGNORES: &[&str] =
    &["node_modules", ".next", ".venv", "__pycache__", "(?d).DS_Store", "*.pcbridge-part", ".git", ".claude"];
/// Seconds Syncthing waits after a change before sending it (its default is 10).
pub const WATCH_DELAY_S: u64 = 2;

/// Overwritten or deleted files go to the folder's `.stversions` bin for 14 days instead of vanishing.
pub fn versioning() -> Value {
    json!({"type": "trashcan", "params": {"cleanoutDays": "14"}})
}

pub fn folder_id(name: &str) -> String {
    let safe: String = name.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    let safe = safe.trim_matches('-');
    format!("pcbridge-{}", if safe.is_empty() { "shared" } else { safe })
}

pub struct Syncthing {
    pub api_key: String,
    pub base: String,
    http: reqwest::blocking::Client,
}

impl Syncthing {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base: format!("http://{API_ADDR}"),
            http: reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().unwrap(),
        }
    }

    fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, String> {
        let mut r = self.http.request(method, format!("{}{}", self.base, path)).header("X-API-Key", &self.api_key);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().map_err(|e| format!("Syncthing isn't responding: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("Syncthing error {}", resp.status()));
        }
        let text = resp.text().unwrap_or_default();
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub fn alive(&self) -> bool {
        self.device_id().is_ok()
    }

    pub fn device_id(&self) -> Result<String, String> {
        self.call(reqwest::Method::GET, "/rest/system/status", None)?["myID"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| "no device id".into())
    }

    pub fn add_device(&self, id: &str, name: &str, address: &str) -> Result<(), String> {
        self.call(
            reqwest::Method::PUT,
            &format!("/rest/config/devices/{id}"),
            Some(json!({"deviceID": id, "name": name, "addresses": [address]})),
        )
        .map(|_| ())
    }

    pub fn share_folder(&self, name: &str, path: &Path, devices: &[String]) -> Result<(), String> {
        let id = folder_id(name);
        self.call(
            reqwest::Method::PUT,
            &format!("/rest/config/folders/{id}"),
            Some(json!({
                "id": id, "label": name, "path": path.display().to_string(), "type": "sendreceive",
                "devices": devices.iter().map(|d| json!({"deviceID": d})).collect::<Vec<_>>(),
                "fsWatcherEnabled": true, "fsWatcherDelayS": WATCH_DELAY_S, "rescanIntervalS": 60,
                "versioning": versioning()
            })),
        )?;
        self.call(reqwest::Method::POST, &format!("/rest/db/ignores?folder={id}"), Some(json!({"ignore": IGNORES})))
            .map(|_| ())
    }

    /// Per-folder sync completion (0–100) as reported by Syncthing.
    pub fn completion(&self, name: &str) -> Option<f64> {
        self.call(reqwest::Method::GET, &format!("/rest/db/completion?folder={}", folder_id(name)), None)
            .ok()?
            .get("completion")?
            .as_f64()
    }
}

pub fn home() -> PathBuf {
    crate::config::home_dir().join("syncthing")
}

pub fn binary_path() -> PathBuf {
    home().join(if cfg!(windows) { "syncthing.exe" } else { "syncthing" })
}

/// Download the official Syncthing release for macOS (universal) into ~/.pcbridge/syncthing.
pub fn download_macos() -> Result<PathBuf, String> {
    let bin = binary_path();
    if bin.exists() {
        return Ok(bin);
    }
    std::fs::create_dir_all(home()).map_err(|e| e.to_string())?;
    let http = reqwest::blocking::Client::builder()
        .user_agent("pc-bridge")
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?;
    let rel: Value = http
        .get("https://api.github.com/repos/syncthing/syncthing/releases/latest")
        .send()
        .and_then(|r| r.json())
        .map_err(|e| format!("Couldn't check the Syncthing download: {e}"))?;
    let asset = rel["assets"]
        .as_array()
        .and_then(|a| {
            a.iter().find(|x| {
                let n = x["name"].as_str().unwrap_or("");
                n.contains("macos-universal") && n.ends_with(".zip")
            })
        })
        .ok_or("Couldn't find the Syncthing download for Mac")?;
    let url = asset["browser_download_url"].as_str().unwrap_or_default();
    let bytes = http.get(url).send().and_then(|r| r.bytes()).map_err(|e| format!("Download failed: {e}"))?;
    let zip = home().join("syncthing.zip");
    std::fs::write(&zip, &bytes).map_err(|e| e.to_string())?;
    let out = home().join("extract");
    let st = Command::new("ditto").args(["-x", "-k"]).arg(&zip).arg(&out).status().map_err(|e| e.to_string())?;
    if !st.success() {
        return Err("Couldn't unpack Syncthing".into());
    }
    let found = std::fs::read_dir(&out)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|d| d.path().join("syncthing"))
        .find(|p| p.exists())
        .ok_or("Syncthing program not found in the download")?;
    std::fs::rename(found, &bin).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(out);
    let _ = std::fs::remove_file(zip);
    Ok(bin)
}

/// Start Syncthing in the background with our fixed API key and address.
pub fn start(bin: &Path, data_home: &Path, api_key: &str) -> Result<Child, String> {
    std::fs::create_dir_all(data_home).map_err(|e| e.to_string())?;
    Command::new(bin)
        .arg("serve")
        .arg("--no-browser")
        .arg(format!("--home={}", data_home.display()))
        .arg(format!("--gui-address={API_ADDR}"))
        .arg(format!("--gui-apikey={api_key}"))
        .env("STNOUPGRADE", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Couldn't start Syncthing: {e}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn folder_ids_match_agent() {
        // Must match agent/pcbridge_agent/syncthing.py:folder_id
        assert_eq!(super::folder_id("My Videos"), "pcbridge-my-videos");
        assert_eq!(super::folder_id("???"), "pcbridge-shared");
    }
}
