import { useEffect, useState } from "react";
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

function MacStatus() {
  const [s] = usePoll(() => api.statusSnapshot(), 5000);
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
      </Card>
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
