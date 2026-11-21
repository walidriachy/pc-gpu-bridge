//! Desktop notifications from headless modes (shims / MCP) without pulling in GUI crates.

pub fn send(title: &str, body: &str) {
    if std::env::var_os("PCBRIDGE_NO_NOTIFY").is_some() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title));
        let _ = std::process::Command::new("osascript").args(["-e", &script]).spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (title, body);
    }
}
