//! Wiring PC Bridge into Claude: MCP server entries, CLAUDE.md routing rules, shims and PATH.
//! Every edit is idempotent, keeps a `.pcbridge-backup` copy, and can be undone.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const BEGIN: &str = "<!-- PC Bridge: begin -->";
pub const END: &str = "<!-- PC Bridge: end -->";
pub const PATH_BEGIN: &str = "# >>> PC Bridge >>>";
pub const PATH_END: &str = "# <<< PC Bridge <<<";

pub const CLAUDE_MD_SNIPPET: &str = r#"## PC Bridge: where to run heavy work

This Mac is paired with a Windows PC with an NVIDIA GPU through PC Bridge (`pc_gpu` and `pc_memory` show its exact graphics card and memory).

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
`pc_run` picks the faster computer by itself and says why; add `mode: "pc"` when a job must use the PC's GPU (CUDA). If unsure, ask `pc_route_advice`.
If a job fails because a program is missing on the PC, offer to install it with `pc_install` (ask the user first).
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

/// The program Claude starts for PC Bridge (`mcpServers["pc-bridge"].command`), if connected.
pub fn mcp_command(config_path: &Path) -> Option<String> {
    read_json(config_path).ok()?["mcpServers"]["pc-bridge"]["command"].as_str().map(String::from)
}

/// A path inside the hidden, temporary copy of the app that macOS sometimes runs (App
/// Translocation). It disappears at the next restart.
pub fn is_translocated(path: &str) -> bool {
    path.contains("/AppTranslocation/")
}

/// Point Claude's PC Bridge connections and the helper commands in `bin_dir` at `binary` again
/// when they still point at macOS's hidden temporary copy (written by 0.1.2 when it ran from
/// there). Returns what was fixed.
pub fn repair_translocated(home: &Path, bin_dir: &Path, binary: &str, tools: &[&str]) -> Result<Vec<String>, String> {
    let mut fixed = vec![];
    for (config, who) in [(claude_code_config(home), "Claude Code"), (claude_desktop_config(home), "Claude Desktop")] {
        if mcp_command(&config).is_some_and(|c| c != binary && is_translocated(&c)) {
            install_mcp(&config, binary)?;
            fixed.push(format!("{who} connection"));
        }
    }
    let launcher = std::fs::read_to_string(bin_dir.join("pc-bridge")).unwrap_or_default();
    if is_translocated(&launcher) && !launcher.contains(&format!("\"{binary}\"")) {
        install_shims(bin_dir, binary, tools)?;
        fixed.push("helper commands".into());
    }
    Ok(fixed)
}

/// After an update: rewrite only the parts of the Claude setup that are already installed, so new
/// routing rules and helper commands reach the user without running setup again.
pub fn refresh_installed(home: &Path, bin_dir: &Path, binary: &str, tools: &[&str]) -> Result<Vec<String>, String> {
    let mut done = vec![];
    for (config, who) in [(claude_code_config(home), "Claude Code"), (claude_desktop_config(home), "Claude Desktop")] {
        if mcp_installed(&config) {
            install_mcp(&config, binary)?;
            done.push(format!("{who} connection"));
        }
    }
    let md = claude_md(home);
    let text = std::fs::read_to_string(&md).unwrap_or_default();
    if text.contains(BEGIN) && !text.contains(CLAUDE_MD_SNIPPET.trim()) {
        install_claude_md(&md)?;
        done.push("routing rules in CLAUDE.md".into());
    }
    if bin_dir.join("pc-bridge").exists() {
        install_shims(bin_dir, binary, tools)?;
        done.push("helper commands".into());
    }
    Ok(done)
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
    fn paths_into_the_hidden_temporary_copy_are_repaired() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".pcbridge/bin");
        let old = "/private/var/folders/xy/T/AppTranslocation/0F1E/d/PC Bridge.app/Contents/MacOS/pc-bridge";
        let new = "/Applications/PC Bridge.app/Contents/MacOS/pc-bridge";
        install_mcp(&claude_code_config(home.path()), old).unwrap();
        install_mcp(&claude_desktop_config(home.path()), "/Users/sam/Applications/PC Bridge.app/x").unwrap();
        install_shims(&bin, old, &["ffmpeg"]).unwrap();
        let fixed = repair_translocated(home.path(), &bin, new, &["ffmpeg"]).unwrap();
        assert_eq!(fixed, vec!["Claude Code connection".to_string(), "helper commands".into()]);
        assert_eq!(mcp_command(&claude_code_config(home.path())).as_deref(), Some(new));
        // A deliberate other copy is left alone.
        assert_eq!(
            mcp_command(&claude_desktop_config(home.path())).as_deref(),
            Some("/Users/sam/Applications/PC Bridge.app/x")
        );
        let shim = std::fs::read_to_string(bin.join("ffmpeg")).unwrap();
        assert!(shim.contains(new) && !is_translocated(&shim), "{shim}");
        // Nothing left to fix, and nothing is created when Claude was never connected.
        assert!(repair_translocated(home.path(), &bin, new, &["ffmpeg"]).unwrap().is_empty());
        let empty = tempfile::tempdir().unwrap();
        assert!(repair_translocated(empty.path(), &empty.path().join("bin"), new, &["ffmpeg"]).unwrap().is_empty());
        assert!(!claude_code_config(empty.path()).exists() && !empty.path().join("bin").exists());
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
        assert!(s.contains("offer to install it with `pc_install` (ask the user first)"), "{s}");
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

    #[test]
    fn refresh_touches_only_what_was_installed() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".pcbridge/bin");
        // Nothing installed: nothing refreshed, nothing created.
        assert!(refresh_installed(home.path(), &bin, "/A/pc-bridge", &["ffmpeg"]).unwrap().is_empty());
        assert!(!claude_md(home.path()).exists() && !bin.exists());
        // An old CLAUDE.md block and old shims get the current text.
        let md = claude_md(home.path());
        std::fs::create_dir_all(md.parent().unwrap()).unwrap();
        std::fs::write(&md, format!("mine\n{BEGIN}\nold rules\n{END}\n")).unwrap();
        install_shims(&bin, "/Old/pc-bridge", &["ffmpeg"]).unwrap();
        let done = refresh_installed(home.path(), &bin, "/A/pc-bridge", &["ffmpeg"]).unwrap();
        assert_eq!(done, vec!["routing rules in CLAUDE.md".to_string(), "helper commands".into()]);
        let text = std::fs::read_to_string(&md).unwrap();
        assert!(text.starts_with("mine\n") && text.contains(CLAUDE_MD_SNIPPET.trim()) && !text.contains("old rules"));
        assert!(std::fs::read_to_string(bin.join("ffmpeg")).unwrap().contains("/A/pc-bridge"));
        // Already current: CLAUDE.md is left alone.
        let again = refresh_installed(home.path(), &bin, "/A/pc-bridge", &["ffmpeg"]).unwrap();
        assert_eq!(again, vec!["helper commands".to_string()]);
    }
}
