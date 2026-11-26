import { useEffect, useState, type ReactNode } from "react";
import { usePoll } from "../lib/poll";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, errText, type Platform } from "../lib/api";
import { duration, gb, jobLabel } from "../lib/format";
import { Card, Dot, Icon, MemoryCompare, Pill, Toggle, Track } from "../components/ui";

export function Checking({ text }: { text: string }) {
  return <p className="muted checking"><span className="spinner" /> {text}</p>;
}

function GpuCard({ gpu }: { gpu: any }) {
  const g = gpu?.gpus?.[0];
  if (!g)
    return (
      <Card title="Graphics card" kicker="GPU" icon="chip">
        <p className="muted">Not available right now.</p>
      </Card>
    );
  const vram = g.vram_total_mb ? g.vram_used_mb / g.vram_total_mb : 0;
  const temp = Number(g.temperature_c);
  return (
    <Card title="Graphics card" kicker="GPU" icon="chip" primary>
      <div className="gpu-title">
        <strong className="one-line" title={g.name}>{g.name}</strong>
        {g.temperature_c != null && <Pill tone={temp >= 83 ? "rose" : temp >= 72 ? "amber" : "teal"}>{g.temperature_c}°C</Pill>}
      </div>
      <div className="meter"><span>Utilization</span><Track value={g.utilization / 100} /><span>{g.utilization}%</span></div>
      <div className="meter"><span>VRAM</span><Track value={vram} /><span>{(g.vram_used_mb / 1024).toFixed(1)} / {(g.vram_total_mb / 1024).toFixed(0)} GB</span></div>
    </Card>
  );
}

function SyncCard({ sync }: { sync: any[] | undefined }) {
  if (!sync || sync.length === 0) return null;
  return (
    <Card title="Folder copy to the PC" kicker="SYNC" icon="folder">
      {sync.map((f) =>
        f.percent == null ? (
          <p key={f.name} className="muted">{f.name}: not running right now (it restarts within a minute).</p>
        ) : f.need_files === 0 ? (
          <div key={f.name} className="sync-row membar-head">
            <strong className="one-line" title={f.name}>{f.name}</strong>
            <Pill tone="teal" icon="check">Synced</Pill>
          </div>
        ) : (
          <div key={f.name} className="sync-row">
            <div className="membar-head">
              <strong className="one-line" title={f.name}>{f.name}</strong>
              <span className="num">{Math.floor(f.percent)}%</span>
            </div>
            <Track value={f.percent / 100} tone="teal" />
            <p className="muted small">
              {f.need_files.toLocaleString()} files ({gb(f.need_bytes)}) still to copy. Jobs send the files they need
              themselves, so they don't have to wait for this.
            </p>
          </div>
        ),
      )}
    </Card>
  );
}

function RunningCard({ pc }: { pc: any }) {
  const running: any[] = pc?.running || [];
  return (
    <Card title="Right now" kicker="ACTIVE JOBS" icon="pulse" actions={running.length > 0 ? <Pill tone="sapphire" spin>{running.length} running</Pill> : undefined}>
      {running.length === 0 ? (
        <p className="empty"><Icon name="clock" /> No job running{pc?.waiting ? ` · ${pc.waiting} waiting` : ""}.</p>
      ) : (
        running.map((j) => (
          <div key={j.id} className="job-line">
            <span className="spinner" />
            <strong className="job-name" title={j.name}>{jobLabel(j.name)}</strong>
            <span className="job-time">· {duration(j.duration)}</span>
            <button className="btn small ghost danger" onClick={() => api.cancelJob(j.id)}>Cancel</button>
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
    <Card title={`Updated to ${n.version}`} kicker="WHAT'S NEW" icon="spark" className="callout-card" actions={<button className="btn small ghost" onClick={close}>Got it</button>}>
      <p>
        PC Bridge updated itself{n.previous ? ` from ${n.previous}` : ""}.{" "}
        {n.refreshed.length > 0 ? `Claude's setup was refreshed too (${n.refreshed.join(", ")}).` : ""}
      </p>
      <p><button className="btn small" onClick={() => openUrl(n.notes_url)}><Icon name="open" size={13} /> See what's new</button></p>
    </Card>
  );
}

function OpenSessionsCard() {
  const { data: s } = usePoll("stale-sessions", () => api.staleClaudeSessions(), 30000);
  if (!s || (s.code === 0 && !s.desktop)) return null;
  const parts = [
    s.code > 0 ? `${s.code} Claude Code session${s.code === 1 ? " was" : "s were"}` : "",
    s.desktop ? `Claude Desktop was` : "",
  ].filter(Boolean);
  return (
    <Card title="Start a new Claude session" kicker="ACTION NEEDED" icon="alert" className="callout-card amber">
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
    <p className="callout small">
      The PC has PC Bridge {pcVersion}, this Mac {macVersion}. Each one updates itself: it checks every hour and waits
      until no job is running. To update the PC now, open PC Bridge there → Settings → Check for updates now.
    </p>
  );
}

type Beam = "live" | "idle" | "off" | "paused";

/** Mac and PC as two nodes joined by a glowing link, with the link's state in the middle. */
function BridgeHero({
  left,
  right,
  beam,
  beamLabel,
  children,
}: {
  left: { kicker: string; name: string };
  right: { kicker: string; name: string };
  beam: Beam;
  beamLabel: string;
  children?: ReactNode;
}) {
  return (
    <Card className="hero span-all" primary>
      <div className="hero-link">
        <div className="node">
          <span className="node-icon"><Icon name="laptop" size={24} /></span>
          <div className="node-text">
            <span className="kicker">{left.kicker}</span>
            <strong title={left.name}>{left.name}</strong>
          </div>
        </div>
        <div className="beam-wrap">
          <div className={beam === "live" ? "beam" : `beam ${beam}`} />
          <span className="beam-label">
            <Icon name={beam === "live" ? "lock" : beam === "idle" ? "power" : beam === "paused" ? "stop" : "x"} size={12} />
            {beamLabel}
          </span>
        </div>
        <div className="node right">
          <div className="node-text">
            <span className="kicker">{right.kicker}</span>
            <strong title={right.name}>{right.name}</strong>
          </div>
          <span className="node-icon"><Icon name="monitor" size={24} /></span>
        </div>
      </div>
      {children && <div className="hero-foot">{children}</div>}
    </Card>
  );
}

function MacStatus() {
  const { data: s } = usePoll("mac-status", () => api.statusSnapshot(), 5000, (d) => d.reachable);
  const [macVersion, setMacVersion] = useState("");
  useEffect(() => {
    api.appInfo().then((i) => setMacVersion(i.version)).catch(() => {});
  }, []);
  const [msg, setMsg] = useState("");
  if (!s) return <Checking text="Checking your PC…" />;
  const refused = s.error_kind === "unauthorized";
  const color = s.reachable ? "green" : s.paired && !refused ? "yellow" : "red";
  const label = s.reachable
    ? `${s.pc_hostname || "PC"} is ready`
    : refused
      ? "The PC didn't accept this Mac's pairing key. Go to Settings → Run setup again to pair."
      : s.paired
        ? "PC is asleep or offline. It will be woken for the next job."
        : "Not connected to a PC yet";
  const beam: Beam = s.reachable ? (s.redirect_enabled ? "live" : "paused") : s.paired && !refused ? "idle" : "off";
  const beamLabel = { live: "Tailscale private", paused: "Paused", idle: "Sleeping", off: refused ? "Pairing refused" : "Not connected" }[beam];
  return (
    <div className="grid">
      <BridgeHero
        left={{ kicker: "MAC · THE BRAIN", name: "This Mac" }}
        right={{ kicker: "WINDOWS PC · THE MUSCLE", name: s.pc_hostname || (s.paired ? "Your PC" : "No PC paired") }}
        beam={beam}
        beamLabel={beamLabel}
      >
        <p className="status-line"><Dot color={color} /> {label}</p>
        <div className="row">
          {!s.reachable && s.paired && !refused && (
            <button className="btn small" onClick={() => api.wakePc().then(setMsg).catch((e) => setMsg(errText(e)))}>
              <Icon name="power" size={13} /> Wake PC
            </button>
          )}
          <Toggle checked={s.redirect_enabled} onChange={(v) => api.setRedirect(v)} label="Send heavy jobs to the PC" />
        </div>
        {msg && <p className="muted small span-all">{msg}</p>}
      </BridgeHero>
      {s.reachable && s.pc?.version && s.pc.version !== macVersion && macVersion && (
        <div className="span-all"><VersionNote pcVersion={s.pc?.version} macVersion={macVersion} /></div>
      )}
      <WhatsNewCard />
      <OpenSessionsCard />
      <Card title="Memory: Mac vs PC" kicker="RAM" icon="memory">
        <MemoryCompare mac={s.mac_memory} pc={s.pc?.memory} pressure={s.mac_memory?.pressure} />
      </Card>
      <GpuCard gpu={s.gpu} />
      <SyncCard sync={s.sync} />
      <RunningCard pc={s.pc} />
    </div>
  );
}

function WinStatus() {
  const { data: s } = usePoll("win-status", () => api.winStatus(), 5000, (d) => d.agent_running && d.tailscale?.running);
  if (!s) return <Checking text="Checking this PC…" />;
  const ok = s.agent_running && s.tailscale?.running;
  const beam: Beam = ok ? "live" : s.agent_running ? "idle" : "off";
  return (
    <div className="grid">
      <BridgeHero
        left={{ kicker: "CONNECTED MAC", name: "Your Mac" }}
        right={{ kicker: "THIS PC · THE MUSCLE", name: s.tailscale?.self_name || "This PC" }}
        beam={beam}
        beamLabel={ok ? "Tailscale private" : s.agent_running ? "Tailscale off" : "Helper stopped"}
      >
        <p className="status-line"><Dot color={ok ? "green" : s.agent_running ? "yellow" : "red"} /> {ok ? "Ready for jobs from your Mac" : s.agent_running ? "Tailscale is off, so your Mac can't reach this PC" : "PC Bridge helper isn't running"}</p>
        <p className="muted small break" style={{ margin: 0 }}><Icon name="folder" size={13} /> Files: {s.root}</p>
      </BridgeHero>
      <Card title="Memory" kicker="RAM" icon="memory">
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
