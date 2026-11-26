import { useEffect, useState } from "react";
import { openPath } from "@tauri-apps/plugin-opener";
import { api, errText, type BridgeConfig, type Platform, type WinState } from "../lib/api";
import { Card, CopyBox, Icon, Pill, Toggle } from "../components/ui";
import { ProgramsCard } from "../components/Programs";

function UpdatesCard() {
  const [msg, setMsg] = useState("");
  const [busy, setBusy] = useState(false);
  const check = async () => {
    setBusy(true);
    setMsg("Checking…");
    try {
      const r = await api.checkForUpdate(false);
      if (!r.available) {
        setMsg(`You have the latest version (${r.current}).`);
      } else {
        setMsg(`Installing version ${r.version}… PC Bridge will restart by itself.`);
        const done = await api.checkForUpdate(true);
        if (done.waiting) setMsg(`Version ${done.version} is ready. It installs as soon as the current PC job finishes.`);
      }
    } catch (e) {
      setMsg(errText(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Card title="Updates" kicker="SIGNED RELEASES" icon="shield">
      <p>PC Bridge updates itself: it checks a few minutes after it starts and then every 6 hours, installs the new version and restarts. Every update is signed, so only real PC Bridge releases are installed.</p>
      <button className="btn" disabled={busy} onClick={check}><Icon name="refresh" size={14} /> Check for updates now</button>
      {msg && <p className="muted small">{msg}</p>}
    </Card>
  );
}

function MacSettings({ onRerunSetup }: { onRerunSetup: () => void }) {
  const [cfg, setCfg] = useState<BridgeConfig | null>(null);
  const [msg, setMsg] = useState("");
  useEffect(() => {
    api.getConfig().then(setCfg);
  }, []);
  if (!cfg) return null;
  const save = async (c: BridgeConfig) => {
    setCfg(c);
    await api.saveConfig(c);
    setMsg("Saved");
    setTimeout(() => setMsg(""), 1200);
  };
  return (
    <div className="stack">
      <Card title="Redirecting" kicker="ROUTING" icon="link" primary>
        <div className="stack-switches">
          <Toggle checked={cfg.redirect_enabled} onChange={(v) => { api.setRedirect(v); setCfg({ ...cfg, redirect_enabled: v }); }} label="Send heavy jobs to the PC" />
          <Toggle checked={cfg.notify_on_fallback} onChange={(v) => save({ ...cfg, notify_on_fallback: v })} label="Notify me when a job had to run on the Mac instead" />
          <Toggle checked={cfg.notify_on_finish} onChange={(v) => save({ ...cfg, notify_on_finish: v })} label="Notify me when a PC job that took a minute or more finishes" />
        </div>
      </Card>
      <Card title="Paired PC" kicker="DEVICE" icon="monitor" actions={cfg.pc_host ? <Pill tone="teal" icon="check">Paired</Pill> : <Pill tone="rose">Not paired</Pill>}>
        <dl className="kv">
          <dt>Name</dt><dd>{cfg.pc_hostname || cfg.pc_host || "Not paired"}</dd>
          {cfg.pc_host && (<><dt>Address</dt><dd className="mono">{cfg.pc_host}</dd></>)}
          <dt>PC folder</dt><dd className="mono">{cfg.pc_root}</dd>
        </dl>
        <div className="row">
          <button className="btn" onClick={onRerunSetup}><Icon name="refresh" size={14} /> Run setup again</button>
          <button className="btn ghost" onClick={async () => { await api.unpair(); setMsg("Unpaired. Run setup again to pair."); }}>Unpair</button>
        </div>
      </Card>
      <Card title="Shared folders" kicker="SYNC" icon="folder">
        <ul className="folder-list">
          {cfg.shared_folders.map((f) => (
            <li key={f.mac_path}>
              <Icon name="folder" size={18} />
              <div className="folder-info">
                <div>{f.mac_path}</div>
                <div className="muted small">PC · workspace\{f.name}</div>
              </div>
              <button className="btn small ghost" onClick={() => openPath(f.mac_path)}>Open</button>
            </li>
          ))}
        </ul>
        <p className="muted small">To change folders, use “Run setup again” → Choose folders.</p>
      </Card>
      <Card title="Docker & npm/pnpm (per project)" kicker=".PCBRIDGE" icon="terminal">
        <p>Big JavaScript builds and Docker only go to the PC in projects you opt in. Put a file named <code>.pcbridge</code> in the project folder (the folder must be shared) containing:</p>
        <CopyBox command={'offload = ["npm", "pnpm", "docker"]'} explain="Remove any you don't want. Light commands like “npm install” still run on the Mac; builds and test suites go to the PC." />
        <p className="muted small">You can also send any single command from Terminal:</p>
        <CopyBox command={"pc-bridge run -- npm run build"} explain="“Run this on the PC”: works for any command inside a shared folder. (The helper commands must be installed: Run setup again → Connect to Claude.)" />
      </Card>
      <Card title="Claude connection" kicker="MCP" icon="spark">
        <div className="row">
          <button className="btn" onClick={() => api.claudeInstall(true, true).then((r) => setMsg(r.join(" · "))).catch((e) => setMsg(errText(e)))}>Reconnect to Claude</button>
          <button className="btn ghost" onClick={() => api.claudeUninstall().then(() => setMsg("Removed PC Bridge from Claude, CLAUDE.md and your PATH."))}>Disconnect from Claude</button>
        </div>
      </Card>
      <UpdatesCard />
      {msg && <p className="toast">{msg}</p>}
    </div>
  );
}

function WinSettings({ onRerunSetup }: { onRerunSetup: () => void }) {
  const [ws, setWs] = useState<WinState | null>(null);
  const [auto, setAuto] = useState(true);
  const [helper, setHelper] = useState("");
  useEffect(() => {
    api.winGetState().then(setWs);
  }, []);
  if (!ws) return null;
  return (
    <div className="stack">
      <Card title="Startup" kicker="WINDOWS" icon="power" primary>
        <Toggle checked={auto} onChange={(v) => { setAuto(v); api.winSetAutostart(v); }} label="Start PC Bridge with Windows" />
      </Card>
      <Card title="Files" kicker="STORAGE" icon="folder">
        <p>Everything PC Bridge writes is inside <strong className="mono">{ws.root}</strong>.</p>
        <div className="row">
          <button className="btn" onClick={() => openPath(ws.root)}><Icon name="folder" size={14} /> Open folder</button>
          <button className="btn ghost" onClick={() => openPath(`${ws.root}\\logs`)}>Open logs</button>
        </div>
        <p className="muted small">Job folders older than 14 days are deleted automatically (change “cleanup_days” in {ws.root}\config\settings.json).</p>
      </Card>
      <UpdatesCard />
      <ProgramsCard />
      <Card title="Helper" kicker="AGENT" icon="pulse">
        <p>The helper is the background program your Mac talks to. Restart it if something you just installed (like FFmpeg) still shows as missing.</p>
        <button
          className="btn"
          disabled={helper === "Restarting…"}
          onClick={async () => {
            setHelper("Restarting…");
            try {
              const r = await api.winRestartHelper();
              setHelper(`Helper running (version ${r.version})`);
            } catch (e) {
              setHelper(errText(e));
            }
          }}
        >
          Restart the helper
        </button>
        {helper && <p className="muted small">{helper}</p>}
      </Card>
      <Card title="Pairing" kicker="TRUST" icon="lock">
        <PairedMacs />
        <button className="btn" onClick={onRerunSetup}>Show a pairing code / run setup again</button>
      </Card>
    </div>
  );
}

function PairedMacs() {
  const [list, setList] = useState<{ index: number; name: string; paired_at: number }[] | null>(null);
  const [msg, setMsg] = useState("");
  const load = () =>
    api
      .winPairedMacs()
      .then((r) => setList(r.devices))
      .catch((e) => setMsg(errText(e)));
  useEffect(() => {
    load();
  }, []);
  if (!list) return msg ? <p className="muted small">{msg}</p> : null;
  return (
    <div className="paired-macs">
      {list.length === 0 ? (
        <p className="muted">No Mac is paired with this PC.</p>
      ) : (
        <ul className="plain-list">
          {list.map((d) => (
            <li key={d.index}>
              <Icon name="laptop" size={16} />
              <strong>{d.name}</strong>{" "}
              <span className="muted small">paired {new Date(d.paired_at * 1000).toLocaleDateString()}</span>{" "}
              <button
                className="btn small ghost danger"
                onClick={() =>
                  api
                    .winRemoveMac(d.index)
                    .then(() => {
                      setMsg(`${d.name} removed. Its key no longer works on this PC.`);
                      load();
                    })
                    .catch((e) => setMsg(errText(e)))
                }
              >
                Remove
              </button>
            </li>
          ))}
        </ul>
      )}
      {msg && <p className="muted small">{msg}</p>}
    </div>
  );
}

export function Settings({ platform, onRerunSetup }: { platform: Platform; onRerunSetup: () => void }) {
  return platform === "windows" ? <WinSettings onRerunSetup={onRerunSetup} /> : <MacSettings onRerunSetup={onRerunSetup} />;
}
