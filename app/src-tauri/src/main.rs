// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod mac;
mod state;
mod tray;
mod util;
mod win;

use bridge_core::config::BridgeConfig;
use bridge_core::router::Target;
use bridge_core::shim::{Engine, StdIo};
use tauri::{Manager, RunEvent, WindowEvent};

/// The same binary has three jobs:
///   pc-bridge                 -> desktop app (menu bar / tray + window)
///   pc-bridge mcp             -> MCP server for Claude (stdio)
///   pc-bridge shim <tool> ... -> transparent redirect used by ~/.pcbridge/bin/<tool>
///   pc-bridge run -- <cmd...> -> "run this on the PC" from a terminal
fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("mcp") => {
            let engine = Engine::from_config(BridgeConfig::load());
            if let Err(e) = bridge_core::mcp::serve_stdio(&engine) {
                eprintln!("pc-bridge mcp: {e}");
                std::process::exit(1);
            }
        }
        Some("doctor") => {
            // Plain-text health check for support: what the setup screens see, without the window.
            let report = serde_json::json!({ "tailscale": mac::tailscale_status() });
            println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
        }
        Some("shim") if args.len() >= 3 => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
            let engine = Engine::from_config(BridgeConfig::load());
            let r = engine.run(&args[2], &args[3..], &cwd, None, &mut StdIo);
            std::process::exit(r.exit_code);
        }
        Some("run") => {
            let rest: Vec<String> = args[2..].iter().skip_while(|a| *a == "--").cloned().collect();
            let Some((tool, tail)) = rest.split_first() else {
                eprintln!("usage: pc-bridge run -- <command> [args...]");
                std::process::exit(2);
            };
            let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
            let engine = Engine::from_config(BridgeConfig::load());
            let r = engine.run(tool, tail, &cwd, Some(Target::Pc), &mut StdIo);
            eprintln!("[PC Bridge] ran on {:?} in {:.1}s — {}", r.target, r.seconds, r.reason);
            std::process::exit(r.exit_code);
        }
        Some("--version") => println!("pc-bridge {}", env!("CARGO_PKG_VERSION")),
        _ => run_gui(args.iter().any(|a| a == "--minimized")),
    }
}

fn run_gui(minimized: bool) {
    let app_state = state::AppState::default();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| tray::show_main(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .manage(app_state.clone())
        .setup(move |app| {
            tray::build(app.handle())?;
            if minimized {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            if cfg!(windows) {
                win::spawn_watchdog(app_state.clone());
            } else {
                // Restart this Mac's private Syncthing if folders were shared before.
                let st = app_state.clone();
                std::thread::spawn(move || {
                    let mut cfg = BridgeConfig::load();
                    if !cfg.shared_folders.is_empty() && !cfg.syncthing_api_key.is_empty() {
                        let _ = mac::ensure_local_syncthing(&st, &mut cfg);
                    }
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps PC Bridge running in the menu bar / tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            mac::app_info,
            mac::get_config,
            mac::save_config,
            mac::set_redirect,
            mac::tailscale_check,
            mac::pair_pc,
            mac::unpair,
            mac::share_folders,
            mac::claude_preview,
            mac::claude_install,
            mac::claude_uninstall,
            mac::network_check,
            mac::final_test,
            mac::status_snapshot,
            mac::jobs_list,
            mac::cancel_job,
            mac::wake_pc,
            mac::run_diagnostics_mac,
            win::win_get_state,
            win::win_save_state,
            win::win_check_gpu,
            win::win_check_tool,
            win::win_install,
            win::win_open_tailscale,
            win::win_tailscale_check,
            win::win_claude_login,
            win::win_claude_check,
            win::win_setup_agent,
            win::win_pairing_code,
            win::win_configure_wol,
            win::win_power,
            win::win_set_autostart,
            win::win_status,
            win::run_diagnostics_win,
        ])
        .build(tauri::generate_context!())
        .expect("error while building PC Bridge");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(s) = handle.try_state::<state::AppState>() {
                s.procs.lock().unwrap().kill_all();
            }
        }
    });
}
