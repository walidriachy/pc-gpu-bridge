import { usePoll } from "../lib/poll";
import { Checking } from "./Status";
import { openPath } from "@tauri-apps/plugin-opener";
import { api, type Platform } from "../lib/api";
import { duration, jobLabel, timeAgo } from "../lib/format";
import { Card, Icon, Pill, type IconName, type Tone } from "../components/ui";

const STATE: Record<string, { label: string; tone: Tone; icon?: IconName }> = {
  done: { label: "Done", tone: "teal", icon: "check" },
  failed: { label: "Failed", tone: "rose", icon: "x" },
  cancelled: { label: "Cancelled", tone: "neutral", icon: "stop" },
  interrupted: { label: "Stopped", tone: "amber", icon: "alert" },
  running: { label: "Running", tone: "sapphire" },
  queued: { label: "Waiting", tone: "amber", icon: "clock" },
};

function StatePill({ state, code }: { state: string; code?: number }) {
  const st = STATE[state] || { label: state, tone: "neutral" as Tone };
  return (
    <Pill tone={st.tone} icon={st.icon} spin={state === "running"} title={state === "interrupted" ? "PC Bridge restarted while this job was running" : undefined}>
      {st.label}{code ? ` · ${code}` : ""}
    </Pill>
  );
}

function dirname(p: string): string {
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return i > 0 ? p.slice(0, i) : p;
}

export function Jobs({ platform }: { platform: Platform }) {
  const { data, err, reload: load } = usePoll<{ jobs: any[]; history: any[] }>(
    `jobs-${platform}`,
    () =>
      platform === "windows"
        ? api.winJobs().then((jobs) => ({ jobs: jobs || [], history: [] }))
        : api.jobsList().then((r) => ({ jobs: r.pc_jobs || [], history: r.history || [] })),
    5000,
  );
  const jobs = data?.jobs || [];
  const history = data?.history || [];

  return (
    <div className="stack">
      {err && <p className="error">{err}</p>}
      <Card title="Jobs on the PC" kicker="QUEUE" icon="jobs" actions={<button className="btn small ghost" onClick={load}><Icon name="refresh" size={13} /> Refresh</button>}>
        {!data ? (
          <Checking text="Loading jobs…" />
        ) : jobs.length === 0 ? (
          <p className="empty"><Icon name="box" /> No jobs yet. Ask Claude to encode a video or generate an image, and it will show up here.</p>
        ) : (
          <div className="table-wrap">
          <table className="jobs">
            <colgroup>
              <col />
              <col className="c-status" />
              <col className="c-time" />
              <col className="c-when" />
              <col className={platform === "windows" ? "c-act-win" : "c-act-mac"} />
            </colgroup>
            <thead>
              <tr><th>Job</th><th>Status</th><th>Time</th><th>When</th><th /></tr>
            </thead>
            <tbody>
              {jobs.map((j) => (
                <tr key={j.id}>
                  <td className="job-cell" title={(j.argv || []).join(" ") || j.name}>{j.cached && <Pill tone="amber">reused</Pill>}{jobLabel(j.name)}</td>
                  <td><StatePill state={j.state} code={j.exit_code} /></td>
                  <td className="mono-col">{duration(j.duration)}</td>
                  <td className="mono-col">{timeAgo(j.created)}</td>
                  <td className="actions">
                    {platform === "windows" ? (
                      <button className="btn small" onClick={() => openPath(j.folder)}><Icon name="folder" size={13} /> Open folder</button>
                    ) : j.mac_outputs?.length ? (
                      <>
                        <button className="btn small" title="Open the file this job made" onClick={() => openPath(j.mac_outputs[0])}><Icon name="open" size={13} /> Open</button>
                        <button className="btn small ghost" onClick={() => openPath(dirname(j.mac_outputs[0]))}>Folder</button>
                      </>
                    ) : null}
                    {(j.state === "running" || j.state === "queued") && (
                      <button className="btn small ghost danger" onClick={() => api.cancelJob(j.id).then(load)}>Cancel</button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          </div>
        )}
      </Card>
      {platform !== "windows" && (
        <Card title="Where jobs ran (routing history)" kicker="ROUTER" icon="link">
          {history.length === 0 ? (
            <p className="empty"><Icon name="link" /> PC Bridge learns from every job to pick the faster computer next time.</p>
          ) : (
            <div className="table-wrap">
            <table className="jobs">
              <colgroup>
                <col />
                <col className="c-where" />
                <col className="c-time" />
                <col className="c-when" />
              </colgroup>
              <thead><tr><th>Command</th><th>Ran on</th><th>Time</th><th>When</th></tr></thead>
              <tbody>
                {history.slice(0, 30).map((h, i) => (
                  <tr key={i}>
                    <td className="job-cell mono small" title={h.name || h.tool}>{jobLabel(h.name || h.tool)}</td>
                    <td>{h.target === "pc" ? <span className="where pc"><Icon name="monitor" size={14} /> PC</span> : <span className="where mac"><Icon name="laptop" size={14} /> Mac</span>}</td>
                    <td className="mono-col">{duration(h.seconds)}</td>
                    <td className="mono-col">{timeAgo(h.ts)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            </div>
          )}
        </Card>
      )}
    </div>
  );
}
