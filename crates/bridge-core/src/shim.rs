//! The redirect engine shared by the shims (`pc-bridge shim ffmpeg ...`) and the MCP tools.

use crate::client::{AgentClient, ClientError, RunRequest};
use crate::config::BridgeConfig;
use crate::detect::{self, Classification, JobKind};
use crate::memory::{self, MemoryInfo};
use crate::paths::{normalize, PathMapper};
use crate::router::{self, Decision, History, RouteInput, Target};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where shim output goes (real terminal, or a buffer for MCP).
pub trait ShimIo {
    fn out(&mut self, s: &str);
    fn err(&mut self, s: &str);
    /// A user-visible notification (e.g. "PC unreachable, ran on the Mac").
    fn notify(&mut self, s: &str);
}

pub struct StdIo;
impl ShimIo for StdIo {
    fn out(&mut self, s: &str) {
        use std::io::Write;
        let mut o = std::io::stdout();
        let _ = o.write_all(s.as_bytes());
        let _ = o.flush();
    }
    fn err(&mut self, s: &str) {
        use std::io::Write;
        let mut e = std::io::stderr();
        let _ = e.write_all(s.as_bytes());
        let _ = e.flush();
    }
    fn notify(&mut self, s: &str) {
        eprintln!("[PC Bridge] {s}");
        crate::notify::send("PC Bridge", s);
    }
}

#[derive(Default)]
pub struct Capture {
    pub stdout: String,
    pub stderr: String,
    pub notes: Vec<String>,
}
impl ShimIo for Capture {
    fn out(&mut self, s: &str) {
        self.stdout.push_str(s);
    }
    fn err(&mut self, s: &str) {
        self.stderr.push_str(s);
    }
    fn notify(&mut self, s: &str) {
        self.notes.push(s.to_string());
    }
}

/// Runs a command on the Mac. Injected so tests don't spawn real tools.
pub type LocalRunner = fn(tool: &str, args: &[String], cwd: &Path) -> i32;

pub struct Engine {
    pub cfg: BridgeConfig,
    pub client: Option<AgentClient>,
    pub history: Option<History>,
    pub mac_mem: Option<MemoryInfo>,
    pub local: LocalRunner,
    /// How long to wait for the PC after sending Wake-on-LAN.
    pub wake_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub argv: Vec<String>,
    pub cwd_rel: String,
    pub inputs: Vec<(PathBuf, String)>,
    pub class: Classification,
    pub input_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ShimResult {
    pub exit_code: i32,
    pub target: Target,
    pub reason: String,
    pub outputs: Vec<PathBuf>,
    pub job_id: Option<String>,
    pub seconds: f64,
    pub decision: Option<Decision>,
}

fn looks_like_path(s: &str) -> bool {
    // Characters that never appear in a Mac file name but are common in inline code
    // (`powershell -Command '...'`, `node -e '...'`): such an argument is not a path.
    const CODE_CHARS: &[char] = &['\n', '\r', ';', '|', '$', '{', '}', '<', '>', '"', '`', '\\'];
    !s.is_empty()
        && s.len() <= 1024
        && !s.starts_with('-')
        && !s.contains("://")
        && !s.contains(CODE_CHARS)
        && (s.contains('/') || s.contains('.'))
}

/// Flags whose next argument is inline code, never a path.
fn is_code_flag(tool: &str, arg: &str) -> bool {
    let tool = tool.to_ascii_lowercase();
    let tool = tool.trim_end_matches(".exe");
    let arg_l = arg.to_ascii_lowercase();
    match tool {
        t if t.starts_with("python") => arg == "-c",
        "node" => matches!(arg, "-e" | "--eval" | "-p" | "--print"),
        "powershell" | "pwsh" => matches!(arg_l.as_str(), "-command" | "-c" | "-encodedcommand" | "-ec"),
        "bash" | "sh" | "zsh" => arg == "-c",
        "cmd" => matches!(arg_l.as_str(), "/c" | "/k"),
        _ => false,
    }
}

/// Real tool to use on the PC (Windows has `python`, not `python3`).
pub fn pc_tool(tool: &str) -> String {
    match tool {
        "python3" => "python".into(),
        other => other.into(),
    }
}

/// Resolve an argument to a Mac path if it looks like one.
fn resolve(cwd: &Path, s: &str) -> Option<PathBuf> {
    if !looks_like_path(s) {
        return None;
    }
    let p = Path::new(s);
    Some(normalize(&if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }))
}

/// Translate a command for the PC. Returns Err(reason) if it can't run there.
pub fn plan(mapper: &PathMapper, tool: &str, args: &[String], cwd: &Path) -> Result<Plan, String> {
    // Windows has `python`, not `python3`; a Linux/WSL agent (posix root) keeps the name.
    let mut argv = vec![if mapper.pc_root.starts_with('/') { tool.to_string() } else { pc_tool(tool) }];
    let mut inputs: Vec<(PathBuf, String)> = vec![];
    let mut needs_cwd = false;
    let mut skip_next = false;
    for (i, a) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            argv.push(a.clone());
            continue;
        }
        // python -c CODE, powershell -Command CODE, …: never treat code as a path.
        if is_code_flag(tool, a) {
            skip_next = true;
            argv.push(a.clone());
            continue;
        }
        let (prefix, value) = match a.split_once('=') {
            Some((k, v)) if k.starts_with('-') && looks_like_path(v) => (format!("{k}="), v.to_string()),
            _ => (String::new(), a.clone()),
        };
        let Some(mac) = resolve(cwd, &value) else {
            argv.push(a.clone());
            continue;
        };
        let exists = mac.exists();
        let parent_exists = mac.parent().map(|p| p.exists()).unwrap_or(false);
        if !exists && !parent_exists {
            argv.push(a.clone()); // not a real path (e.g. "1.5", "scale=1280:-1")
            continue;
        }
        match mapper.mac_to_rel(&mac) {
            Some(rel) => {
                if exists && mac.is_file() {
                    inputs.push((mac.clone(), rel.clone()));
                }
                argv.push(format!("{prefix}{}", mapper.rel_to_pc(&rel)));
            }
            None if exists => {
                return Err(format!(
                    "'{}' isn't inside a folder shared with the PC (argument {})",
                    mac.display(),
                    i + 1
                ));
            }
            None => {
                needs_cwd = true;
                argv.push(a.clone());
            }
        }
    }
    let cwd_rel = match mapper.mac_to_rel(&normalize(cwd)) {
        Some(r) => r,
        None if !needs_cwd => "workspace".into(),
        None => return Err("this folder isn't shared with the PC".into()),
    };
    let input_paths: Vec<PathBuf> = inputs.iter().map(|(p, _)| p.clone()).collect();
    let input_bytes = input_paths.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
    let class = detect::classify(tool, args, cwd, &input_paths);
    if tool == "ffmpeg" {
        if let Some(rewritten) = detect::ffmpeg_to_nvenc(&argv[1..]) {
            argv.truncate(1);
            argv.extend(rewritten);
        }
    }
    Ok(Plan { argv, cwd_rel, inputs, class, input_bytes })
}

impl Engine {
    pub fn from_config(cfg: BridgeConfig) -> Self {
        let client = cfg.is_paired().then(|| AgentClient::new(cfg.base_url(), crate::secrets::get_token()));
        let history = History::open(&crate::config::home_dir().join("history.sqlite")).ok();
        Self { cfg, client, history, mac_mem: None, local: run_local_real, wake_timeout: Duration::from_secs(90) }
    }

    pub fn mapper(&self) -> PathMapper {
        PathMapper::new(self.cfg.shared_folders.clone(), self.cfg.pc_root.clone())
    }

    /// Check the PC; if it's asleep send Wake-on-LAN and wait. Returns PC status JSON.
    pub fn ensure_awake(&self, io: &mut dyn ShimIo) -> Option<(Value, bool)> {
        let client = self.client.as_ref()?;
        if let Ok(s) = client.status() {
            return Some((s, true));
        }
        if self.cfg.pc_mac_address.is_empty() || self.wake_timeout.is_zero() {
            return None;
        }
        io.err("[PC Bridge] waking the PC…\n");
        let _ = crate::wol::wake(&self.cfg.pc_mac_address);
        let start = Instant::now();
        while start.elapsed() < self.wake_timeout {
            std::thread::sleep(Duration::from_secs(3));
            if let Ok(s) = client.status() {
                return Some((s, false));
            }
        }
        None
    }

    fn local_fallback(
        &self,
        tool: &str,
        args: &[String],
        cwd: &Path,
        reason: String,
        d: Option<Decision>,
    ) -> ShimResult {
        let start = Instant::now();
        let code = (self.local)(tool, args, cwd);
        ShimResult {
            exit_code: code,
            target: Target::Mac,
            reason,
            outputs: vec![],
            job_id: None,
            seconds: start.elapsed().as_secs_f64(),
            decision: d,
        }
    }

    /// Main entry. `force`: Some(Pc) for "run this on the PC", Some(Mac) to force local.
    pub fn run(
        &self,
        tool: &str,
        args: &[String],
        cwd: &Path,
        force: Option<Target>,
        io: &mut dyn ShimIo,
    ) -> ShimResult {
        let tool_l = tool.to_lowercase();
        if force == Some(Target::Mac) {
            return self.local_fallback(tool, args, cwd, "forced local".into(), None);
        }
        if force.is_none() && detect::OPT_IN_TOOLS.contains(&tool_l.as_str()) {
            let opted = detect::project_opt_ins(cwd);
            if !opted.iter().any(|t| t == &tool_l) {
                return self.local_fallback(tool, args, cwd, "project hasn't opted in (.pcbridge)".into(), None);
            }
        }
        if force.is_none() && !self.cfg.redirect_enabled {
            return self.local_fallback(tool, args, cwd, "Redirecting is paused".into(), None);
        }
        let mapper = self.mapper();
        let plan = match plan(&mapper, &tool_l, args, cwd) {
            Ok(p) => p,
            Err(why) => {
                if force == Some(Target::Pc) {
                    io.err(&format!("[PC Bridge] can't run on the PC: {why}\n"));
                    return ShimResult {
                        exit_code: 2,
                        target: Target::Pc,
                        reason: why,
                        outputs: vec![],
                        job_id: None,
                        seconds: 0.0,
                        decision: None,
                    };
                }
                return self.local_fallback(tool, args, cwd, why, None);
            }
        };
        if force.is_none() && plan.class.kind == JobKind::Light {
            return self.local_fallback(tool, args, cwd, plan.class.reason.clone(), None);
        }

        let Some((status, was_awake)) = self.ensure_awake(io) else {
            let why = "The PC couldn't be reached, so this ran on the Mac instead".to_string();
            if self.cfg.notify_on_fallback {
                io.notify(&why);
            }
            if force == Some(Target::Pc) {
                return ShimResult {
                    exit_code: 3,
                    target: Target::Pc,
                    reason: why,
                    outputs: vec![],
                    job_id: None,
                    seconds: 0.0,
                    decision: None,
                };
            }
            return self.local_fallback(tool, args, cwd, why, None);
        };

        let mac_mem = self.mac_mem.clone().unwrap_or_else(memory::local_memory);
        let decision = router::decide(
            &RouteInput {
                tool: tool_l.clone(),
                class: plan.class.clone(),
                input_bytes: plan.input_bytes,
                mac_mem,
                pc_free_ram: status["memory"]["available_bytes"].as_u64(),
                pc_reachable: true,
                pc_awake: was_awake,
                pc_queue_depth: status["queue_depth"].as_u64().unwrap_or(0) as u32,
                measured_mbps: self.cfg.measured_mbps,
                redirect_enabled: true,
            },
            self.history.as_ref(),
        );
        if force.is_none() && decision.target == Target::Mac {
            return self.local_fallback(tool, args, cwd, decision.reason.clone(), Some(decision));
        }
        match self.run_on_pc(&tool_l, &plan, io) {
            Ok(mut r) => {
                r.reason = decision.reason.clone();
                r.decision = Some(decision);
                if let Some(h) = &self.history {
                    let _ = h.record(
                        &tool_l,
                        plan.class.kind,
                        Target::Pc,
                        plan.input_bytes,
                        r.seconds,
                        r.exit_code,
                        &format!("{tool} {}", args.join(" ")),
                        r.job_id.as_deref(),
                        None,
                    );
                }
                r
            }
            Err(e) => {
                if force == Some(Target::Pc) {
                    let why = format!("The PC job couldn't run: {e}");
                    io.notify(&why);
                    return ShimResult {
                        exit_code: 3,
                        target: Target::Pc,
                        reason: why,
                        outputs: vec![],
                        job_id: None,
                        seconds: 0.0,
                        decision: Some(decision),
                    };
                }
                let why = format!("The PC job couldn't run ({e}), so it ran on the Mac instead");
                if self.cfg.notify_on_fallback {
                    io.notify(&why);
                }
                self.local_fallback(tool, args, cwd, why, Some(decision))
            }
        }
    }

    fn run_on_pc(&self, tool: &str, plan: &Plan, io: &mut dyn ShimIo) -> Result<ShimResult, ClientError> {
        let client = self.client.as_ref().ok_or_else(|| ClientError::Other("not paired".into()))?;
        let mapper = self.mapper();
        // Make sure inputs are on the PC (Syncthing may not have caught up yet).
        for (mac, rel) in &plan.inputs {
            let local_size = std::fs::metadata(mac).map(|m| m.len()).unwrap_or(0);
            let st = client.stat(rel)?;
            if st["exists"] != Value::Bool(true) || st["size"].as_u64() != Some(local_size) {
                io.err(&format!("[PC Bridge] sending {} to the PC…\n", mac.display()));
                client.push_file(mac, rel)?;
            }
        }
        let start = Instant::now();
        let req = RunRequest {
            argv: plan.argv.clone(),
            cwd: plan.cwd_rel.clone(),
            name: Some(format!("{tool} {}", plan.argv.get(1..).map(|a| a.join(" ")).unwrap_or_default())),
            gpu: plan.class.kind == JobKind::Gpu,
            inputs: plan.inputs.iter().map(|(_, r)| r.clone()).collect(),
            cache: true,
            env: Default::default(),
        };
        // Once the PC has accepted the job, never run it a second time on the Mac.
        let mut accepted = false;
        let result = client.run(&req, |ev| {
            accepted = true;
            match ev["type"].as_str() {
                Some("stdout") => io.out(&mapper.rewrite_output(ev["data"].as_str().unwrap_or(""))),
                Some("stderr") => io.err(&mapper.rewrite_output(ev["data"].as_str().unwrap_or(""))),
                Some("queued") if ev["position"].as_u64().unwrap_or(0) > 1 => {
                    io.err(&format!("[PC Bridge] queued on the PC (position {})\n", ev["position"]));
                }
                Some("cached") => io.err("[PC Bridge] same job ran before: reusing the cached result\n"),
                _ => {}
            }
        });
        let outcome = match result {
            Ok(o) => o,
            Err(e) if accepted => {
                io.err(&format!(
                    "[PC Bridge] lost the connection to the PC while the job was running ({e}). \
                     It may still finish there: see the Jobs tab.\n"
                ));
                return Ok(ShimResult {
                    exit_code: 1,
                    target: Target::Pc,
                    reason: "connection to the PC lost during the job".into(),
                    outputs: vec![],
                    job_id: None,
                    seconds: start.elapsed().as_secs_f64(),
                    decision: None,
                });
            }
            Err(e) => return Err(e),
        };
        // Bring outputs back: give Syncthing a moment, then pull anything still missing/different.
        let mut outputs = vec![];
        let deadline = Instant::now() + Duration::from_secs(self.cfg.sync_wait_secs);
        for rel in &outcome.outputs {
            let Some(mac) = mapper.rel_to_mac(rel) else { continue };
            // The job already ran: a file we can't fetch is a warning, never a reason to run it again.
            let Some(pc_size) = client.stat(rel).ok().and_then(|s| s["size"].as_u64()) else { continue };
            loop {
                let local = std::fs::metadata(&mac).ok().map(|m| m.len());
                if local == Some(pc_size) {
                    outputs.push(mac);
                    break;
                }
                if Instant::now() >= deadline {
                    match client.pull_file(rel, &mac) {
                        Ok(_) => outputs.push(mac),
                        Err(e) => io.err(&format!("[PC Bridge] couldn't bring back {}: {e}\n", mac.display())),
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        Ok(ShimResult {
            exit_code: outcome.exit_code,
            target: Target::Pc,
            reason: String::new(),
            outputs,
            job_id: outcome.job_id,
            seconds: start.elapsed().as_secs_f64(),
            decision: None,
        })
    }
}

/// Find the real binary on PATH, skipping our own shim folder.
pub fn find_real(tool: &str) -> Option<PathBuf> {
    let shim_dir = crate::config::home_dir().join("bin");
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| normalize(d) != normalize(&shim_dir))
        .map(|d| d.join(tool))
        .find(|p| p.is_file())
}

pub fn run_local_real(tool: &str, args: &[String], cwd: &Path) -> i32 {
    let Some(exe) = find_real(tool) else {
        eprintln!("[PC Bridge] '{tool}' isn't installed on this Mac and the PC couldn't run it.");
        return 127;
    };
    std::process::Command::new(exe)
        .args(args)
        .current_dir(cwd)
        .env("PCBRIDGE_LOCAL", "1")
        .status()
        .map(|s| s.code().unwrap_or(1))
        .unwrap_or(127)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SharedFolder;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn plans_ffmpeg_with_paths_and_nvenc() {
        let dir = tempfile::tempdir().unwrap();
        let videos = dir.path().join("Videos");
        std::fs::create_dir_all(videos.join("out")).unwrap();
        std::fs::write(videos.join("in.mov"), b"0123456789").unwrap();
        let m = PathMapper::new(
            vec![SharedFolder { mac_path: videos.display().to_string(), name: "Videos".into() }],
            r"D:\PCBridge",
        );
        let p =
            plan(&m, "ffmpeg", &s(&["-i", "in.mov", "-c:v", "libx264", "-crf", "18", "out/o.mp4"]), &videos).unwrap();
        assert_eq!(
            p.argv,
            s(&[
                "ffmpeg",
                "-i",
                r"D:\PCBridge\workspace\Videos\in.mov",
                "-c:v",
                "h264_nvenc",
                "-cq",
                "18",
                r"D:\PCBridge\workspace\Videos\out\o.mp4"
            ])
        );
        assert_eq!(p.cwd_rel, "workspace/Videos");
        assert_eq!(p.inputs.len(), 1);
        assert_eq!(p.input_bytes, 10);
        assert_eq!(p.class.kind, JobKind::Gpu);
    }

    #[test]
    fn refuses_unshared_input() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.mov"), b"1").unwrap();
        let m = PathMapper::new(vec![], r"D:\PCBridge");
        assert!(plan(&m, "ffmpeg", &s(&["-i", "x.mov", "o.mp4"]), dir.path()).is_err());
    }

    #[test]
    fn inline_scripts_are_not_paths() {
        let dir = tempfile::tempdir().unwrap();
        let m = PathMapper::new(
            vec![SharedFolder { mac_path: dir.path().display().to_string(), name: "w".into() }],
            r"D:\PCBridge",
        );
        let script = r#"$m=[Environment]::GetEnvironmentVariable("Path","Machine"); Get-Command ffmpeg | % Source"#;
        let p = plan(&m, "powershell", &s(&["-NoProfile", "-Command", script]), dir.path()).unwrap();
        assert_eq!(p.argv[3], script);
        // Even without a code flag, code-looking text isn't mistaken for an output file.
        let p = plan(&m, "pwsh", &s(&["-File", "x.ps1", "-Note", "a.b; c"]), dir.path()).unwrap();
        assert_eq!(p.argv[4], "a.b; c");
        let p = plan(&m, "node", &s(&["-e", "console.log(1.5)"]), dir.path()).unwrap();
        assert_eq!(p.argv[2], "console.log(1.5)");
        // Real file names with spaces and brackets are still paths.
        std::fs::write(dir.path().join("my clip (1).mov"), b"1").unwrap();
        let p = plan(&m, "ffmpeg", &s(&["-i", "my clip (1).mov", "out.mp4"]), dir.path()).unwrap();
        assert_eq!(p.argv[2], r"D:\PCBridge\workspace\w\my clip (1).mov");
    }

    #[test]
    fn python_code_is_not_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let m = PathMapper::new(
            vec![SharedFolder { mac_path: dir.path().display().to_string(), name: "w".into() }],
            r"D:\PCBridge",
        );
        let p = plan(&m, "python3", &s(&["-c", "import torch; print('a/b.c')"]), dir.path()).unwrap();
        assert_eq!(p.argv[0], "python");
        assert_eq!(p.argv[2], "import torch; print('a/b.c')");
        assert_eq!(p.class.kind, JobKind::Gpu);
    }

    #[test]
    fn key_value_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cfg.json"), b"{}").unwrap();
        let m = PathMapper::new(
            vec![SharedFolder { mac_path: dir.path().display().to_string(), name: "w".into() }],
            r"D:\PCBridge",
        );
        let p = plan(&m, "blender", &s(&["--config=cfg.json", "-b"]), dir.path()).unwrap();
        assert_eq!(p.argv[1], r"--config=D:\PCBridge\workspace\w\cfg.json");
    }

    fn fake_local(_: &str, _: &[String], _: &Path) -> i32 {
        42
    }

    #[test]
    fn opt_in_and_paused_run_locally() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = BridgeConfig::default();
        cfg.shared_folders.push(SharedFolder { mac_path: dir.path().display().to_string(), name: "w".into() });
        let e =
            Engine { cfg, client: None, history: None, mac_mem: None, local: fake_local, wake_timeout: Duration::ZERO };
        let mut io = Capture::default();
        let r = e.run("npm", &s(&["run", "build"]), dir.path(), None, &mut io);
        assert_eq!((r.exit_code, r.target), (42, Target::Mac));
        assert!(r.reason.contains("opted in"));
        // light command -> local
        let r = e.run("ffprobe", &s(&["-version"]), dir.path(), None, &mut io);
        assert_eq!(r.target, Target::Mac);
        // GPU job but PC unreachable (no client) -> local + notification
        std::fs::write(dir.path().join("a.mov"), b"1").unwrap();
        let r = e.run("ffmpeg", &s(&["-i", "a.mov", "b.mp4"]), dir.path(), None, &mut io);
        assert_eq!((r.exit_code, r.target), (42, Target::Mac));
        assert_eq!(io.notes.len(), 1);
        // forcing the PC while unreachable reports an error instead of running locally
        let r = e.run("ffmpeg", &s(&["-i", "a.mov", "b.mp4"]), dir.path(), Some(Target::Pc), &mut io);
        assert_eq!(r.exit_code, 3);
    }
}
