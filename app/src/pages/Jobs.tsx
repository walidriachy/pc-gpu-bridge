import { useEffect, useState } from "react";
import { openPath } from "@tauri-apps/plugin-opener";
import { api, errText, type Platform } from "../lib/api";
import { duration, jobLabel, timeAgo } from "../lib/format";
import { Card } from "../components/ui";

const STATE_LABEL: Record<string, string> = {
  done: "✅ Done",
  failed: "❌ Failed",
  cancelled: "⏹ Cancelled",
  interrupted: "⚠️ Stopped (PC Bridge restarted)",
  running: "⏳ Running",
  queued: "🕒 Waiting",
};

function dirname(p: string): string {
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return i > 0 ? p.slice(0, i) : p;
}

export function Jobs({ platform }: { platform: Platform }) {
  const [jobs, setJobs] = useState<any[]>([]);
  const [history, setHistory] = useState<any[]>([]);
  const [err, setErr] = useState("");
  const load = () => {
    if (platform === "windows") {
      api.winStatus().then((s) => setJobs(s.jobs || [])).catch((e) => setErr(errText(e)));
    } else {
      api
        .jobsList()
        .then((r) => {
          setJobs(r.pc_jobs || []);
          setHistory(r.history || []);
        })
        .catch((e) => setErr(errText(e)));
    }
  };
  useEffect(() => {
    load();
    const t = setInterval(load, 8000);
    return () => clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="stack">
      {err && <p className="error">{err}</p>}
      <Card title="Jobs on the PC" actions={<button className="btn small ghost" onClick={load}>Refresh</button>}>
        {jobs.length === 0 ? (
          <p className="muted">No jobs yet. Ask Claude to encode a video or generate an image, and it will show up here.</p>
        ) : (
          <table className="jobs">
            <thead>
              <tr><th>Job</th><th>Status</th><th>Time</th><th>When</th><th /></tr>
            </thead>
            <tbody>
              {jobs.map((j) => (
                <tr key={j.id}>
                  <td className="job-cell" title={(j.argv || []).join(" ") || j.name}>{jobLabel(j.name)}{j.cached && <span className="pill">reused</span>}</td>
                  <td>{STATE_LABEL[j.state] || j.state}{j.exit_code ? ` (code ${j.exit_code})` : ""}</td>
                  <td>{duration(j.duration)}</td>
                  <td>{timeAgo(j.created)}</td>
                  <td className="actions">
                    {platform === "windows" ? (
                      <button className="btn small" onClick={() => openPath(j.folder)}>Open folder</button>
                    ) : j.mac_outputs?.length ? (
                      <>
                        <button className="btn small" onClick={() => openPath(j.mac_outputs[0])}>Open output</button>
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
              <thead><tr><th>Command</th><th>Ran on</th><th>Time</th><th>When</th></tr></thead>
              <tbody>
                {history.slice(0, 30).map((h, i) => (
                  <tr key={i}>
                    <td className="mono small">{(h.name || h.tool).slice(0, 70)}</td>
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
