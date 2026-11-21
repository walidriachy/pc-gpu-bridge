import { invoke } from "@tauri-apps/api/core";

export type Platform = "macos" | "windows" | "linux";

export interface SharedFolder {
  mac_path: string;
  name: string;
}

export interface BridgeConfig {
  pc_host: string;
  port: number;
  pc_root: string;
  pc_hostname: string;
  pc_mac_address: string;
  shared_folders: SharedFolder[];
  redirect_enabled: boolean;
  measured_mbps: number;
  sync_wait_secs: number;
  notify_on_fallback: boolean;
  notify_on_finish: boolean;
  seen_version: string;
  setup_complete: boolean;
  wizard_step: number;
  syncthing_api_key: string;
}

export interface WinState {
  root: string;
  wizard_step: number;
  setup_complete: boolean;
  syncthing_api_key: string;
  mac_address: string;
  want_ollama: boolean;
  want_comfy: boolean;
  want_pytorch: boolean;
  use_wsl: boolean;
}

/** A program the PC Bridge helper can install on the PC. */
export interface PcProgram {
  key: string;
  name: string;
  why: string;
  size_gb: number;
  installed: boolean;
}

export interface ProgramsInfo {
  programs: PcProgram[];
  /** Free space on the drive programs install to, in GB (-1 when unknown). */
  free_gb: number;
  drive: string;
}

export interface InstallResult {
  ok: boolean;
  /** Installed, but Windows must restart to finish the install. */
  restart_needed?: boolean;
  exit_code: number;
  log_tail: string;
}

export interface Check {
  name: string;
  ok: boolean;
  detail: string;
  fix: string;
}

/** Error text from a failed Tauri command, always as a readable string. */
export function errText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}

export const api = {
  appInfo: () => invoke<{ platform: Platform; version: string; binary: string }>("app_info"),
  // Mac
  getConfig: () => invoke<BridgeConfig>("get_config"),
  saveConfig: (cfg: BridgeConfig) => invoke<void>("save_config", { cfg }),
  setRedirect: (enabled: boolean) => invoke<void>("set_redirect", { enabled }),
  tailscaleCheck: () => invoke<any>("tailscale_check"),
  pairPc: (host: string, code: string) => invoke<{ hostname: string; root: string }>("pair_pc", { host, code }),
  unpair: () => invoke<void>("unpair"),
  shareFolders: (folders: string[]) => invoke<any>("share_folders", { folders }),
  claudePreview: () => invoke<any>("claude_preview"),
  claudeInstall: (includeClaudeMd: boolean, includeShims: boolean) =>
    invoke<string[]>("claude_install", { includeClaudeMd, includeShims }),
  claudeUninstall: () => invoke<void>("claude_uninstall"),
  networkCheck: () => invoke<any>("network_check"),
  finalTest: () => invoke<any>("final_test"),
  statusSnapshot: () => invoke<any>("status_snapshot"),
  jobsList: () => invoke<any>("jobs_list"),
  cancelJob: (id: string) => invoke<void>("cancel_job", { id }),
  wakePc: () => invoke<string>("wake_pc"),
  diagnosticsMac: () => invoke<Check[]>("run_diagnostics_mac"),
  whatsNew: () =>
    invoke<{ version: string; previous: string; refreshed: string[]; notes_url: string } | null>("whats_new"),
  dismissWhatsNew: () => invoke<void>("dismiss_whats_new"),
  staleClaudeSessions: () => invoke<{ code: number; desktop: boolean }>("stale_claude_sessions"),
  // Windows
  winGetState: () => invoke<WinState>("win_get_state"),
  winSaveState: (state: WinState) => invoke<void>("win_save_state", { state }),
  winCheckGpu: () => invoke<any>("win_check_gpu"),
  winCheckTool: (id: string) => invoke<{ id: string; installed: boolean; detail: string }>("win_check_tool", { id }),
  winInstall: (id: string) => invoke<{ id: string; installed: boolean; detail: string }>("win_install", { id }),
  winOpenTailscale: () => invoke<void>("win_open_tailscale"),
  winTailscaleCheck: () => invoke<any>("win_tailscale_check"),
  winClaudeLogin: () => invoke<void>("win_claude_login"),
  winClaudeCheck: () => invoke<any>("win_claude_check"),
  winSetupAgent: () => invoke<any>("win_setup_agent"),
  winPairedMacs: () =>
    invoke<{ devices: { index: number; name: string; paired_at: number; device_id: string | null }[] }>("win_paired_macs"),
  winRemoveMac: (index: number) => invoke<unknown>("win_remove_mac", { index }),
  winPairingCode: (fresh: boolean) => invoke<{ code: string; pc_name: string; pc_ip: string }>("win_pairing_code", { fresh }),
  winPrograms: () => invoke<ProgramsInfo>("win_programs"),
  winInstallProgram: (key: string) => invoke<InstallResult>("win_install_program", { key }),
  winConfigureWol: () => invoke<{ adapter: string; mac: string }>("win_configure_wol"),
  winPower: () => invoke<any>("win_power"),
  winSetAutostart: (enabled: boolean) => invoke<void>("win_set_autostart", { enabled }),
  checkForUpdate: (install: boolean) =>
    invoke<{ available: boolean; current: string; version?: string; waiting?: string }>("check_for_update", { install }),
  winRestartHelper: () => invoke<{ ok: boolean; version: string }>("win_restart_helper"),
  winStatus: () => invoke<any>("win_status"),
  diagnosticsWin: () => invoke<Check[]>("run_diagnostics_win"),
};
