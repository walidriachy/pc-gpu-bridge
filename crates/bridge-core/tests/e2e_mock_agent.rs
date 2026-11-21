#![allow(clippy::field_reassign_with_default)]
//! End-to-end: real shim engine + HTTP client against a mock PC agent.

use bridge_core::client::AgentClient;
use bridge_core::config::{BridgeConfig, SharedFolder};
use bridge_core::memory::{MemoryInfo, Pressure};
use bridge_core::router::Target;
use bridge_core::shim::{Capture, Engine};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Seen {
    run_bodies: Vec<Value>,
    install_bodies: Vec<Value>,
    pushed: Vec<usize>,
    auth_ok: bool,
}

/// How the mock answers POST /run.
#[derive(Clone, Copy, PartialEq)]
enum RunMode {
    /// The job runs and makes out.mp4.
    Ok,
    /// The PC accepts the job but the program isn't installed there (exit 127 + "missing" event).
    Missing,
    /// The PC refuses before accepting the job (HTTP 500, no events).
    Refuse,
    /// A newer agent answers from its result cache, saying so in the exit event.
    Cached,
}

fn start_mock() -> (String, Arc<Mutex<Seen>>) {
    start_mock_with(RunMode::Ok)
}

fn start_mock_with(mode: RunMode) -> (String, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let seen = s2.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let parts: Vec<&str> = line.split_whitespace().collect();
                let (method, target) = (parts[0].to_string(), parts[1].to_string());
                let mut len = 0usize;
                let mut authed = false;
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    if h == "\r\n" || h.is_empty() {
                        break;
                    }
                    let lower = h.to_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    if lower.starts_with("authorization: bearer secret") {
                        seen.lock().unwrap().auth_ok = true;
                        authed = true;
                    }
                    if lower.starts_with("authorization: bearer ") && !authed {
                        authed = !lower.starts_with("authorization: bearer wrong");
                    }
                }
                let mut body = vec![0u8; len];
                reader.read_exact(&mut body).unwrap();
                let path = target.split('?').next().unwrap();
                let ndjson = |evs: &[Value]| evs.iter().map(|e| e.to_string() + "\n").collect::<String>().into_bytes();
                let mut status_line = "200 OK";
                let (ctype, resp): (&str, Vec<u8>) = match (method.as_str(), path) {
                    _ if !authed => {
                        status_line = "401 Unauthorized";
                        ("application/json", br#"{"detail":"not paired"}"#.to_vec())
                    }
                    ("GET", "/status") => (
                        "application/json",
                        json!({
                            "ok": true, "queue_depth": 0,
                            "memory": {"total_bytes": 34359738368u64, "available_bytes": 30000000000u64},
                            "tools": {"ffmpeg": true, "ffprobe": true, "blender": false, "node": true, "claude": true}
                        })
                        .to_string()
                        .into_bytes(),
                    ),
                    ("GET", "/programs") => (
                        "application/json",
                        json!({
                            "programs": [
                                {"key": "ffmpeg", "name": "FFmpeg", "why": "video", "size_gb": 0.3, "installed": true},
                                {"key": "blender", "name": "Blender", "why": "3D", "size_gb": 1.0, "installed": false}
                            ],
                            "free_gb": 120.5, "drive": "C:\\"
                        })
                        .to_string()
                        .into_bytes(),
                    ),
                    ("POST", "/programs/install") => {
                        let b: Value = serde_json::from_slice(&body).unwrap();
                        seen.lock().unwrap().install_bodies.push(b.clone());
                        match b["program"].as_str().unwrap_or("") {
                            "ffmpeg" => (
                                "application/x-ndjson",
                                ndjson(&[
                                    json!({"type": "job", "id": "inst1", "folder": r"D:\PCBridge\jobs\i"}),
                                    json!({"type": "started"}),
                                    json!({"type": "stdout", "data": "Found FFmpeg [Gyan.FFmpeg]\n"}),
                                    json!({"type": "stdout", "data": "Successfully installed\n"}),
                                    json!({"type": "exit", "code": 0, "state": "done", "duration": 12.0, "outputs": []}),
                                ]),
                            ),
                            "git" => (
                                "application/x-ndjson",
                                ndjson(&[
                                    json!({"type": "job", "id": "inst2", "folder": r"D:\PCBridge\jobs\g"}),
                                    json!({"type": "stderr", "data": "Installer failed: the user said no\n"}),
                                    json!({"type": "exit", "code": 1, "state": "failed", "duration": 3.0, "outputs": []}),
                                ]),
                            ),
                            "docker" => (
                                "application/x-ndjson",
                                ndjson(&[
                                    json!({"type": "job", "id": "inst3", "folder": r"D:\PCBridge\jobs\d"}),
                                    json!({"type": "stdout", "data": "PC Bridge: installed. Restart the PC to finish the install.\n"}),
                                    json!({"type": "restart_needed"}),
                                    json!({"type": "exit", "code": 0, "state": "done", "duration": 300.0, "outputs": []}),
                                ]),
                            ),
                            "blender" => {
                                status_line = "507 Insufficient Storage";
                                (
                                    "application/json",
                                    json!({"detail": "Not enough space on the PC's C:\\ drive for Blender: 1.0 GB free, about 3 GB needed"})
                                        .to_string()
                                        .into_bytes(),
                                )
                            }
                            other => {
                                status_line = "404 Not Found";
                                (
                                    "application/json",
                                    json!({"detail": format!("PC Bridge doesn't know how to install '{other}'")})
                                        .to_string()
                                        .into_bytes(),
                                )
                            }
                        }
                    }
                    ("GET", "/files/stat") => {
                        let body = if target.contains("out.mp4") {
                            json!({"exists": true, "size": 3})
                        } else {
                            json!({"exists": false})
                        };
                        ("application/json", body.to_string().into_bytes())
                    }
                    ("POST", "/files/push") => {
                        seen.lock().unwrap().pushed.push(body.len());
                        (
                            "application/json",
                            json!({"ok": true, "path": "workspace/w/in.mov", "bytes": 10}).to_string().into_bytes(),
                        )
                    }
                    ("POST", "/run") if mode == RunMode::Refuse => {
                        seen.lock().unwrap().run_bodies.push(serde_json::from_slice(&body).unwrap());
                        status_line = "500 Internal Server Error";
                        ("application/json", b"{\"detail\":\"agent is restarting\"}".to_vec())
                    }
                    ("POST", "/run") if mode == RunMode::Missing => {
                        seen.lock().unwrap().run_bodies.push(serde_json::from_slice(&body).unwrap());
                        (
                            "application/x-ndjson",
                            ndjson(&[
                                json!({"type": "job", "id": "miss1", "folder": r"D:\PCBridge\jobs\m"}),
                                json!({"type": "started"}),
                                json!({"type": "stderr", "data": "PC Bridge: program not found on the PC: ffmpeg\n"}),
                                json!({"type": "stderr", "data": "PC Bridge: FFmpeg isn't installed on the PC. Ask Claude to install it (tool pc_install, program \"ffmpeg\"), or on the PC open PC Bridge → Settings → Programs → Install.\n"}),
                                json!({"type": "missing", "program": "ffmpeg", "name": "FFmpeg"}),
                                json!({"type": "exit", "code": 127, "state": "failed", "duration": 0.1, "outputs": []}),
                            ]),
                        )
                    }
                    ("POST", "/run") if mode == RunMode::Cached => {
                        seen.lock().unwrap().run_bodies.push(serde_json::from_slice(&body).unwrap());
                        (
                            "application/x-ndjson",
                            ndjson(&[
                                json!({"type": "job", "id": "c1", "folder": r"D:\PCBridge\jobs\c"}),
                                json!({"type": "exit", "code": 0, "state": "done", "duration": 0, "queue_secs": 0,
                                       "cached": true, "outputs": ["workspace/w/out.mp4"]}),
                            ]),
                        )
                    }
                    ("POST", "/run") => {
                        seen.lock().unwrap().run_bodies.push(serde_json::from_slice(&body).unwrap());
                        // Agent 0.1.3: no queue_secs in the exit event, only event timestamps.
                        let evs = [
                            json!({"type": "job", "id": "abc123", "folder": r"D:\PCBridge\jobs\x", "t": 99.9}),
                            json!({"type": "queued", "position": 1, "t": 100.0}),
                            json!({"type": "started", "t": 100.75}),
                            json!({"type": "stderr", "data": "Output #0, mp4, to 'D:\\PCBridge\\workspace\\w\\out.mp4':\n"}),
                            json!({"type": "exit", "code": 0, "state": "done", "duration": 1.5, "outputs": ["workspace/w/out.mp4"]}),
                        ];
                        (
                            "application/x-ndjson",
                            evs.iter().map(|e| e.to_string() + "\n").collect::<String>().into_bytes(),
                        )
                    }
                    ("GET", "/files/pull") => ("application/octet-stream", b"abc".to_vec()),
                    _ => ("application/json", b"{\"detail\":\"nope\"}".to_vec()),
                };
                let head = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    resp.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&resp).unwrap();
            });
        }
    });
    (addr, seen)
}

#[test]
fn ffmpeg_shim_round_trip_through_mock_agent() {
    let (url, seen) = start_mock();
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("work");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("in.mov"), b"0123456789").unwrap();

    let mut cfg = BridgeConfig::default();
    cfg.shared_folders = vec![SharedFolder { mac_path: ws.display().to_string(), name: "w".into() }];
    cfg.sync_wait_secs = 0;
    cfg.notify_on_fallback = false;
    let engine = Engine {
        cfg,
        client: Some(AgentClient::new(url, Some("secret".into()))),
        history: Some(bridge_core::router::History::in_memory().unwrap()),
        mac_mem: Some(MemoryInfo {
            total_bytes: 16 << 30,
            available_bytes: 8 << 30,
            used_percent: 50.0,
            pressure: Pressure::Normal,
        }),
        local: |_, _, _| panic!("should not run locally"),
        wake_timeout: Duration::ZERO,
    };
    let args: Vec<String> = ["-i", "in.mov", "-c:v", "libx264", "out.mp4"].iter().map(|s| s.to_string()).collect();
    let mut io = Capture::default();
    let r = engine.run("ffmpeg", &args, &ws, None, &mut io);

    assert_eq!(r.target, Target::Pc, "reason: {}", r.reason);
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.job_id.as_deref(), Some("abc123"));
    // Output pulled back to the Mac folder
    assert_eq!(std::fs::read(ws.join("out.mp4")).unwrap(), b"abc");
    assert_eq!(r.outputs, vec![ws.join("out.mp4")]);
    // PC paths in tool output rewritten to Mac paths
    // (Separators normalised: on Windows CI the simulated "Mac" folder is a Windows temp path.)
    let want = ws.join("out.mp4").display().to_string().replace('\\', "/");
    assert!(io.stderr.replace('\\', "/").contains(&want), "{}", io.stderr);
    let seen = seen.lock().unwrap();
    assert!(seen.auth_ok);
    assert_eq!(seen.pushed.len(), 1, "input should be pushed when missing on the PC");
    let argv: Vec<String> = serde_json::from_value(seen.run_bodies[0]["argv"].clone()).unwrap();
    assert_eq!(
        argv,
        vec![
            "ffmpeg",
            "-i",
            r"D:\PCBridge\workspace\w\in.mov",
            "-c:v",
            "h264_nvenc",
            // New outputs keep their relative name; the PC runs in the same shared folder.
            "out.mp4"
        ]
    );
    assert_eq!(seen.run_bodies[0]["cwd"], "workspace/w");
    assert_eq!(seen.run_bodies[0]["gpu"], true);
    // history recorded
    assert_eq!(engine.history.as_ref().unwrap().recent(5).len(), 1);
}

#[test]
fn unreachable_pc_falls_back_locally() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("in.mov"), b"x").unwrap();
    let mut cfg = BridgeConfig::default();
    cfg.shared_folders = vec![SharedFolder { mac_path: dir.path().display().to_string(), name: "w".into() }];
    let engine = Engine {
        cfg,
        client: Some(AgentClient::new("http://127.0.0.1:9", Some("t".into()))),
        history: None,
        mac_mem: None,
        local: |_, _, _| 7,
        wake_timeout: Duration::ZERO,
    };
    let mut io = Capture::default();
    let args: Vec<String> = ["-i", "in.mov", "o.mp4"].iter().map(|s| s.to_string()).collect();
    let r = engine.run("ffmpeg", &args, dir.path(), None, &mut io);
    assert_eq!((r.target, r.exit_code), (Target::Mac, 7));
    assert_eq!(io.notes.len(), 1);
}

fn engine_for(url: &str, dir: &std::path::Path, local: bridge_core::shim::LocalRunner) -> Engine {
    let mut cfg = BridgeConfig::default();
    cfg.shared_folders = vec![SharedFolder { mac_path: dir.display().to_string(), name: "w".into() }];
    cfg.sync_wait_secs = 0;
    cfg.notify_on_fallback = false;
    Engine {
        cfg,
        client: Some(AgentClient::new(url, Some("secret".into()))),
        history: None,
        mac_mem: Some(MemoryInfo {
            total_bytes: 16 << 30,
            available_bytes: 8 << 30,
            used_percent: 50.0,
            pressure: Pressure::Normal,
        }),
        local,
        wake_timeout: Duration::ZERO,
    }
}

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn missing_program_on_pc_never_runs_on_the_mac() {
    let (url, seen) = start_mock_with(RunMode::Missing);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("in.mov"), b"0123456789").unwrap();
    let engine = engine_for(&url, dir.path(), |_, _, _| panic!("must not fall back to the Mac"));
    // Forced to the PC (pc_run) and left to the router (shim): both report the PC's answer.
    for force in [Some(Target::Pc), None] {
        let mut io = Capture::default();
        let r =
            engine.run("ffmpeg", &args(&["-i", "in.mov", "-c:v", "libx264", "out.mp4"]), dir.path(), force, &mut io);
        assert_eq!((r.target, r.exit_code), (Target::Pc, 127), "reason: {}", r.reason);
        assert_eq!(r.missing.as_deref(), Some("ffmpeg"));
        assert_eq!(r.job_id.as_deref(), Some("miss1"));
        assert!(io.stderr.contains("pc_install"), "{}", io.stderr);
        assert!(io.notes.is_empty(), "{:?}", io.notes);
    }
    assert_eq!(seen.lock().unwrap().run_bodies.len(), 2);

    // Through MCP, Claude is told to ask the user and use pc_install.
    let r = bridge_core::mcp::call_tool(
        &engine,
        "pc_run",
        &json!({"command": "ffmpeg -i in.mov -c:v libx264 out.mp4", "cwd": dir.path().display().to_string()}),
    );
    assert_eq!(r["isError"], true);
    let t = r["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("Ran on the PC") && t.contains("pc_install (program \"ffmpeg\")"), "{t}");
}

#[test]
fn refused_before_acceptance_still_falls_back_unless_forced() {
    let (url, _seen) = start_mock_with(RunMode::Refuse);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("in.mov"), b"0123456789").unwrap();
    let engine = engine_for(&url, dir.path(), |_, _, _| 7);
    let a = args(&["-i", "in.mov", "-c:v", "libx264", "out.mp4"]);
    let mut io = Capture::default();
    let r = engine.run("ffmpeg", &a, dir.path(), None, &mut io);
    assert_eq!((r.target, r.exit_code), (Target::Mac, 7), "reason: {}", r.reason);
    assert!(r.missing.is_none());
    let r = engine.run("ffmpeg", &a, dir.path(), Some(Target::Pc), &mut io);
    assert_eq!((r.target, r.exit_code), (Target::Pc, 3), "reason: {}", r.reason);
}

#[test]
fn install_programs_through_client_and_mcp() {
    let (url, seen) = start_mock();
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_for(&url, dir.path(), |_, _, _| panic!("no local runs"));
    let client = engine.client.clone().unwrap();

    let p = client.programs().unwrap();
    assert_eq!(p["programs"][0]["key"], "ffmpeg");
    assert_eq!(p["programs"][1]["installed"], false);

    let mut events = 0;
    let o = client.install_program("ffmpeg", |_| events += 1).unwrap();
    assert_eq!((o.exit_code, o.job_id.as_deref()), (0, Some("inst1")));
    assert_eq!(events, 5);
    assert!(!o.restart_needed);

    // Installed, but Windows must restart to finish: Claude is told to ask for a restart.
    assert!(client.install_program("docker", |_| {}).unwrap().restart_needed);
    let r = bridge_core::mcp::call_tool(&engine, "pc_install", &json!({"program": "docker"}));
    assert_eq!(r["isError"], false, "{r}");
    let t = r["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("must restart") && t.contains("restart the PC"), "{t}");

    let r = bridge_core::mcp::call_tool(&engine, "pc_install", &json!({"program": " FFmpeg "}));
    assert_eq!(r["isError"], false, "{r}");
    let t = r["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("ffmpeg is now installed on the PC") && t.contains("Successfully installed"), "{t}");

    let r = bridge_core::mcp::call_tool(&engine, "pc_install", &json!({"program": "git"}));
    assert_eq!(r["isError"], true);
    let t = r["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("exit code 1") && t.contains("the user said no") && t.contains("\"Yes\""), "{t}");

    // Low disk space (507): the PC's own sentence reaches Claude.
    let r = bridge_core::mcp::call_tool(&engine, "pc_install", &json!({"program": "blender"}));
    assert_eq!(r["isError"], true);
    let t = r["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("Not enough space") && !t.contains("HTTP 507"), "{t}");

    let r = bridge_core::mcp::call_tool(&engine, "pc_install", &json!({"program": "photoshop"}));
    assert!(r["content"][0]["text"].as_str().unwrap().contains("doesn't know how to install"), "{r}");

    // The MCP tool trims and lowercases the name before sending it.
    assert_eq!(seen.lock().unwrap().install_bodies[1], json!({"program": "docker", "stream": true}));
    assert_eq!(seen.lock().unwrap().install_bodies[3], json!({"program": "ffmpeg", "stream": true}));

    // pc_status lists which known programs are installed.
    let r = bridge_core::mcp::call_tool(&engine, "pc_status", &json!({}));
    let t = r["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("Programs on the PC: installed: ffmpeg, node · not installed: blender"), "{t}");
}

fn learning_engine(url: &str, dir: &std::path::Path, local: bridge_core::shim::LocalRunner) -> Engine {
    let mut e = engine_for(url, dir, local);
    e.history = Some(bridge_core::router::History::in_memory().unwrap());
    e
}

#[test]
fn pc_rows_store_agent_compute_time_and_steer_the_router() {
    let (url, seen) = start_mock();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("in.mov"), b"0123456789").unwrap();
    let engine = learning_engine(&url, dir.path(), |_, _, _| 0);
    let a = args(&["-i", "in.mov", "-c:v", "libx264", "out.mp4"]);
    for _ in 0..3 {
        let r = engine.run("ffmpeg", &a, dir.path(), None, &mut Capture::default());
        assert_eq!(r.target, Target::Pc, "defaults send it to the PC: {}", r.reason);
    }
    let h = engine.history.as_ref().unwrap();
    for row in h.recent(3) {
        assert_eq!(row["target"], "pc");
        assert_eq!(row["signature"], "ffmpeg:libx264");
        assert_eq!(row["pc_job_id"], "abc123");
        // The agent's run time, not the Mac-side round trip.
        assert_eq!(row["compute_secs"].as_f64(), Some(1.5));
        assert!((row["queue_secs"].as_f64().unwrap() - 0.75).abs() < 1e-9, "{row}");
        assert!(row["transfer_secs"].as_f64().unwrap() >= 0.0);
        assert_eq!(row["cached"], false);
        // Agent 0.1.3's duration also counts collecting outputs: kept, but only as an upper bound.
        assert_eq!(row["approx"], true);
    }
    // A tiny input is billed as 1 MB, the same as decide() bills it: 1.5 s/MB, not 1500.
    assert_eq!(h.secs_per_mb("ffmpeg", "ffmpeg:libx264", Target::Pc), Some(1.5));
    let r = engine.run("ffmpeg", &a, dir.path(), None, &mut Capture::default());
    assert_eq!(r.target, Target::Pc, "{}", r.reason);
    let d = r.decision.unwrap();
    assert!(d.pc_est_secs < 5.0 && d.pc_est_secs < d.mac_est_secs, "{d:?}");
    // The Mac turned out much faster for this exact job: after 3 Mac runs the router keeps it local.
    for _ in 0..3 {
        h.record(&bridge_core::router::JobRecord {
            signature: "ffmpeg:libx264",
            ..bridge_core::router::JobRecord::new(
                "ffmpeg",
                bridge_core::detect::JobKind::Gpu,
                Target::Mac,
                13,
                0.01,
                0,
                "ffmpeg",
            )
        })
        .unwrap();
        h.record(&bridge_core::router::JobRecord {
            signature: "ffmpeg:libx264",
            transfer_secs: 9.0,
            ..bridge_core::router::JobRecord::new(
                "ffmpeg",
                bridge_core::detect::JobKind::Gpu,
                Target::Pc,
                13,
                1.5,
                0,
                "ffmpeg",
            )
        })
        .unwrap();
    }
    let r = engine.run("ffmpeg", &a, dir.path(), None, &mut Capture::default());
    assert_eq!((r.target, r.exit_code), (Target::Mac, 0), "{}", r.reason);
    assert_eq!(seen.lock().unwrap().run_bodies.len(), 4);
    let mac = &h.recent(1)[0];
    // Same input measure as the router: in.mov plus the out.mp4 the earlier runs brought back.
    assert_eq!((mac["target"].as_str(), mac["input_bytes"].as_i64()), (Some("mac"), Some(13)));
    assert_eq!((mac["signature"].as_str(), mac["approx"].as_bool()), (Some("ffmpeg:libx264"), Some(false)));
}

#[test]
fn cache_hits_are_kept_but_not_learned() {
    let (url, _seen) = start_mock_with(RunMode::Cached);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("in.mov"), b"0123456789").unwrap();
    let engine = learning_engine(&url, dir.path(), |_, _, _| panic!("cache hits stay on the PC"));
    let a = args(&["-i", "in.mov", "-c:v", "libx264", "out.mp4"]);
    for _ in 0..4 {
        let r = engine.run("ffmpeg", &a, dir.path(), None, &mut Capture::default());
        assert_eq!((r.target, r.exit_code), (Target::Pc, 0), "{}", r.reason);
    }
    let h = engine.history.as_ref().unwrap();
    let rows = h.recent(10);
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|r| r["cached"] == true && r["queue_secs"].as_f64() == Some(0.0)), "{rows:?}");
    // This agent sends queue_secs, so its duration is exact compute time.
    assert!(rows.iter().all(|r| r["approx"] == false), "{rows:?}");
    assert_eq!(h.secs_per_mb("ffmpeg", "ffmpeg:libx264", Target::Pc), None);
}

#[test]
fn refused_pc_job_is_learned_as_a_mac_run() {
    let (url, _seen) = start_mock_with(RunMode::Refuse);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("in.mov"), b"0123456789").unwrap();
    let engine = learning_engine(&url, dir.path(), |_, _, _| 0);
    let r = engine.run("ffmpeg", &args(&["-i", "in.mov", "out.mp4"]), dir.path(), None, &mut Capture::default());
    assert_eq!(r.target, Target::Mac);
    let rows = engine.history.as_ref().unwrap().recent(10);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0]["target"].as_str(), rows[0]["signature"].as_str()), (Some("mac"), Some("ffmpeg")));
}

/// Optional cross-language test against the REAL Python agent. CI starts the agent and sets:
/// PCBRIDGE_E2E_URL, PCBRIDGE_E2E_TOKEN, PCBRIDGE_E2E_ROOT.
#[test]
fn real_python_agent_if_available() {
    let (Ok(url), Ok(token), Ok(root)) =
        (std::env::var("PCBRIDGE_E2E_URL"), std::env::var("PCBRIDGE_E2E_TOKEN"), std::env::var("PCBRIDGE_E2E_ROOT"))
    else {
        eprintln!("skipping: real agent not configured");
        return;
    };
    let client = AgentClient::new(url, Some(token));
    let st = client.status().expect("status");
    assert!(st["memory"]["total_bytes"].as_u64().unwrap() > 0);
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("data.txt"), b"hello").unwrap();
    std::fs::write(ws.join("job.py"), "# pcbridge: ram\nimport pathlib\nimport sys\npathlib.Path('result.txt').write_text(pathlib.Path(sys.argv[1]).read_text().upper())\nprint('ok')\n").unwrap();
    let mut cfg = BridgeConfig::default();
    cfg.pc_root = root;
    cfg.shared_folders = vec![SharedFolder { mac_path: ws.display().to_string(), name: "proj".into() }];
    cfg.sync_wait_secs = 0;
    let engine = Engine {
        cfg,
        client: Some(client),
        history: None,
        mac_mem: None,
        local: |_, _, _| 99,
        wake_timeout: Duration::ZERO,
    };
    let py = if cfg!(windows) { "python" } else { "python3" };
    let mut io = Capture::default();
    let r = engine.run(py, &["job.py".to_string(), "data.txt".to_string()], &ws, Some(Target::Pc), &mut io);
    assert_eq!(r.exit_code, 0, "stderr: {} notes: {:?}", io.stderr, io.notes);
    assert!(io.stdout.contains("ok"));
    assert_eq!(std::fs::read_to_string(ws.join("result.txt")).unwrap(), "HELLO");
}

#[test]
fn refused_key_is_reported_without_waking_the_pc() {
    let (url, _seen) = start_mock();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("in.mov"), b"x").unwrap();
    let mut cfg = BridgeConfig::default();
    cfg.shared_folders = vec![SharedFolder { mac_path: dir.path().display().to_string(), name: "w".into() }];
    cfg.pc_mac_address = "00:11:22:33:44:55".into();
    cfg.notify_on_fallback = false;
    let engine = Engine {
        cfg,
        client: Some(AgentClient::new(url, Some("wrong".into()))),
        history: None,
        mac_mem: None,
        local: |_, _, _| 0,
        wake_timeout: Duration::from_secs(60),
    };
    let mut io = Capture::default();
    let start = std::time::Instant::now();
    let err = engine.ensure_awake(&mut io).unwrap_err();
    assert_eq!(err, bridge_core::shim::KEY_REFUSED);
    assert!(start.elapsed() < Duration::from_secs(5), "must not wait for a wake");
    assert!(!io.stderr.contains("waking"), "{}", io.stderr);
    let args: Vec<String> = ["-i", "in.mov", "-c:v", "libx264", "o.mp4"].iter().map(|s| s.to_string()).collect();
    let r = engine.run("ffmpeg", &args, dir.path(), Some(Target::Pc), &mut io);
    assert_eq!(r.exit_code, 3);
    assert!(r.reason.contains("didn't accept this Mac's pairing key"), "{}", r.reason);
}
