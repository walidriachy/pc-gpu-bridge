//! The redirect engine shared by the shims (`pc-bridge shim ffmpeg ...`) and the MCP tools.

use crate::client::{AgentClient, ClientError, RunRequest};
use crate::config::BridgeConfig;
use crate::detect::{self, Classification, JobKind};
use crate::memory::{self, MemoryInfo};
use crate::paths::{normalize, PathMapper};
use crate::router::{self, Decision, History, JobRecord, RouteInput, Target};
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

/// PC jobs at least this long show a "finished" notification on the Mac.
pub const FINISH_NOTICE_SECS: f64 = 60.0;

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
    /// See [`router::job_signature`].
    pub signature: String,
}

/// Where the time of a PC job went, for the router's history.
#[derive(Debug, Clone, Copy, Default)]
struct PcTiming {
    /// The agent's "duration": the process's own run time.
    compute_secs: Option<f64>,
    /// `compute_secs` excludes output collection (agents from 0.1.4).
    exact: bool,
    queue_secs: f64,
    /// Sending inputs and waiting for outputs to come back.
    transfer_secs: f64,
    cached: bool,
    /// From sending the job until its exit event.
    stream_secs: f64,
}

/// The PC job this process is currently following, so Ctrl-C can stop it on the PC too.
static RUNNING_JOB: std::sync::Mutex<Option<(AgentClient, String)>> = std::sync::Mutex::new(None);

/// Ctrl-C (or Claude stopping a command) used to stop only the Mac side and leave the job running
/// on the PC. Now the PC job is cancelled first, then this process exits as usual (code 130).
fn watch_ctrl_c() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = ctrlc::set_handler(|| {
            let job = RUNNING_JOB.lock().ok().and_then(|g| g.clone());
            if let Some((client, id)) = job {
                let _ = client.cancel(&id);
                eprintln!("\n[PC Bridge] stopped the job on the PC");
            }
            std::process::exit(130);
        });
    });
}

/// The PC is on but didn't accept this Mac's pairing key.
pub const KEY_REFUSED: &str =
    "The PC didn't accept this Mac's pairing key. Pair again: on the Mac open PC Bridge → Settings → Run setup again";

#[derive(Debug, Clone)]
pub struct ShimResult {
    pub exit_code: i32,
    pub target: Target,
    pub reason: String,
    pub outputs: Vec<PathBuf>,
    pub job_id: Option<String>,
    pub seconds: f64,
    pub decision: Option<Decision>,
    /// A program the PC job needed but the PC doesn't have (catalog key, e.g. "blender").
    /// The PC already printed a hint on stderr; the job is never re-run on the Mac because of it.
    pub missing: Option<String>,
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

/// "out.mp4", "renders/frame.png": a plausible new output file (no spaces, an extension with
/// at least one letter), as opposed to "1.5", "29.97" or a sentence.
fn looks_like_output_file(s: &str) -> bool {
    let name = s.rsplit('/').next().unwrap_or(s);
    !s.contains(' ')
        && !s.contains(':')
        && name.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty()
                && (1..=5).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && ext.chars().any(|c| c.is_ascii_alphabetic())
        })
}

/// The quote character for a shell whose arguments are code (powershell/pwsh: ', cmd: ").
fn shell_quote(tool: &str) -> Option<char> {
    let t = tool.to_ascii_lowercase();
    match t.trim_end_matches(".exe") {
        "powershell" | "pwsh" => Some('\''),
        "cmd" => Some('"'),
        _ => None,
    }
}

fn translate_code(mapper: &PathMapper, code: &str, quote: Option<char>) -> String {
    match quote {
        Some(q) => crate::mcp::translate_inline_quoted(mapper, code, q),
        None => code.to_string(),
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
    let shell = shell_quote(tool);
    // powershell/pwsh -Command and cmd /c take every remaining argument as code.
    let mut code_rest = false;
    for (i, a) in args.iter().enumerate() {
        if code_rest {
            argv.push(translate_code(mapper, a, shell));
            continue;
        }
        if skip_next {
            skip_next = false;
            argv.push(a.clone());
            continue;
        }
        // python -c CODE, powershell -Command CODE, …: never treat code as a path.
        if is_code_flag(tool, a) {
            if shell.is_some() {
                code_rest = true;
            } else {
                skip_next = true;
            }
            argv.push(a.clone());
            continue;
        }
        let (prefix, value) = match a.split_once('=') {
            Some((k, v)) if k.starts_with('-') && looks_like_path(v) => (format!("{k}="), v.to_string()),
            _ => (String::new(), a.clone()),
        };
        let Some(mac) = resolve(cwd, &value) else {
            // A shell's inline code (powershell without -Command) may still name Mac paths.
            argv.push(if shell.is_some() && !a.starts_with('-') {
                translate_code(mapper, a, shell)
            } else {
                a.clone()
            });
            continue;
        };
        let exists = mac.exists();
        // A relative argument is a path only if it exists: "1.5", "29.97", "a red car. 4k" or
        // "Get-Content x.txt" are not, and new output files keep working because the PC runs
        // in the same (shared) folder. An absolute Mac path inside a shared folder is always
        // translated, even when it doesn't exist yet (a new output folder).
        let absolute = Path::new(&value).is_absolute();
        let mapped = mapper.mac_to_rel(&mac);
        if !exists && !(absolute && mapped.is_some()) {
            let output_name = !absolute && looks_like_output_file(&value);
            if (absolute && mac.parent().is_some_and(|p| p.exists())) || output_name {
                needs_cwd = true;
            }
            argv.push(if shell.is_some() { translate_code(mapper, a, shell) } else { a.clone() });
            continue;
        }
        match mapped {
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
    let signature = router::job_signature(tool, args);
    Ok(Plan { argv, cwd_rel, inputs, class, input_bytes, signature })
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

    /// A Mac notification when a long PC job ends, so the user can look away while it runs.
    pub fn notify_finished(&self, what: &str, seconds: f64, exit_code: i32) {
        if !self.cfg.notify_on_finish || seconds < FINISH_NOTICE_SECS {
            return;
        }
        let what: String = what.chars().take(60).collect();
        let took = if seconds >= 3600.0 {
            format!("{:.0} h {:.0} min", (seconds / 3600.0).floor(), (seconds % 3600.0) / 60.0)
        } else if seconds >= 60.0 {
            format!("{:.0} min {:.0} s", (seconds / 60.0).floor(), seconds % 60.0)
        } else {
            format!("{seconds:.0} s")
        };
        let (title, body) = if exit_code == 0 {
            ("PC job finished", format!("{what} — done in {took}"))
        } else {
            ("PC job failed", format!("{what} — stopped with code {exit_code} after {took}"))
        };
        crate::notify::send(title, &body);
    }

    /// Whether this Mac can send Wake-on-LAN to the PC.
    pub fn can_wake(&self) -> bool {
        !self.cfg.pc_mac_address.is_empty() && !self.wake_timeout.is_zero()
    }

    /// The router's choice for a planned job, given the PC's status (`None`: the PC is asleep).
    pub fn decide(&self, tool: &str, plan: &Plan, status: Option<&Value>, awake: bool) -> Decision {
        let mac_mem = self.mac_mem.clone().unwrap_or_else(memory::local_memory);
        router::decide(
            &RouteInput {
                tool: tool.to_string(),
                class: plan.class.clone(),
                input_bytes: plan.input_bytes,
                mac_mem,
                pc_free_ram: status.and_then(|s| s["memory"]["available_bytes"].as_u64()),
                pc_reachable: status.is_some() || self.can_wake(),
                pc_awake: awake,
                pc_queue_depth: status.and_then(|s| s["queue_depth"].as_u64()).unwrap_or(0) as u32,
                pc_queue_secs: status.and_then(|s| s["queue_eta_secs"].as_f64()),
                measured_mbps: self.cfg.measured_mbps,
                // `run` handles "redirecting is paused" itself (an explicit PC request overrides it).
                redirect_enabled: true,
                signature: plan.signature.clone(),
            },
            self.history.as_ref(),
        )
    }

    /// Check the PC; if it's asleep send Wake-on-LAN and wait. Returns PC status JSON.
    /// Reach the PC, waking it if needed. `Err` says why it can't be used, in plain words.
    pub fn ensure_awake(&self, io: &mut dyn ShimIo) -> Result<(Value, bool), String> {
        let Some(client) = self.client.as_ref() else {
            return Err("This Mac isn't paired with a PC yet. Open PC Bridge to set it up.".into());
        };
        match client.status() {
            Ok(s) => return Ok((s, true)),
            // Waking won't help: the PC is on and turned the key down.
            Err(ClientError::Unauthorized) => return Err(KEY_REFUSED.into()),
            Err(_) => {}
        }
        if !self.can_wake() {
            return Err("The PC couldn't be reached".into());
        }
        io.err("[PC Bridge] waking the PC…\n");
        let _ = crate::wol::wake(&self.cfg.pc_mac_address);
        let start = Instant::now();
        while start.elapsed() < self.wake_timeout {
            std::thread::sleep(Duration::from_secs(3));
            match client.status() {
                Ok(s) => return Ok((s, false)),
                Err(ClientError::Unauthorized) => return Err(KEY_REFUSED.into()),
                Err(_) => {}
            }
        }
        Err("The PC couldn't be reached".into())
    }

    /// Run on the Mac. With `learn`, the run is one the router could have sent to the PC (it chose
    /// the Mac, or the PC wasn't available), so its time is recorded for future decisions.
    fn local_fallback(
        &self,
        tool: &str,
        args: &[String],
        cwd: &Path,
        reason: String,
        d: Option<Decision>,
        learn: Option<&Plan>,
    ) -> ShimResult {
        let start = Instant::now();
        let code = (self.local)(tool, args, cwd);
        let seconds = start.elapsed().as_secs_f64();
        if let (Some(plan), Some(h)) = (learn, &self.history) {
            let tool_l = tool.to_lowercase();
            let name = format!("{tool} {}", args.join(" "));
            let _ = h.record(&JobRecord {
                signature: &plan.signature,
                ..JobRecord::new(&tool_l, plan.class.kind, Target::Mac, plan.input_bytes, seconds, code, &name)
            });
        }
        ShimResult {
            exit_code: code,
            target: Target::Mac,
            reason,
            outputs: vec![],
            job_id: None,
            seconds,
            decision: d,
            missing: None,
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
            return self.local_fallback(tool, args, cwd, "forced local".into(), None, None);
        }
        if force.is_none() && detect::OPT_IN_TOOLS.contains(&tool_l.as_str()) {
            let opted = detect::project_opt_ins(cwd);
            if !opted.iter().any(|t| t == &tool_l) {
                return self.local_fallback(tool, args, cwd, "project hasn't opted in (.pcbridge)".into(), None, None);
            }
        }
        if force.is_none() && !self.cfg.redirect_enabled {
            return self.local_fallback(tool, args, cwd, "Redirecting is paused".into(), None, None);
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
                        missing: None,
                    };
                }
                return self.local_fallback(tool, args, cwd, why, None, None);
            }
        };
        if force.is_none() && plan.class.kind == JobKind::Light {
            return self.local_fallback(tool, args, cwd, plan.class.reason.clone(), None, None);
        }

        // Decide before waking: if the PC is asleep and the Mac would be quicker even without the
        // wake-up, don't wake it at all.
        let probe = self.client.as_ref().map(|c| c.status());
        if force.is_none() {
            if let Some(Err(e)) = &probe {
                if !matches!(e, ClientError::Unauthorized) && self.can_wake() {
                    let asleep = self.decide(&tool_l, &plan, None, false);
                    if asleep.target == Target::Mac {
                        let why = format!("{} (the PC is asleep, so it wasn't woken)", asleep.reason);
                        return self.local_fallback(tool, args, cwd, why, Some(asleep), Some(&plan));
                    }
                }
            }
        }
        let awake = match probe {
            Some(Ok(s)) => Ok((s, true)),
            _ => self.ensure_awake(io),
        };
        let (status, was_awake) = match awake {
            Ok(v) => v,
            Err(problem) => {
                let why = format!("{problem}, so this ran on the Mac instead");
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
                        missing: None,
                    };
                }
                return self.local_fallback(tool, args, cwd, why, None, Some(&plan));
            }
        };

        let decision = self.decide(&tool_l, &plan, Some(&status), was_awake);
        if force.is_none() && decision.target == Target::Mac {
            return self.local_fallback(tool, args, cwd, decision.reason.clone(), Some(decision), Some(&plan));
        }
        match self.run_on_pc(&tool_l, &plan, io) {
            // The PC accepted the job: whatever happened there (including "program not installed on
            // the PC", reported in `r.missing`) is the result. It is never run again on the Mac.
            Ok((mut r, t)) => {
                // Honest about why: when the PC was asked for but the router would have kept it here.
                r.reason = if force == Some(Target::Pc) && decision.target == Target::Mac {
                    format!(
                        "Sent to the PC as asked (the Mac might have been quicker: ~{:.0}s vs ~{:.0}s via the PC)",
                        decision.mac_est_secs, decision.pc_est_secs
                    )
                } else if force == Some(Target::Pc) {
                    format!("Sent to the PC as asked. {}", decision.reason)
                } else {
                    decision.reason.clone()
                };
                self.notify_finished(&format!("{tool} {}", args.join(" ")), r.seconds, r.exit_code);
                r.decision = Some(decision);
                if let Some(h) = &self.history {
                    let name = format!("{tool} {}", args.join(" "));
                    // An agent that didn't report its run time: take the stream time minus the queue.
                    let compute = t.compute_secs.unwrap_or((t.stream_secs - t.queue_secs).max(0.0));
                    let _ = h.record(&JobRecord {
                        signature: &plan.signature,
                        compute_secs: compute,
                        queue_secs: t.queue_secs,
                        transfer_secs: t.transfer_secs,
                        cached: t.cached,
                        approx: !t.exact,
                        pc_job_id: r.job_id.as_deref(),
                        ..JobRecord::new(
                            &tool_l,
                            plan.class.kind,
                            Target::Pc,
                            plan.input_bytes,
                            r.seconds,
                            r.exit_code,
                            &name,
                        )
                    });
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
                        missing: None,
                    };
                }
                let why = format!("The PC job couldn't run ({e}), so it ran on the Mac instead");
                if self.cfg.notify_on_fallback {
                    io.notify(&why);
                }
                self.local_fallback(tool, args, cwd, why, Some(decision), Some(&plan))
            }
        }
    }

    fn run_on_pc(&self, tool: &str, plan: &Plan, io: &mut dyn ShimIo) -> Result<(ShimResult, PcTiming), ClientError> {
        let client = self.client.as_ref().ok_or_else(|| ClientError::Other("not paired".into()))?;
        let mapper = self.mapper();
        let push_start = Instant::now();
        // Make sure inputs are on the PC (Syncthing may not have caught up yet).
        for (mac, rel) in &plan.inputs {
            let local_size = std::fs::metadata(mac).map(|m| m.len()).unwrap_or(0);
            let st = client.stat(rel)?;
            if st["exists"] != Value::Bool(true) || st["size"].as_u64() != Some(local_size) {
                io.err(&format!("[PC Bridge] sending {} to the PC…\n", mac.display()));
                client.push_file(mac, rel)?;
            }
        }
        let push_secs = push_start.elapsed().as_secs_f64();
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
        watch_ctrl_c();
        let result = client.run(&req, |ev| {
            accepted = true;
            match ev["type"].as_str() {
                Some("job") => {
                    if let (Ok(mut g), Some(id)) = (RUNNING_JOB.lock(), ev["id"].as_str()) {
                        *g = Some((client.clone(), id.to_string()));
                    }
                }
                Some("stdout") => io.out(&mapper.rewrite_output(ev["data"].as_str().unwrap_or(""))),
                Some("stderr") => io.err(&mapper.rewrite_output(ev["data"].as_str().unwrap_or(""))),
                Some("queued") if ev["position"].as_u64().unwrap_or(0) > 1 => {
                    io.err(&format!("[PC Bridge] queued on the PC (position {})\n", ev["position"]));
                }
                Some("cached") => io.err("[PC Bridge] same job ran before: reusing the cached result\n"),
                _ => {}
            }
        });
        if let Ok(mut g) = RUNNING_JOB.lock() {
            *g = None;
        }
        let outcome = match result {
            Ok(o) => o,
            Err(e) if accepted => {
                io.err(&format!(
                    "[PC Bridge] lost the connection to the PC while the job was running ({e}). \
                     It may still finish there: see the Jobs tab.\n"
                ));
                let seconds = start.elapsed().as_secs_f64();
                let timing = PcTiming { transfer_secs: push_secs, stream_secs: seconds, ..Default::default() };
                return Ok((
                    ShimResult {
                        exit_code: 1,
                        target: Target::Pc,
                        reason: "connection to the PC lost during the job".into(),
                        outputs: vec![],
                        job_id: None,
                        seconds,
                        decision: None,
                        missing: None,
                    },
                    timing,
                ));
            }
            Err(e) => return Err(e),
        };
        let stream_secs = start.elapsed().as_secs_f64();
        // Bring outputs back: give Syncthing a moment, then pull anything still missing/different.
        let sync_start = Instant::now();
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
        let timing = PcTiming {
            compute_secs: outcome.duration,
            exact: outcome.exact_duration,
            queue_secs: outcome.queue_secs.unwrap_or(0.0),
            transfer_secs: push_secs + sync_start.elapsed().as_secs_f64(),
            cached: outcome.cached,
            stream_secs,
        };
        let result = ShimResult {
            exit_code: outcome.exit_code,
            target: Target::Pc,
            reason: String::new(),
            outputs,
            job_id: outcome.job_id,
            seconds: start.elapsed().as_secs_f64(),
            decision: None,
            missing: outcome.missing,
        };
        Ok((result, timing))
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
                "out/o.mp4"
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

    fn ok_local(_: &str, _: &[String], _: &Path) -> i32 {
        0
    }

    fn learning_engine(dir: &Path, local: LocalRunner) -> Engine {
        let mut cfg = BridgeConfig::default();
        cfg.shared_folders.push(SharedFolder { mac_path: dir.display().to_string(), name: "w".into() });
        cfg.notify_on_fallback = false;
        Engine {
            cfg,
            client: None,
            history: Some(History::in_memory().unwrap()),
            mac_mem: None,
            local,
            wake_timeout: Duration::ZERO,
        }
    }

    #[test]
    fn records_mac_runs_only_when_the_router_had_a_say() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.mov"), vec![0u8; 4096]).unwrap();
        let mut e = learning_engine(dir.path(), fake_local);
        let enc = s(&["-i", "a.mov", "-c:v", "libx264", "b.mp4"]);
        let mut io = Capture::default();
        // Forced local, not opted in, light: the router never weighed these, so nothing is learned.
        e.run("ffmpeg", &enc, dir.path(), Some(Target::Mac), &mut io);
        e.run("npm", &s(&["run", "build"]), dir.path(), None, &mut io);
        e.run("ffprobe", &s(&["-version"]), dir.path(), None, &mut io);
        e.cfg.redirect_enabled = false;
        e.run("ffmpeg", &enc, dir.path(), None, &mut io);
        assert!(e.history.as_ref().unwrap().recent(10).is_empty());
        // The PC was out of reach: the Mac run is what the router would have compared against.
        e.cfg.redirect_enabled = true;
        let r = e.run("ffmpeg", &enc, dir.path(), None, &mut io);
        assert_eq!((r.target, r.exit_code), (Target::Mac, 42));
        let rows = e.history.as_ref().unwrap().recent(10);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!((row["target"].as_str(), row["tool"].as_str()), (Some("mac"), Some("ffmpeg")));
        assert_eq!((row["input_bytes"].as_i64(), row["exit_code"].as_i64()), (Some(4096), Some(42)));
        assert_eq!(row["signature"], "ffmpeg:libx264");
        assert_eq!(row["cached"], false);
        assert!(row["compute_secs"].as_f64().unwrap() >= 0.0);
    }

    #[test]
    fn mac_runs_change_the_next_decision() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.mov"), vec![0u8; 1 << 20]).unwrap();
        let e = learning_engine(dir.path(), ok_local);
        let enc = s(&["-i", "a.mov", "-c:v", "libx264", "b.mp4"]);
        let big_encode = RouteInput {
            tool: "ffmpeg".into(),
            class: Classification { kind: JobKind::Gpu, est_ram_bytes: 1 << 30, reason: "test".into() },
            input_bytes: 2 << 30,
            mac_mem: MemoryInfo {
                total_bytes: 16 << 30,
                available_bytes: 8 << 30,
                used_percent: 50.0,
                pressure: crate::memory::Pressure::Normal,
            },
            pc_free_ram: Some(28 << 30),
            pc_reachable: true,
            pc_awake: true,
            pc_queue_depth: 0,
            pc_queue_secs: None,
            measured_mbps: 900.0,
            redirect_enabled: true,
            signature: router::job_signature("ffmpeg", &enc),
        };
        let mut io = Capture::default();
        for n in 0..3 {
            let d = router::decide(&big_encode, e.history.as_ref());
            assert_eq!(d.target, Target::Pc, "defaults until 3 Mac runs are known (have {n})");
            e.run("ffmpeg", &enc, dir.path(), None, &mut io);
        }
        // This Mac turned out to encode almost instantly: the router now keeps the job local.
        let d = router::decide(&big_encode, e.history.as_ref());
        assert_eq!(d.target, Target::Mac, "{d:?}");
        assert_eq!(d.mac_est_secs, 5.0);
    }
    fn spaced() -> (tempfile::TempDir, PathBuf, PathMapper) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("Created Apps in claude");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("in.mov"), b"0123456789").unwrap();
        std::fs::write(shared.join("package.json"), b"{}").unwrap();
        let m = PathMapper::new(
            vec![SharedFolder { mac_path: shared.display().to_string(), name: "Created Apps in claude".into() }],
            r"D:\PCBridge",
        );
        (dir, shared, m)
    }

    #[test]
    fn numbers_and_sentences_are_never_paths() {
        let (_d, shared, m) = spaced();
        let args = s(&["-ss", "00:00:01.5", "-i", "in.mov", "-r", "29.97", "-b:v", "2.5M", "out.mp4"]);
        let p = plan(&m, "ffmpeg", &args, &shared).unwrap();
        assert_eq!(p.argv[2], "00:00:01.5");
        assert_eq!(p.argv[4], r"D:\PCBridge\workspace\Created Apps in claude\in.mov");
        assert_eq!(&p.argv[5..], &s(&["-r", "29.97", "-b:v", "2.5M", "out.mp4"])[..]);
        let p = plan(&m, "python3", &s(&["gen.py", "--prompt", "a red car. 4k"]), &shared).unwrap();
        assert_eq!(p.argv[3], "a red car. 4k");
    }

    #[test]
    fn powershell_code_is_kept_and_mac_paths_inside_are_quoted() {
        let (_d, shared, m) = spaced();
        let p = plan(&m, "powershell", &s(&["-NoProfile", "Get-Content package.json"]), &shared).unwrap();
        assert_eq!(p.argv, s(&["powershell", "-NoProfile", "Get-Content package.json"]));
        let code = format!("Get-Content {}/package.json | Measure-Object", shared.display());
        let p = plan(&m, "powershell", &s(&["-NoProfile", "-Command", &code]), &shared).unwrap();
        assert_eq!(
            p.argv[3],
            r"Get-Content 'D:\PCBridge\workspace\Created Apps in claude\package.json' | Measure-Object"
        );
        // -Command split over several words: all of them are code.
        let p = plan(&m, "pwsh", &s(&["-Command", "Write-Output", "1.5"]), &shared).unwrap();
        assert_eq!(p.argv, s(&["pwsh", "-Command", "Write-Output", "1.5"]));
    }

    #[test]
    fn cmd_c_and_new_output_folders() {
        let (_d, shared, m) = spaced();
        let p = plan(&m, "cmd", &s(&["/c", "echo", "hi"]), &shared).unwrap();
        assert_eq!(p.argv, s(&["cmd", "/c", "echo", "hi"]));
        let new_out = shared.join("renders").join("run2").join("out.mp4");
        let p = plan(&m, "ffmpeg", &s(&["-i", "in.mov", &new_out.display().to_string()]), &shared).unwrap();
        assert_eq!(p.argv[3], r"D:\PCBridge\workspace\Created Apps in claude\renders\run2\out.mp4");
    }

    #[test]
    fn new_output_in_an_unshared_folder_is_refused() {
        let (dir, shared, m) = spaced();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let input = shared.join("in.mov").display().to_string();
        let r = plan(&m, "ffmpeg", &s(&["-i", &input, "out.mp4"]), &elsewhere);
        assert!(r.is_err(), "{:?}", r.map(|p| p.argv));
        assert!(plan(&m, "ffmpeg", &s(&["-i", &input, "-r", "29.97", "-f", "null", "-"]), &elsewhere).is_ok());
    }

    #[test]
    fn inline_translation_quotes_paths_with_spaces_once() {
        let (_d, shared, m) = spaced();
        let base = shared.display().to_string();
        assert_eq!(
            crate::mcp::translate_inline(&m, &format!("type {base}/a.txt")),
            r"type 'D:\PCBridge\workspace\Created Apps in claude\a.txt'"
        );
        assert_eq!(
            crate::mcp::translate_inline(&m, &format!("type \"{base}/a.txt\"")),
            r#"type "D:\PCBridge\workspace\Created Apps in claude\a.txt""#
        );
    }
}
