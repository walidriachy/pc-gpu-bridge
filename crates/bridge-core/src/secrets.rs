//! The pairing key ("token") the Mac sends to the PC.
//!
//! It lives in `~/.pcbridge/token`, readable only by this user (0600) — the same way the PC keeps
//! its own copy. Earlier versions kept it in the macOS Keychain; because the app is signed ad hoc,
//! every update looked like a new app to macOS and the Keychain asked for the Mac password again.
//! A key still in the Keychain is moved to the file the first time it is read (that read may ask
//! one last time) and then removed from the Keychain.
//! `PCBRIDGE_TOKEN` overrides everything (used by tests and CI).

use std::path::{Path, PathBuf};

const SERVICE: &str = "com.pcbridge.app";
const ACCOUNT: &str = "agent-token";

pub fn token_path() -> PathBuf {
    crate::config::home_dir().join("token")
}

pub fn get_token() -> Option<String> {
    if let Ok(t) = std::env::var("PCBRIDGE_TOKEN") {
        return Some(t);
    }
    if let Some(t) = read_token_file(&token_path()) {
        return Some(t);
    }
    let entry = keyring::Entry::new(SERVICE, ACCOUNT).ok()?;
    let t = entry.get_password().ok()?;
    if write_token_file(&token_path(), &t).is_ok() {
        let _ = entry.delete_credential();
    }
    Some(t)
}

pub fn set_token(token: &str) -> Result<(), String> {
    write_token_file(&token_path(), token).map_err(|e| format!("Couldn't save the pairing key: {e}"))?;
    if let Ok(e) = keyring::Entry::new(SERVICE, ACCOUNT) {
        let _ = e.delete_credential();
    }
    Ok(())
}

pub fn clear_token() {
    let _ = std::fs::remove_file(token_path());
    if let Ok(e) = keyring::Entry::new(SERVICE, ACCOUNT) {
        let _ = e.delete_credential();
    }
}

fn read_token_file(path: &Path) -> Option<String> {
    let t = std::fs::read_to_string(path).ok()?;
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Write via a private temp file and rename, so the key is never readable by others, even briefly.
fn write_token_file(path: &Path, token: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(token.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_file_round_trip_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/token");
        write_token_file(&p, "abc123").unwrap();
        assert_eq!(read_token_file(&p).as_deref(), Some("abc123"));
        write_token_file(&p, "new\n").unwrap();
        assert_eq!(read_token_file(&p).as_deref(), Some("new"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(&p, "  \n").unwrap();
        assert_eq!(read_token_file(&p), None);
    }
}
