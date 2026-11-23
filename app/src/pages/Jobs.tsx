import { usePoll } from "../lib/poll";
import { Checking } from "./Status";
import { openPath } from "@tauri-apps/plugin-opener";
import { api, type Platform } from "../lib/api";
import { duration, jobLabel, timeAgo } from "../lib/format";
import { Card } from "../components/ui";

const STATE_LABEL: Record<string, string> = {
  done: "✅ Done",
  failed: "❌ Failed",
  cancelled: "⏹ Cancelled",
  interrupted: "⚠️ Stopped",
  running: "⏳ Running",
  queued: "🕒 Waiting",
};

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
      <Card title="Jobs on the PC" actions={<button className="btn small ghost" onClick={load}>Refresh</button>}>
        {!data ? (
          <Checking text="Loading jobs…" />
        ) : jobs.length === 0 ? (
          <p className="muted">No jobs yet. Ask Claude to encode a video or generate an image, and it will show up here.</p>
        ) : (
          <table className="jobs">
            <colgroup>
              <col />
              <col style={{ width: 150 }} />
              <col style={{ width: 92 }} />
              <col style={{ width: 86 }} />
              <col style={{ width: platform === "windows" ? 184 : 212 }} />
            </colgroup>
            <thead>
              <tr><th>Job</th><th>Status</th><th>Time</th><th>When</th><th /></tr>
            </thead>
            <tbody>
              {jobs.map((j) => (
                <tr key={j.id}>
                  <td className="job-cell" title={(j.argv || []).join(" ") || j.name}>{j.cached && <span className="pill">reused</span>}{jobLabel(j.name)}</td>
                  <td title={j.state === "interrupted" ? "PC Bridge restarted while this job was running" : undefined}>
                    {STATE_LABEL[j.state] || j.state}{j.exit_code ? ` (code ${j.exit_code})` : ""}
                  </td>
                  <td>{duration(j.duration)}</td>
                  <td>{timeAgo(j.created)}</td>
                  <td className="actions">
                    {platform === "windows" ? (
                      <button className="btn small" onClick={() => openPath(j.folder)}>Open folder</button>
                    ) : j.mac_outputs?.length ? (
                      <>
                        <button className="btn small" title="Open the file this job made" onClick={() => openPath(j.mac_outputs[0])}>Open</button>
                        <button className="btn small ghost" onClick={() => openPath(dirname(j.mac_outputs[0]))}>Folder</button>
                      </>
                    ) : null}
                    {(j.state === "running" || j.state === "queued") && (
                      <button className="btn small ghost" onClick={() => api.cancelJob(j.id).then(load)}>Cancel</button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Card>
      {platform !== "windows" && (
        <Card title="Where jobs ran (routing history)">
          {history.length === 0 ? (
            <p className="muted">PC Bridge learns from every job to pick the faster computer next time.</p>
          ) : (
            <table className="jobs">
              <colgroup>
                <col />
                <col style={{ width: 96 }} />
                <col style={{ width: 96 }} />
                <col style={{ width: 92 }} />
              </colgroup>
              <thead><tr><th>Command</th><th>Ran on</th><th>Time</th><th>When</th></tr></thead>
              <tbody>
                {history.slice(0, 30).map((h, i) => (
                  <tr key={i}>
                    <td className="job-cell mono small" title={h.name || h.tool}>{jobLabel(h.name || h.tool)}</td>
                    <td>{h.target === "pc" ? "🖥 PC" : "💻 Mac"}</td>
                    <td>{duration(h.seconds)}</td>
                    <td>{timeAgo(h.ts)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Card>
      )}
    </div>
  );
}
