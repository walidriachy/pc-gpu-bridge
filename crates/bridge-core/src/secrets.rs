//! Token storage in the OS keychain (macOS Keychain / Windows Credential Manager).
//! `PCBRIDGE_TOKEN` overrides it (used by tests and CI).

const SERVICE: &str = "com.pcbridge.app";
const ACCOUNT: &str = "agent-token";

pub fn get_token() -> Option<String> {
    if let Ok(t) = std::env::var("PCBRIDGE_TOKEN") {
        return Some(t);
    }
    keyring::Entry::new(SERVICE, ACCOUNT).ok()?.get_password().ok()
}

pub fn set_token(token: &str) -> Result<(), String> {
    keyring::Entry::new(SERVICE, ACCOUNT)
        .and_then(|e| e.set_password(token))
        .map_err(|e| format!("Couldn't save to the keychain: {e}"))
}

pub fn clear_token() {
    if let Ok(e) = keyring::Entry::new(SERVICE, ACCOUNT) {
        let _ = e.delete_credential();
    }
}
