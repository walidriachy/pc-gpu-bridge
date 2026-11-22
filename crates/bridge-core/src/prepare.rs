//! Getting a job ready for the PC before it is sent:
//! - is it a job that can only work on a Mac (Mac-only programs in the command or the project)?
//! - which of the job's folder files the PC doesn't have yet (the folder copy may still be running),
//! - which Node projects need their packages installed on the PC (`node_modules` is never synced),
//! - and, after a failed PC run, whether it failed because something was missing on the PC.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Folders a job's folder scan skips: never synced, or Syncthing's own.
const SKIP_DIRS: &[&str] =
    &["node_modules", ".next", ".venv", "__pycache__", ".git", ".claude", ".stversions", ".stfolder"];
/// Above this many files the folder is too big to compare before every job; only the files named
/// in the command are checked then (as before).
pub const MAX_SCAN_FILES: usize = 30_000;
/// Missing files PC Bridge sends itself before a job. More than this waits for the folder copy.
pub const MAX_PUSH_FILES: usize = 20_000;
pub const MAX_PUSH_BYTES: u64 = 4_000_000_000;

#[derive(Debug, Clone)]
pub struct LocalFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: Option<f64>,
}

#[derive(Debug, Default)]
pub struct FolderScan {
    pub files: Vec<LocalFile>,
    /// More than [`MAX_SCAN_FILES`] files: not compared.
    pub too_big: bool,
    /// A Mac-only program bundled in the folder (e.g. `engine/browser/.../mac_arm-158`), when the
    /// folder has no Windows build next to it.
    pub mac_only_program: Option<String>,
}

fn skip_file(name: &str) -> bool {
    name == ".DS_Store"
        || name.ends_with(".pcbridge-part")
        || name.starts_with("~syncthing~")
        || (name.starts_with(".syncthing.") && name.ends_with(".tmp"))
}

/// Path parts that mean "a program built for macOS".
fn mac_marker(part: &str) -> bool {
    let p = part.to_ascii_lowercase();
    [
        "mac_arm",
        "mac-arm64",
        "mac_x64",
        "mac-x64",
        "macos-arm64",
        "macos-x64",
        "darwin-arm64",
        "darwin-x64",
        "darwin_arm64",
    ]
    .iter()
    .any(|m| p.contains(m))
        || (p.ends_with(".app") && p.len() > 4)
}

/// Path parts that mean "a program built for Windows".
fn windows_marker(part: &str) -> bool {
    let p = part.to_ascii_lowercase();
    p.ends_with(".exe") || ["win64", "win32", "win-x64", "windows-x64", "win_x64"].iter().any(|m| p.contains(m))
}

/// List the job's folder (without `node_modules`, `.venv`, `.git` …).
pub fn scan_folder(dir: &Path) -> FolderScan {
    let mut scan = FolderScan::default();
    let mut mac: Option<String> = None;
    let mut windows = false;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(meta) = std::fs::symlink_metadata(e.path()) else { continue };
            if meta.is_symlink() {
                continue;
            }
            if mac.is_none() && mac_marker(&name) {
                mac = Some(e.path().strip_prefix(dir).unwrap_or(&e.path()).to_string_lossy().replace('\\', "/"));
            }
            if windows_marker(&name) {
                windows = true;
            }
            if meta.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) && !(name.to_ascii_lowercase().ends_with(".app")) {
                    stack.push(e.path());
                }
                continue;
            }
            if skip_file(&name) {
                continue;
            }
            if scan.files.len() >= MAX_SCAN_FILES {
                scan.too_big = true;
                scan.files.clear();
                scan.mac_only_program = if windows { None } else { mac };
                return scan;
            }
            let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64());
            scan.files.push(LocalFile { path: e.path(), size: meta.len(), mtime });
        }
    }
    scan.mac_only_program = if windows { None } else { mac };
    scan
}

/// Programs that run the project's own code (as opposed to ffmpeg, blender …, which don't care
/// what else is in the folder).
pub fn runs_project_code(tool: &str, argv: &[String]) -> bool {
    const RUNNERS: &[&str] = &["node", "npm", "npx", "tsx", "pnpm", "yarn", "bun", "deno", "bash", "sh", "zsh"];
    let t = tool.to_ascii_lowercase();
    let t = t.trim_end_matches(".exe");
    if RUNNERS.contains(&t) || t.starts_with("python") {
        return true;
    }
    if matches!(t, "powershell" | "pwsh" | "cmd") {
        return argv.iter().any(|a| {
            let a = a.to_ascii_lowercase();
            RUNNERS.iter().chain(["python"].iter()).any(|r| {
                a.starts_with(&format!("{r} ")) || a.contains(&format!(" {r} ")) || a.contains(&format!("; {r} "))
            })
        });
    }
    false
}

/// Uses Node packages (so `node_modules` must exist on the PC).
pub fn runs_node(tool: &str, argv: &[String]) -> bool {
    const NODE: &[&str] = &["node", "npm", "npx", "tsx", "pnpm", "yarn"];
    let t = tool.to_ascii_lowercase();
    let t = t.trim_end_matches(".exe");
    if NODE.contains(&t) {
        return true;
    }
    matches!(t, "powershell" | "pwsh" | "cmd")
        && argv.iter().any(|a| {
            let a = a.to_ascii_lowercase();
            NODE.iter().any(|r| {
                a.starts_with(&format!("{r} ")) || a.contains(&format!(" {r} ")) || a.contains(&format!("; {r} "))
            })
        })
}

/// A Mac-only tool or path in the command itself.
pub fn mac_only_in_command(args: &[String]) -> Option<String> {
    const MARKERS: &[&str] = &[
        "/applications/",
        ".app/contents",
        "osascript",
        "xcrun",
        "xcodebuild",
        "/opt/homebrew",
        "/usr/local/cellar",
        "pbcopy",
        "pbpaste",
        "afplay",
        "launchctl",
        "mdfind",
    ];
    args.iter().find_map(|a| {
        let l = a.to_ascii_lowercase();
        MARKERS.iter().find(|m| l.contains(*m)).map(|m| m.trim_matches('/').to_string())
    })
}

/// Folders (the job's folder and the folders of scripts it runs, then upwards, stopping at the
/// shared folder) whose `package.json` lists packages: they need `npm install` on the PC.
pub fn node_projects(cwd: &Path, args: &[String], stop_at: &Path) -> Vec<PathBuf> {
    let mut starts = vec![cwd.to_path_buf()];
    for a in args {
        let l = a.to_ascii_lowercase();
        if [".js", ".mjs", ".cjs", ".ts", ".mts", ".tsx"].iter().any(|e| l.ends_with(e)) {
            let p = if Path::new(a).is_absolute() { PathBuf::from(a) } else { cwd.join(a) };
            if let Some(d) = p.parent() {
                starts.push(d.to_path_buf());
            }
        }
    }
    let mut out: Vec<PathBuf> = vec![];
    for start in starts {
        let mut d = Some(start.as_path());
        while let Some(dir) = d {
            if !dir.starts_with(stop_at) {
                break;
            }
            let pj = dir.join("package.json");
            if pj.is_file() {
                if has_dependencies(&pj) && !out.iter().any(|o| o == dir) {
                    out.push(dir.to_path_buf());
                }
                break;
            }
            d = dir.parent();
        }
    }
    out
}

fn has_dependencies(package_json: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(package_json) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    ["dependencies", "devDependencies", "optionalDependencies"]
        .iter()
        .any(|k| v.get(*k).and_then(|d| d.as_object()).is_some_and(|o| !o.is_empty()))
}

/// Newest change to a Node project's package list on the Mac (seconds since 1970).
pub fn node_manifest_mtime(dir: &Path) -> f64 {
    ["package.json", "package-lock.json"]
        .iter()
        .filter_map(|f| std::fs::metadata(dir.join(f)).ok()?.modified().ok())
        .filter_map(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .fold(0.0, f64::max)
}

/// The command that installs a Node project's packages on the PC.
pub fn npm_install_argv(dir: &Path) -> Vec<String> {
    if dir.join("package-lock.json").is_file() {
        vec!["npm".into(), "ci".into(), "--no-audit".into(), "--no-fund".into()]
    } else {
        // No lock file: don't create one (it would sync back to the Mac).
        vec!["npm".into(), "install".into(), "--no-package-lock".into(), "--no-audit".into(), "--no-fund".into()]
    }
}

/// Why a PC job failed, when it is because something it needs isn't on the PC (so the Mac, which
/// has everything, can run it instead). `None` for ordinary failures.
pub fn missing_on_pc(stderr: &str) -> Option<&'static str> {
    let s = stderr.to_ascii_lowercase();
    if s.contains("not a valid win32 application") || s.contains("exec format error") || s.contains("bad cpu type") {
        return Some("it uses a Mac-only program");
    }
    if s.contains("cannot find module") || s.contains("err_module_not_found") || s.contains("cannot find package") {
        return Some("a file or Node package it needs isn't on the PC");
    }
    if s.contains("modulenotfounderror") || s.contains("no module named") {
        return Some("a Python package it needs isn't installed on the PC");
    }
    if s.contains("is not recognized as the name of a cmdlet")
        || s.contains("is not recognized as an internal or external command")
        || s.contains("command not found")
        || (s.contains("spawn") && s.contains("enoent"))
    {
        return Some("a program it needs isn't installed on the PC");
    }
    if s.contains("no such file or directory") || s.contains("cannot find the path specified") {
        return Some("a file it needs isn't on the PC");
    }
    None
}

pub fn human_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn scan_skips_unsynced_folders_and_finds_mac_only_programs() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        std::fs::create_dir_all(r.join("engine/browser/chrome-headless-shell/mac_arm-158/bin")).unwrap();
        std::fs::write(r.join("engine/browser/chrome-headless-shell/mac_arm-158/bin/chrome"), b"x").unwrap();
        std::fs::create_dir_all(r.join("node_modules/a")).unwrap();
        std::fs::write(r.join("node_modules/a/i.js"), b"x").unwrap();
        std::fs::create_dir_all(r.join(".venv/bin")).unwrap();
        std::fs::write(r.join(".venv/bin/python"), b"x").unwrap();
        std::fs::write(r.join("engine/reel.mjs"), b"abc").unwrap();
        std::fs::write(r.join(".DS_Store"), b"x").unwrap();
        let scan = scan_folder(r);
        let mut names: Vec<String> =
            scan.files.iter().map(|f| f.path.strip_prefix(r).unwrap().to_string_lossy().replace('\\', "/")).collect();
        names.sort();
        assert_eq!(names, vec!["engine/browser/chrome-headless-shell/mac_arm-158/bin/chrome", "engine/reel.mjs"]);
        assert_eq!(scan.mac_only_program.as_deref(), Some("engine/browser/chrome-headless-shell/mac_arm-158"));
        // A Windows build next to it: not Mac-only.
        std::fs::create_dir_all(r.join("engine/browser/chrome-headless-shell/win64-158")).unwrap();
        assert_eq!(scan_folder(r).mac_only_program, None);
    }

    #[test]
    fn project_code_runners() {
        assert!(runs_project_code("node", &s(&["engine/reel.mjs"])));
        assert!(runs_project_code("python3", &s(&["x.py"])));
        assert!(runs_project_code("powershell", &s(&["-Command", "node engine/reel.mjs render v01"])));
        assert!(!runs_project_code("ffmpeg", &s(&["-i", "a.mov", "b.mp4"])));
        assert!(!runs_project_code("powershell", &s(&["-Command", "Get-ChildItem"])));
        assert!(runs_node("powershell", &s(&["-Command", "cd x; npm run build"])));
        assert!(!runs_node("python", &s(&["x.py"])));
    }

    #[test]
    fn mac_only_commands() {
        assert_eq!(mac_only_in_command(&s(&["-e", "osascript -e 'beep'"])).as_deref(), Some("osascript"));
        assert!(mac_only_in_command(&s(&["/Applications/Foo.app/Contents/MacOS/foo"])).is_some());
        assert_eq!(mac_only_in_command(&s(&["-i", "in.mov", "out.mp4"])), None);
    }

    #[test]
    fn node_projects_with_packages_only() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        std::fs::create_dir_all(r.join("app/src")).unwrap();
        std::fs::write(r.join("app/package.json"), r#"{"dependencies":{"sharp":"1"}}"#).unwrap();
        std::fs::create_dir_all(r.join("engine")).unwrap();
        std::fs::write(r.join("engine/package.json"), r#"{"name":"e","type":"module"}"#).unwrap();
        assert_eq!(node_projects(&r.join("app/src"), &[], r), vec![r.join("app")]);
        // No packages listed: nothing to install.
        assert!(node_projects(r, &s(&["engine/reel.mjs"]), r).is_empty());
        // Never above the shared folder.
        assert!(node_projects(&r.join("engine"), &[], &r.join("engine/sub")).is_empty());
        assert_eq!(npm_install_argv(&r.join("app"))[1], "install");
    }

    #[test]
    fn recognises_missing_on_pc_failures() {
        assert!(missing_on_pc("Error: Cannot find module 'D:\\x\\engine\\reel.mjs'").is_some());
        assert!(missing_on_pc("ModuleNotFoundError: No module named 'cv2'").is_some());
        assert!(missing_on_pc("'whisper-cli' is not recognized as an internal or external command").is_some());
        assert!(missing_on_pc("Error: spawn chrome ENOENT").is_some());
        assert_eq!(missing_on_pc("TypeError: x is not a function"), None);
    }
}
