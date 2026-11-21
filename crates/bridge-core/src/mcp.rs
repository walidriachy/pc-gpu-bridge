//! MCP server over stdio (JSON-RPC 2.0, one message per line). Run as `pc-bridge mcp`.

use crate::memory;
use crate::paths::PathMapper;
use crate::router::{self, RouteInput, Target};
use crate::shim::{plan, Capture, Engine};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_OUTPUT: usize = 20_000;

/// Programs the PC agent knows how to install (`pc_install`); keys match the agent's catalog.
pub const KNOWN_PROGRAMS: &[&str] = &["ffmpeg", "blender", "pytorch", "ollama", "node", "docker", "git"];

/// Split a command line into words, honouring single/double quotes and backslash escapes.
pub fn split_command(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '\'' | '"') => {
                quote = Some(c);
                has = true;
            }
            (None, '\\') | (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    has = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (_, c) => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

fn tail(s: &str) -> String {
    if s.len() <= MAX_OUTPUT {
        return s.to_string();
    }
    let mut start = s.len() - MAX_OUTPUT;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…(earlier output trimmed)…\n{}", &s[start..])
}

/// The last `n` lines of `s`.
fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// "Programs on the PC: installed … · not installed …", from the `tools` map in the PC's /status.
pub fn programs_summary(status: &Value) -> Option<String> {
    let tools = status.get("tools")?.as_object()?;
    let (mut have, mut missing) = (vec![], vec![]);
    for &key in KNOWN_PROGRAMS {
        let found = match key {
            "pytorch" => tools.get("pytorch").or_else(|| tools.get("torch")),
            k => tools.get(k),
        };
        match found.and_then(|v| v.as_bool()) {
            Some(true) => have.push(key),
            Some(false) => missing.push(key),
            None => {}
        }
    }
    if have.is_empty() && missing.is_empty() {
        return None;
    }
    let list = |v: &[&str]| if v.is_empty() { "none".to_string() } else { v.join(", ") };
    let mut line = format!("Programs on the PC: installed: {} · not installed: {}", list(&have), list(&missing));
    if !missing.is_empty() {
        line.push_str(" (pc_install can add them, after asking the user)");
    }
    Some(line)
}

fn str_arg<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(|v| v.as_str())
}

fn words_arg(args: &Value, k: &str) -> Vec<String> {
    match args.get(k) {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        Some(Value::String(s)) => split_command(s),
        _ => vec![],
    }
}

fn cwd_arg(args: &Value) -> PathBuf {
    str_arg(args, "cwd")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn tool_defs() -> Value {
    let cmd = json!({"type": "string", "description": "Command line, e.g. \"ffmpeg -i in.mov -c:v libx264 out.mp4\""});
    let cwd = json!({"type": "string", "description": "Folder on the Mac to run in (must be shared with the PC)"});
    json!([
        {"name": "pc_run", "description": "Run a command on the user's Windows PC (NVIDIA GPU; see pc_gpu / pc_memory) with Mac paths translated automatically. Output streams back and output files sync back to the same Mac folder. Use for ffmpeg encodes, GPU Python (torch/diffusers/whisper), blender renders, big data processing.",
         "inputSchema": {"type": "object", "properties": {"command": cmd, "cwd": cwd,
            "mode": {"type": "string", "enum": ["pc", "auto"], "description": "pc = always the PC (default); auto = let the smart router choose"}}, "required": ["command"]}},
        {"name": "pc_run_task", "description": "Generic: run any shell command line on the PC in PowerShell (Mac paths inside shared folders are translated). Use for one-off heavy tasks with no dedicated tool.",
         "inputSchema": {"type": "object", "properties": {"command": cmd, "cwd": cwd}, "required": ["command"]}},
        {"name": "pc_docker", "description": "Run docker on the PC (build/run/compose) using the PC's RAM. Paths in shared folders are translated.",
         "inputSchema": {"type": "object", "properties": {"args": {"type": "string", "description": "e.g. \"build -t app .\""}, "cwd": cwd}, "required": ["args"]}},
        {"name": "pc_node_build", "description": "Run a big npm/pnpm/npx/node build or test suite on the PC (e.g. next build, jest). The project folder must be shared; node_modules are installed on the PC side.",
         "inputSchema": {"type": "object", "properties": {"tool": {"type": "string", "enum": ["npm", "pnpm", "npx", "node"]}, "args": {"type": "string"}, "cwd": cwd}, "required": ["tool", "args"]}},
        {"name": "pc_delegate_task", "description": "Hand a whole task to the worker Claude running on the PC (claude -p). Good for long GPU workflows that need several steps. Returns the worker's final answer and its job folder.",
         "inputSchema": {"type": "object", "properties": {"task": {"type": "string"}, "files": {"type": "array", "items": {"type": "string"}, "description": "Mac files (in shared folders) to give the worker"}}, "required": ["task"]}},
        {"name": "pc_status", "description": "Is the PC online? Queue, running jobs, which programs are installed on the PC, and Mac vs PC free memory.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "pc_install", "description": "Install a program on the user's Windows PC, in its normal Windows location (never in the shared folders). Known programs: ffmpeg, blender, pytorch, ollama, node, docker, git. Use it when a PC job failed because a program is missing: check pc_status or the job's error first. ASK THE USER FIRST and only install after they say yes. Some installers show a Windows \"Yes\" prompt on the PC's screen that someone at the PC must click, so tell the user. Installs can take several minutes (PyTorch is about 6 GB).",
         "inputSchema": {"type": "object", "properties": {"program": {"type": "string", "description": "One of: ffmpeg, blender, pytorch, ollama, node, docker, git"}}, "required": ["program"]}},
        {"name": "pc_gpu", "description": "GPU usage, VRAM and temperature on the PC.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "pc_memory", "description": "Mac vs PC free RAM side by side, plus the Mac's memory pressure level.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "pc_route_advice", "description": "Ask the smart router whether a command would be faster on the Mac or the PC, without running it.",
         "inputSchema": {"type": "object", "properties": {"command": cmd, "cwd": cwd}, "required": ["command"]}},
        {"name": "pc_push_file", "description": "Copy a Mac file to the PC (into PCBridge\\workspace or a given relative path).",
         "inputSchema": {"type": "object", "properties": {"mac_path": {"type": "string"}, "pc_rel_path": {"type": "string"}}, "required": ["mac_path"]}},
        {"name": "pc_pull_file", "description": "Copy a file from the PC (path relative to PCBridge, e.g. jobs/2026-11-21/abc_x/output/a.png, or an absolute D:\\PCBridge path) to the Mac.",
         "inputSchema": {"type": "object", "properties": {"pc_path": {"type": "string"}, "mac_path": {"type": "string"}}, "required": ["pc_path", "mac_path"]}},
        {"name": "pc_wake", "description": "Wake the PC from sleep (Wake-on-LAN) and wait until it's ready.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "comfy_generate", "description": "Queue a ComfyUI workflow (API-format JSON) on the PC and return the generated image paths.",
         "inputSchema": {"type": "object", "properties": {"workflow": {"type": "object"}}, "required": ["workflow"]}},
        {"name": "ollama_chat", "description": "Chat with a local LLM on the PC through Ollama (models stay warm in VRAM).",
         "inputSchema": {"type": "object", "properties": {"model": {"type": "string"}, "prompt": {"type": "string"}, "messages": {"type": "array"}}, "required": ["model"]}}
    ])
}

fn text(t: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": t.into()}], "isError": false})
}

fn error(t: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": t.into()}], "isError": true})
}

fn gb(v: &Value) -> String {
    v.as_f64().map(|b| format!("{:.1} GB", b / (1u64 << 30) as f64)).unwrap_or_else(|| "?".into())
}

/// Replace Mac shared-folder paths in a free-form command string with PC paths.
pub fn translate_inline(mapper: &PathMapper, cmd: &str) -> String {
    let mut out = cmd.to_string();
    let mut folders = mapper.folders.clone();
    folders.sort_by_key(|f| std::cmp::Reverse(f.mac_path.len()));
    for f in folders {
        let re = regex::Regex::new(&format!(r"{}(/[^\s'\x22]*)?", regex::escape(&f.mac_path))).unwrap();
        out = re
            .replace_all(&out, |c: &regex::Captures| {
                mapper.mac_to_pc(Path::new(&c[0])).unwrap_or_else(|| c[0].to_string())
            })
            .into_owned();
    }
    out
}

pub fn call_tool(engine: &Engine, name: &str, args: &Value) -> Value {
    let mapper = engine.mapper();
    let run_words = |words: Vec<String>, cwd: &Path, force: Option<Target>| -> Value {
        let Some((tool, rest)) = words.split_first() else { return error("Empty command") };
        let mut io = Capture::default();
        let r = engine.run(tool, rest, cwd, force, &mut io);
        let where_ = if r.target == Target::Pc { "the PC" } else { "the Mac" };
        let mut msg = format!("Ran on {where_} — exit code {} — {:.1}s\nWhy: {}\n", r.exit_code, r.seconds, r.reason);
        if !r.outputs.is_empty() {
            msg.push_str("Output files (on the Mac):\n");
            for o in &r.outputs {
                msg.push_str(&format!("  {}\n", o.display()));
            }
        }
        for n in &io.notes {
            msg.push_str(&format!("Note: {n}\n"));
        }
        if let Some(p) = &r.missing {
            msg.push_str(&format!(
                "Note: {p} isn't installed on the PC. Ask the user, then install it with pc_install (program \"{p}\").\n"
            ));
        }
        if !io.stdout.is_empty() {
            msg.push_str(&format!("\n--- stdout ---\n{}", tail(&io.stdout)));
        }
        if !io.stderr.is_empty() {
            msg.push_str(&format!("\n--- stderr ---\n{}", tail(&io.stderr)));
        }
        if r.exit_code == 0 {
            text(msg)
        } else {
            error(msg)
        }
    };

    match name {
        "pc_run" => {
            let force = if str_arg(args, "mode") == Some("auto") { None } else { Some(Target::Pc) };
            run_words(words_arg(args, "command"), &cwd_arg(args), force)
        }
        "pc_docker" => {
            let mut w = vec!["docker".to_string()];
            w.extend(words_arg(args, "args"));
            run_words(w, &cwd_arg(args), Some(Target::Pc))
        }
        "pc_node_build" => {
            let tool = str_arg(args, "tool").unwrap_or("npm");
            if !["npm", "pnpm", "npx", "node"].contains(&tool) {
                return error("tool must be npm, pnpm, npx or node");
            }
            let mut w = vec![tool.to_string()];
            w.extend(words_arg(args, "args"));
            run_words(w, &cwd_arg(args), Some(Target::Pc))
        }
        "pc_run_task" => {
            let Some(cmd) = str_arg(args, "command") else { return error("command is required") };
            let cwd = cwd_arg(args);
            let pc_cmd = translate_inline(&mapper, cmd);
            let Some(client) = &engine.client else {
                return error("Not paired with a PC yet. Open PC Bridge to set it up.");
            };
            let cwd_rel = mapper.mac_to_rel(&cwd).unwrap_or_else(|| "workspace".into());
            let req = crate::client::RunRequest {
                argv: vec!["powershell".into(), "-NoProfile".into(), "-Command".into(), pc_cmd],
                cwd: cwd_rel,
                name: Some(cmd.chars().take(40).collect()),
                gpu: true,
                cache: false,
                ..Default::default()
            };
            let mut log = String::new();
            match client.run(&req, |ev| {
                if let Some(d) = ev["data"].as_str() {
                    log.push_str(&mapper.rewrite_output(d));
                }
            }) {
                Ok(o) => {
                    let outs: Vec<String> = o
                        .outputs
                        .iter()
                        .filter_map(|r| mapper.rel_to_mac(r))
                        .map(|p| p.display().to_string())
                        .collect();
                    let msg = format!(
                        "Ran on the PC — exit code {}\nOutputs: {}\n\n{}",
                        o.exit_code,
                        outs.join(", "),
                        tail(&log)
                    );
                    if o.exit_code == 0 {
                        text(msg)
                    } else {
                        error(msg)
                    }
                }
                Err(e) => error(e.to_string()),
            }
        }
        "pc_delegate_task" => {
            let Some(task) = str_arg(args, "task") else { return error("task is required") };
            let Some(client) = &engine.client else { return error("Not paired with a PC yet.") };
            let files: Vec<String> =
                words_arg(args, "files").iter().filter_map(|f| mapper.mac_to_rel(Path::new(f))).collect();
            let mut steps = 0;
            match client.delegate(task, &files, |ev| {
                if ev["type"] == "claude" {
                    steps += 1;
                }
            }) {
                Ok(o) => {
                    let msg = format!(
                        "Worker Claude on the PC finished (exit {}, {:.0}s, {steps} events).\nJob folder on the PC: {}\n\nResult:\n{}",
                        o.exit_code, o.duration, o.folder.unwrap_or_default(), o.result_text.unwrap_or_else(|| "(no result text)".into())
                    );
                    if o.exit_code == 0 {
                        text(msg)
                    } else {
                        error(msg)
                    }
                }
                Err(e) => error(e.to_string()),
            }
        }
        "pc_status" | "pc_memory" => {
            let mac = memory::local_memory();
            let pc = engine.client.as_ref().map(|c| c.status());
            let mut msg = format!(
                "Mac: {} free of {} (memory pressure: {:?})\n",
                gb(&json!(mac.available_bytes)),
                gb(&json!(mac.total_bytes)),
                mac.pressure
            );
            match pc {
                Some(Ok(s)) => {
                    msg.push_str(&format!(
                        "PC:  {} free of {}\n",
                        gb(&s["memory"]["available_bytes"]),
                        gb(&s["memory"]["total_bytes"])
                    ));
                    if name == "pc_status" {
                        if let Some(line) = programs_summary(&s) {
                            msg.push_str(&format!("{line}\n"));
                        }
                        msg.push_str(&format!("\nPC status:\n{}", serde_json::to_string_pretty(&s).unwrap()));
                    }
                    text(msg)
                }
                Some(Err(e)) => {
                    msg.push_str(&format!("PC: not reachable ({e})"));
                    error(msg)
                }
                None => error(msg + "PC: not paired yet"),
            }
        }
        "pc_install" => {
            let Some(program) = str_arg(args, "program").map(|p| p.trim().to_lowercase()).filter(|p| !p.is_empty())
            else {
                return error(format!("program is required (one of: {})", KNOWN_PROGRAMS.join(", ")));
            };
            let Some(client) = &engine.client else {
                return error("Not paired with a PC yet. Open PC Bridge to set it up.");
            };
            if engine.ensure_awake(&mut Capture::default()).is_none() {
                return error("Can't reach the PC right now. Check it's on and PC Bridge is running there.");
            }
            let mut log = String::new();
            match client.install_program(&program, |ev| {
                if let Some(d) = ev["data"].as_str() {
                    log.push_str(d);
                }
            }) {
                Ok(o) if o.exit_code == 0 && o.restart_needed => text(format!(
                    "{program} is installed on the PC, but the PC must restart to finish the install. \
                     Ask the user to restart the PC before using it.\n\n{}",
                    last_lines(&log, 10)
                )),
                Ok(o) if o.exit_code == 0 => {
                    text(format!("{program} is now installed on the PC ({:.0}s).\n\n{}", o.duration, last_lines(&log, 10)))
                }
                Ok(o) => error(format!(
                    "Installing {program} on the PC failed (exit code {}). If Windows showed a \"Yes\" prompt on the PC, \
                     someone has to click it there.\nLast lines from the installer:\n{}",
                    o.exit_code,
                    last_lines(&log, 15)
                )),
                Err(e) => error(format!("Couldn't install {program} on the PC: {e}")),
            }
        }
        "pc_gpu" => match engine.client.as_ref().map(|c| c.gpu()) {
            Some(Ok(g)) => text(serde_json::to_string_pretty(&g).unwrap()),
            Some(Err(e)) => error(e.to_string()),
            None => error("Not paired with a PC yet."),
        },
        "pc_route_advice" => {
            let words = words_arg(args, "command");
            let Some((tool, rest)) = words.split_first() else { return error("Empty command") };
            let cwd = cwd_arg(args);
            match plan(&mapper, &tool.to_lowercase(), rest, &cwd) {
                Err(why) => text(format!("Mac — {why}")),
                Ok(p) => {
                    let st = engine.client.as_ref().and_then(|c| c.status().ok());
                    let d = router::decide(
                        &RouteInput {
                            tool: tool.to_lowercase(),
                            class: p.class.clone(),
                            input_bytes: p.input_bytes,
                            mac_mem: memory::local_memory(),
                            pc_free_ram: st.as_ref().and_then(|s| s["memory"]["available_bytes"].as_u64()),
                            pc_reachable: st.is_some(),
                            pc_awake: st.is_some(),
                            pc_queue_depth: st.as_ref().and_then(|s| s["queue_depth"].as_u64()).unwrap_or(0) as u32,
                            measured_mbps: engine.cfg.measured_mbps,
                            redirect_enabled: engine.cfg.redirect_enabled,
                        },
                        engine.history.as_ref(),
                    );
                    text(format!(
                        "{:?} — {}\nEstimated: Mac ~{:.0}s, PC ~{:.0}s; job type {:?}, needs ~{:.1} GB RAM",
                        d.target,
                        d.reason,
                        d.mac_est_secs,
                        d.pc_est_secs,
                        p.class.kind,
                        p.class.est_ram_bytes as f64 / (1u64 << 30) as f64
                    ))
                }
            }
        }
        "pc_push_file" => {
            let Some(mac) = str_arg(args, "mac_path") else { return error("mac_path is required") };
            let Some(client) = &engine.client else { return error("Not paired with a PC yet.") };
            let p = Path::new(mac);
            let rel =
                str_arg(args, "pc_rel_path").map(String::from).or_else(|| mapper.mac_to_rel(p)).unwrap_or_else(|| {
                    format!(
                        "workspace/_uploads/{}",
                        p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
                    )
                });
            match client.push_file(p, &rel) {
                Ok(v) => text(format!("Copied to the PC: {}", mapper.rel_to_pc(v["path"].as_str().unwrap_or(&rel)))),
                Err(e) => error(e.to_string()),
            }
        }
        "pc_pull_file" => {
            let (Some(pc), Some(mac)) = (str_arg(args, "pc_path"), str_arg(args, "mac_path")) else {
                return error("pc_path and mac_path are required");
            };
            let Some(client) = &engine.client else { return error("Not paired with a PC yet.") };
            let root = engine.cfg.pc_root.trim_end_matches('\\').to_lowercase();
            let rel = if pc.to_lowercase().starts_with(&root) {
                pc[root.len()..].trim_start_matches(['\\', '/']).replace('\\', "/")
            } else {
                pc.replace('\\', "/")
            };
            match client.pull_file(&rel, Path::new(mac)) {
                Ok(n) => text(format!("Copied {n} bytes to {mac}")),
                Err(e) => error(e.to_string()),
            }
        }
        "pc_wake" => {
            let mut io = Capture::default();
            match engine.ensure_awake(&mut io) {
                Some((_, true)) => text("The PC is already awake."),
                Some((_, false)) => text("The PC woke up and is ready."),
                None => error("Couldn't wake the PC. Wake-on-LAN only works when the Mac is on the same home network; check the PC is plugged in and Wake-on-LAN is enabled."),
            }
        }
        "comfy_generate" => match (&engine.client, args.get("workflow")) {
            (Some(c), Some(w)) => match c.comfy_generate(w.clone()) {
                Ok(v) => text(serde_json::to_string_pretty(&v).unwrap()),
                Err(e) => error(e.to_string()),
            },
            (None, _) => error("Not paired with a PC yet."),
            _ => error("workflow is required"),
        },
        "ollama_chat" => {
            let Some(model) = str_arg(args, "model") else { return error("model is required") };
            let Some(c) = &engine.client else { return error("Not paired with a PC yet.") };
            let messages = args
                .get("messages")
                .cloned()
                .unwrap_or_else(|| json!([{"role": "user", "content": str_arg(args, "prompt").unwrap_or("")}]));
            match c.ollama_chat(model, messages) {
                Ok(v) => text(v["message"]["content"].as_str().map(String::from).unwrap_or_else(|| v.to_string())),
                Err(e) => error(e.to_string()),
            }
        }
        other => error(format!("Unknown tool: {other}")),
    }
}

/// Handle one JSON-RPC message. Returns None for notifications.
pub fn handle(engine: &Engine, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let result = match method {
        "initialize" => {
            let requested = msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION);
            json!({
                "protocolVersion": requested,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "pc-bridge", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "PC Bridge sends heavy work (GPU encodes, AI inference, renders, RAM-heavy builds) from this Mac to the user's Windows PC (NVIDIA GPU, usually much more free RAM than the Mac; pc_gpu and pc_memory give exact figures). Prefer pc_run for single commands and pc_delegate_task for multi-step GPU workflows. Keep light edits and git on the Mac. If a PC job fails because a program is missing there, ask the user before installing it with pc_install."
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tool_defs()}),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
            call_tool(engine, name, &args)
        }
        _ if id.is_none() => return None, // notification (e.g. notifications/initialized)
        _ => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Method not found: {method}")}}),
            );
        }
    };
    id.map(|id| json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

pub fn serve_stdio(engine: &Engine) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(engine, &msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}})),
        };
        if let Some(r) = reply {
            writeln!(stdout, "{r}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BridgeConfig, SharedFolder};
    use std::time::Duration;

    fn engine() -> Engine {
        let mut cfg = BridgeConfig::default();
        cfg.shared_folders.push(SharedFolder { mac_path: "/Users/w/Videos".into(), name: "Videos".into() });
        Engine { cfg, client: None, history: None, mac_mem: None, local: |_, _, _| 0, wake_timeout: Duration::ZERO }
    }

    #[test]
    fn splits_commands() {
        assert_eq!(
            split_command(r#"ffmpeg -i "my file.mov" -vf 'scale=1280:-1' out\ 1.mp4"#),
            vec!["ffmpeg", "-i", "my file.mov", "-vf", "scale=1280:-1", "out 1.mp4"]
        );
        assert_eq!(split_command("  a   b "), vec!["a", "b"]);
        assert_eq!(split_command(r#"echo """#), vec!["echo", ""]);
    }

    #[test]
    fn protocol_handshake_and_list() {
        let e = engine();
        let init = handle(
            &e,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}),
        )
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "pc-bridge");
        assert!(handle(&e, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        let list = handle(&e, &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        let names: Vec<&str> =
            list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        for n in [
            "pc_run",
            "pc_delegate_task",
            "pc_status",
            "pc_gpu",
            "pc_push_file",
            "pc_pull_file",
            "pc_wake",
            "comfy_generate",
            "ollama_chat",
            "pc_run_task",
            "pc_docker",
            "pc_node_build",
            "pc_memory",
            "pc_route_advice",
            "pc_install",
        ] {
            assert!(names.contains(&n), "missing {n}");
        }
        assert_eq!(names.len(), 15, "{names:?}");
        let install = list["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "pc_install").unwrap();
        let desc = install["description"].as_str().unwrap();
        assert!(desc.contains("ASK THE USER FIRST") && desc.contains("never in the shared folders"), "{desc}");
        for k in KNOWN_PROGRAMS {
            assert!(desc.contains(k), "pc_install description should list {k}");
        }
        assert_eq!(install["inputSchema"]["required"], json!(["program"]));
        let bad = handle(&e, &json!({"jsonrpc": "2.0", "id": 3, "method": "nope"})).unwrap();
        assert_eq!(bad["error"]["code"], -32601);
    }

    #[test]
    fn unpaired_tools_explain_themselves() {
        let e = engine();
        let r = handle(
            &e,
            &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "pc_gpu", "arguments": {}}}),
        )
        .unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("Not paired"));
        let r = call_tool(&e, "pc_node_build", &json!({"tool": "rm", "args": "-rf /"}));
        assert_eq!(r["isError"], true);
        let r = call_tool(&e, "pc_install", &json!({"program": "ffmpeg"}));
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("Not paired"));
        let r = call_tool(&e, "pc_install", &json!({"program": "  "}));
        assert!(r["content"][0]["text"].as_str().unwrap().contains("program is required"));
    }

    #[test]
    fn programs_summary_from_status_tools() {
        let s = json!({"tools": {"ffmpeg": true, "blender": false, "node": true, "npm": true, "claude": true}});
        assert_eq!(
            programs_summary(&s).unwrap(),
            "Programs on the PC: installed: ffmpeg, node · not installed: blender (pc_install can add them, after asking the user)"
        );
        let s = json!({"tools": {"ffmpeg": true, "torch": true}});
        assert_eq!(
            programs_summary(&s).unwrap(),
            "Programs on the PC: installed: ffmpeg, pytorch · not installed: none"
        );
        // What the 0.1.3 agent sends: Blender found in its install folder, plus PyTorch and Git.
        let s = json!({"tools": {"ffmpeg": true, "blender": true, "pytorch": false, "git": false, "node": true}});
        assert_eq!(
            programs_summary(&s).unwrap(),
            "Programs on the PC: installed: ffmpeg, blender, node · not installed: pytorch, git (pc_install can add them, after asking the user)"
        );
        assert!(programs_summary(&json!({"tools": {"claude": true}})).is_none());
        assert!(programs_summary(&json!({})).is_none());
        assert_eq!(last_lines("a\n\nb\nc\n", 2), "b\nc");
    }

    #[test]
    fn inline_translation() {
        let e = engine();
        let m = e.mapper();
        assert_eq!(
            translate_inline(&m, "magick /Users/w/Videos/a.png -resize 50% /Users/w/Videos/out/b.png"),
            r"magick D:\PCBridge\workspace\Videos\a.png -resize 50% D:\PCBridge\workspace\Videos\out\b.png"
        );
        assert_eq!(translate_inline(&m, "echo /Users/w/Desktop/x"), "echo /Users/w/Desktop/x");
    }
}
