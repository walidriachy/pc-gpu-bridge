import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, errText, type Platform } from "../lib/api";
import { duration } from "../lib/format";
import { Card, Dot, MemoryCompare } from "../components/ui";

function usePoll<T>(fn: () => Promise<T>, ms: number): [T | null, string] {
  const [data, setData] = useState<T | null>(null);
  const [err, setErr] = useState("");
  useEffect(() => {
    let alive = true;
    const tick = () =>
      fn()
        .then((d) => alive && (setData(d), setErr("")))
        .catch((e) => alive && setErr(errText(e)));
    tick();
    const t = setInterval(tick, ms);
    return () => {
      alive = false;
      clearInterval(t);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return [data, err];
}

function GpuCard({ gpu }: { gpu: any }) {
  const g = gpu?.gpus?.[0];
  if (!g) return <Card title="Graphics card"><p className="muted">Not available right now.</p></Card>;
  const vram = g.vram_total_mb ? g.vram_used_mb / g.vram_total_mb : 0;
  return (
    <Card title="Graphics card">
      <p><strong>{g.name}</strong> · {g.temperature_c}°C</p>
      <div className="meter"><span>Busy</span><div className="membar-track"><div className="membar-fill good" style={{ width: `${g.utilization}%` }} /></div><span>{g.utilization}%</span></div>
      <div className="meter"><span>VRAM</span><div className="membar-track"><div className="membar-fill good" style={{ width: `${vram * 100}%` }} /></div><span>{(g.vram_used_mb / 1024).toFixed(1)} / {(g.vram_total_mb / 1024).toFixed(0)} GB</span></div>
    </Card>
  );
}

function RunningCard({ pc }: { pc: any }) {
  const running: any[] = pc?.running || [];
  return (
    <Card title="Right now">
      {running.length === 0 ? (
        <p className="muted">No job running{pc?.waiting ? ` · ${pc.waiting} waiting` : ""}.</p>
      ) : (
        running.map((j) => (
          <div key={j.id} className="job-line">
            <span className="spinner" /> <strong>{j.name}</strong> · {duration(j.duration)}
            <button className="btn small ghost" onClick={() => api.cancelJob(j.id)}>Cancel</button>
          </div>
        ))
      )}
    </Card>
  );
}

function WhatsNewCard() {
  const [n, setN] = useState<Awaited<ReturnType<typeof api.whatsNew>>>(null);
  useEffect(() => {
    api.whatsNew().then(setN).catch(() => setN(null));
  }, []);
  if (!n) return null;
  const close = () => api.dismissWhatsNew().finally(() => setN(null));
  return (
    <Card title={`Updated to ${n.version}`} actions={<button className="btn small ghost" onClick={close}>Got it</button>}>
      <p>
        PC Bridge updated itself{n.previous ? ` from ${n.previous}` : ""}.{" "}
        {n.refreshed.length > 0 ? `Claude's setup was refreshed too (${n.refreshed.join(", ")}).` : ""}
      </p>
      <p><button className="btn small" onClick={() => openUrl(n.notes_url)}>See what's new</button></p>
    </Card>
  );
}

function OpenSessionsCard() {
  const [s] = usePoll(() => api.staleClaudeSessions(), 30000);
  if (!s || (s.code === 0 && !s.desktop)) return null;
  const parts = [
    s.code > 0 ? `${s.code} Claude Code session${s.code === 1 ? " was" : "s were"}` : "",
    s.desktop ? `Claude Desktop was` : "",
  ].filter(Boolean);
  return (
    <Card title="Start a new Claude session">
      <p>
        {parts.join(" and ")} already open before PC Bridge's Claude setup changed. They keep the old setup and
        won't see PC Bridge's newest tools until you start a new session{s.desktop ? " (Claude Desktop: quit it and open it again)" : ""}.
      </p>
    </Card>
  );
}

function VersionNote({ pcVersion, macVersion }: { pcVersion?: string; macVersion: string }) {
  if (!pcVersion || !macVersion || pcVersion === macVersion) return null;
  return (
    <p className="muted small">
      The PC has PC Bridge {pcVersion}, this Mac {macVersion}. Each one updates itself: it checks every hour and waits
      until no job is running. To update the PC now, open PC Bridge there → Settings → Check for updates now.
    </p>
  );
}

function MacStatus() {
  const [s] = usePoll(() => api.statusSnapshot(), 5000);
  const [macVersion, setMacVersion] = useState("");
  useEffect(() => {
    api.appInfo().then((i) => setMacVersion(i.version)).catch(() => {});
  }, []);
  const [msg, setMsg] = useState("");
  if (!s) return <p className="muted">Checking your PC…</p>;
  const refused = s.error_kind === "unauthorized";
  const color = s.reachable ? "green" : s.paired && !refused ? "yellow" : "red";
  const label = s.reachable
    ? `${s.pc_hostname || "PC"} is ready`
    : refused
      ? "The PC didn't accept this Mac's pairing key. Go to Settings → Run setup again to pair."
      : s.paired
        ? "PC is asleep or offline — it will be woken for the next job"
        : "Not connected to a PC yet";
  return (
    <div className="grid">
      <Card
        title="Your PC"
        actions={
          <label className="switch">
            <input type="checkbox" checked={s.redirect_enabled} onChange={(e) => api.setRedirect(e.target.checked)} />
            <span>Send heavy jobs to the PC</span>
          </label>
        }
      >
        <p className="status-line"><Dot color={color} /> {label}</p>
        {!s.reachable && s.paired && !refused && (
          <button className="btn small" onClick={() => api.wakePc().then(setMsg).catch((e) => setMsg(errText(e)))}>Wake PC</button>
        )}
        {msg && <p className="muted small">{msg}</p>}
        {s.reachable && <VersionNote pcVersion={s.pc?.version} macVersion={macVersion} />}
      </Card>
      <WhatsNewCard />
      <OpenSessionsCard />
      <Card title="Memory: Mac vs PC">
        <MemoryCompare mac={s.mac_memory} pc={s.pc?.memory} pressure={s.mac_memory?.pressure} />
      </Card>
      <GpuCard gpu={s.gpu} />
      <RunningCard pc={s.pc} />
    </div>
  );
}

function WinStatus() {
  const [s] = usePoll(() => api.winStatus(), 5000);
  if (!s) return <p className="muted">Checking…</p>;
  const ok = s.agent_running && s.tailscale?.running;
  return (
    <div className="grid">
      <Card title="This PC">
        <p className="status-line"><Dot color={ok ? "green" : s.agent_running ? "yellow" : "red"} /> {ok ? "Ready for jobs from your Mac" : s.agent_running ? "Tailscale is off — your Mac can't reach this PC" : "PC Bridge helper isn't running"}</p>
        <p className="muted small">Files: {s.root}</p>
      </Card>
      <Card title="Memory">
        <MemoryCompare mac={s.status?.peer_memory} pc={s.status?.memory} />
        {!s.status?.peer_memory && <p className="muted small">Your Mac's free memory appears here once your Mac has connected. Jobs it sends here use this PC's memory.</p>}
      </Card>
      <GpuCard gpu={s.gpu} />
      <RunningCard pc={s.status} />
    </div>
  );
}

export function Status({ platform }: { platform: Platform }) {
  return platform === "windows" ? <WinStatus /> : <MacStatus />;
}
