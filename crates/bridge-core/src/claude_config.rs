//! Wiring PC Bridge into Claude: MCP server entries, CLAUDE.md routing rules, shims and PATH.
//! Every edit is idempotent, keeps a `.pcbridge-backup` copy, and can be undone.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const BEGIN: &str = "<!-- PC Bridge: begin -->";
pub const END: &str = "<!-- PC Bridge: end -->";
pub const PATH_BEGIN: &str = "# >>> PC Bridge >>>";
pub const PATH_END: &str = "# <<< PC Bridge <<<";

pub const CLAUDE_MD_SNIPPET: &str = r#"## PC Bridge: where to run heavy work

This Mac is paired with a Windows PC (RTX 3090 GPU, 24 GB VRAM, 32 GB RAM) through PC Bridge.

Send to the PC (use the `pc-bridge` MCP tools, or just run the command — shims redirect it):
- Video renders and encodes (ffmpeg, blender) → `pc_run`
- AI model inference, image/video generation (torch, diffusers, whisper, ComfyUI) → `pc_run` / `comfy_generate`
- Local LLMs → `ollama_chat`
- Long GPU Python jobs and multi-step GPU workflows → `pc_delegate_task`
- RAM-heavy work: big pandas/numpy processing, Docker builds, large npm/pnpm/Next.js builds and test suites → `pc_run`, `pc_docker`, `pc_node_build`
- When the Mac is short on memory (check with `pc_memory`) → prefer the PC
- Anything else heavy → `pc_run_task`

Keep on the Mac: light edits, git, quick scripts, reading files.
Files must be inside a folder shared with the PC; outputs appear back in the same Mac folder.
If unsure, ask `pc_route_advice`.
"#;

fn backup(path: &Path) {
    if path.exists() {
        let _ = std::fs::copy(path, path.with_extension("pcbridge-backup"));
    }
}

fn read_json(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(json!({})),
        Ok(s) => {
            serde_json::from_str(&s).map_err(|e| format!("{} isn't valid JSON ({e}); not touching it", path.display()))
        }
        Err(_) => Ok(json!({})),
    }
}

fn write_json(path: &Path, v: &Value) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, serde_json::to_string_pretty(v).unwrap()).map_err(|e| e.to_string())
}

pub fn mcp_entry(binary: &str) -> Value {
    json!({"type": "stdio", "command": binary, "args": ["mcp"], "env": {}})
}

/// Add/replace `mcpServers["pc-bridge"]` in a Claude config file. Other keys are preserved.
pub fn install_mcp(config_path: &Path, binary: &str) -> Result<(), String> {
    let mut v = read_json(config_path)?;
    if !v.is_object() {
        return Err(format!("{} has an unexpected format", config_path.display()));
    }
    backup(config_path);
    let servers = v.as_object_mut().unwrap().entry("mcpServers").or_insert_with(|| json!({}));
    servers["pc-bridge"] = mcp_entry(binary);
    write_json(config_path, &v)
}

pub fn uninstall_mcp(config_path: &Path) -> Result<(), String> {
    let mut v = read_json(config_path)?;
    if let Some(s) = v.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
        s.remove("pc-bridge");
    }
    write_json(config_path, &v)
}

pub fn mcp_installed(config_path: &Path) -> bool {
    read_json(config_path).ok().map(|v| v["mcpServers"]["pc-bridge"].is_object()).unwrap_or(false)
}

/// Replace (or append) the text between `begin` and `end` markers.
pub fn upsert_block(existing: &str, begin: &str, end: &str, body: &str) -> String {
    let block = format!("{begin}\n{}\n{end}", body.trim_end());
    if let (Some(a), Some(b)) = (existing.find(begin), existing.find(end)) {
        if b > a {
            return format!("{}{}{}", &existing[..a], block, &existing[b + end.len()..]);
        }
    }
    if existing.trim().is_empty() {
        format!("{block}\n")
    } else {
        format!("{}\n\n{block}\n", existing.trim_end())
    }
}

pub fn remove_block(existing: &str, begin: &str, end: &str) -> String {
    match (existing.find(begin), existing.find(end)) {
        (Some(a), Some(b)) if b > a => {
            let mut s = format!("{}{}", &existing[..a], &existing[b + end.len()..]);
            while s.contains("\n\n\n") {
                s = s.replace("\n\n\n", "\n\n");
            }
            s.trim_end().to_string() + if s.trim().is_empty() { "" } else { "\n" }
        }
        _ => existing.to_string(),
    }
}

pub fn install_claude_md(path: &Path) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    backup(path);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, upsert_block(&existing, BEGIN, END, CLAUDE_MD_SNIPPET)).map_err(|e| e.to_string())
}

pub fn uninstall_claude_md(path: &Path) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    std::fs::write(path, remove_block(&existing, BEGIN, END)).map_err(|e| e.to_string())
}

pub fn shim_script(binary: &str, tool: &str) -> String {
    format!(
        "#!/bin/sh\n# PC Bridge shim: sends heavy {tool} jobs to your PC, runs light ones here.\n\
         if [ -n \"$PCBRIDGE_LOCAL\" ]; then\n  PATH=$(echo \"$PATH\" | tr ':' '\\n' | grep -v '/.pcbridge/bin' | paste -sd: -)\n  exec {tool} \"$@\"\nfi\n\
         exec \"{binary}\" shim {tool} \"$@\"\n"
    )
}

/// Write shims into `bin_dir` for the given tools. Returns the files written.
pub fn install_shims(bin_dir: &Path, binary: &str, tools: &[&str]) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(bin_dir).map_err(|e| e.to_string())?;
    let mut written = vec![];
    for t in tools {
        let p = bin_dir.join(t);
        std::fs::write(&p, shim_script(binary, t)).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
        written.push(p);
    }
    // `pc-bridge` itself, so `pc-bridge run -- <cmd>` and `pc-bridge doctor` work from Terminal.
    let launcher = bin_dir.join("pc-bridge");
    std::fs::write(&launcher, format!("#!/bin/sh\nexec \"{binary}\" \"$@\"\n")).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    written.push(launcher);
    Ok(written)
}

pub fn path_snippet(bin_dir: &Path) -> String {
    format!("export PATH=\"{}:$PATH\"", bin_dir.display())
}

/// Add the shim folder to PATH in the user's shell startup files.
pub fn install_path(home: &Path, bin_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut changed = vec![];
    for f in [".zshrc", ".zprofile", ".bash_profile"] {
        let p = home.join(f);
        if f != ".zshrc" && !p.exists() {
            continue;
        }
        let existing = std::fs::read_to_string(&p).unwrap_or_default();
        backup(&p);
        std::fs::write(&p, upsert_block(&existing, PATH_BEGIN, PATH_END, &path_snippet(bin_dir)))
            .map_err(|e| e.to_string())?;
        changed.push(p);
    }
    Ok(changed)
}

pub fn uninstall_path(home: &Path) {
    for f in [".zshrc", ".zprofile", ".bash_profile"] {
        let p = home.join(f);
        if let Ok(s) = std::fs::read_to_string(&p) {
            let _ = std::fs::write(&p, remove_block(&s, PATH_BEGIN, PATH_END));
        }
    }
}

/// Standard locations on macOS.
pub fn claude_code_config(home: &Path) -> PathBuf {
    home.join(".claude.json")
}
pub fn claude_desktop_config(home: &Path) -> PathBuf {
    home.join("Library/Application Support/Claude/claude_desktop_config.json")
}
pub fn claude_md(home: &Path) -> PathBuf {
    home.join(".claude/CLAUDE.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_install_preserves_other_keys() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("claude.json");
        std::fs::write(&p, r#"{"theme":"dark","mcpServers":{"other":{"command":"x"}}}"#).unwrap();
        install_mcp(&p, "/Applications/PC Bridge.app/Contents/MacOS/pc-bridge").unwrap();
        install_mcp(&p, "/bin/pcb").unwrap(); // idempotent replace
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"]["pc-bridge"]["command"], "/bin/pcb");
        assert!(mcp_installed(&p));
        assert!(p.with_extension("pcbridge-backup").exists());
        uninstall_mcp(&p).unwrap();
        assert!(!mcp_installed(&p));
        // invalid JSON is left alone
        std::fs::write(&p, "{not json").unwrap();
        assert!(install_mcp(&p, "/x").is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{not json");
    }

    #[test]
    fn claude_md_block_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(".claude/CLAUDE.md");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "# My rules\nBe nice.\n").unwrap();
        install_claude_md(&p).unwrap();
        install_claude_md(&p).unwrap();
        let s = std::fs::read_to_string(&p).unwrap();
        assert_eq!(s.matches(BEGIN).count(), 1);
        assert!(s.starts_with("# My rules") && s.contains("pc_delegate_task"));
        uninstall_claude_md(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# My rules\nBe nice.\n");
    }

    #[test]
    fn shims_and_path() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join(".pcbridge/bin");
        let files = install_shims(&bin, "/x/pc-bridge", &["ffmpeg", "python3"]).unwrap();
        assert_eq!(files.len(), 3); // 2 shims + the pc-bridge launcher
        let launcher = std::fs::read_to_string(files.last().unwrap()).unwrap();
        assert!(launcher.contains("exec \"/x/pc-bridge\" \"$@\""), "{launcher}");
        let s = std::fs::read_to_string(bin.join("ffmpeg")).unwrap();
        assert!(s.contains("exec \"/x/pc-bridge\" shim ffmpeg \"$@\""));
        let changed = install_path(d.path(), &bin).unwrap();
        install_path(d.path(), &bin).unwrap();
        assert_eq!(changed.len(), 1); // only .zshrc created; others don't exist
        let z = std::fs::read_to_string(d.path().join(".zshrc")).unwrap();
        assert_eq!(z.matches(PATH_BEGIN).count(), 1);
        uninstall_path(d.path());
        assert!(!std::fs::read_to_string(d.path().join(".zshrc")).unwrap().contains("PC Bridge"));
    }
}
