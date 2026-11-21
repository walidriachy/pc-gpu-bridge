//! Mac-side settings stored in `~/.pcbridge/config.json`. The auth token is NOT stored here: it
//! lives in the macOS Keychain / Windows Credential Manager (see [`crate::secrets`]).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedFolder {
    /// Absolute folder on the Mac, e.g. `/Users/me/Videos`.
    pub mac_path: String,
    /// Folder name inside `PCBridge\workspace\` on the PC, e.g. `Videos`.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BridgeConfig {
    /// Tailscale name or IP of the PC, e.g. `gaming-pc` or `100.101.102.103`.
    pub pc_host: String,
    pub port: u16,
    /// PC root as reported during pairing, e.g. `D:\PCBridge`.
    pub pc_root: String,
    pub pc_hostname: String,
    /// Ethernet MAC address of the PC, used for Wake-on-LAN.
    pub pc_mac_address: String,
    pub shared_folders: Vec<SharedFolder>,
    /// Master switch: when false every shim runs locally.
    pub redirect_enabled: bool,
    /// Measured Mac<->PC throughput in megabits per second.
    pub measured_mbps: f64,
    /// Seconds to wait for outputs to appear via Syncthing before pulling directly.
    pub sync_wait_secs: u64,
    pub notify_on_fallback: bool,
    pub setup_complete: bool,
    pub wizard_step: u32,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            pc_host: String::new(),
            port: 8765,
            pc_root: r"D:\PCBridge".into(),
            pc_hostname: String::new(),
            pc_mac_address: String::new(),
            shared_folders: vec![],
            redirect_enabled: true,
            measured_mbps: 300.0,
            sync_wait_secs: 3,
            notify_on_fallback: true,
            setup_complete: false,
            wizard_step: 0,
        }
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("PCBRIDGE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".pcbridge"))
}

impl BridgeConfig {
    pub fn path() -> PathBuf {
        home_dir().join("config.json")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap())
    }

    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.pc_host, self.port)
    }

    pub fn is_paired(&self) -> bool {
        !self.pc_host.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        let mut c = BridgeConfig::default();
        c.pc_host = "pc".into();
        c.shared_folders.push(SharedFolder { mac_path: "/Users/a/Videos".into(), name: "Videos".into() });
        c.save_to(&p).unwrap();
        let back = BridgeConfig::load_from(&p);
        assert_eq!(back.pc_host, "pc");
        assert_eq!(back.shared_folders, c.shared_folders);
        assert_eq!(back.base_url(), "http://pc:8765");
        assert!(BridgeConfig::load_from(&dir.path().join("missing.json")).pc_host.is_empty());
    }
}
