import { useState } from "react";
import { api, errText, type Check, type Platform } from "../lib/api";
import { Card } from "../components/ui";

function Diagram() {
  return (
    <svg viewBox="0 0 720 230" className="diagram" role="img" aria-label="How PC Bridge works">
      <rect x="10" y="20" width="250" height="190" rx="14" className="box mac" />
      <text x="135" y="48" textAnchor="middle" className="h">Your Mac — the brain</text>
      <text x="135" y="80" textAnchor="middle">You + Claude</text>
      <text x="135" y="104" textAnchor="middle" className="s">decides what's heavy</text>
      <text x="135" y="140" textAnchor="middle">📁 shared folders</text>
      <text x="135" y="175" textAnchor="middle" className="s">browser, editors, Claude use</text>
      <text x="135" y="193" textAnchor="middle" className="s">the Mac's own memory</text>

      <rect x="460" y="20" width="250" height="190" rx="14" className="box pc" />
      <text x="585" y="48" textAnchor="middle" className="h">Your PC — the muscle</text>
      <text x="585" y="80" textAnchor="middle">NVIDIA GPU · lots of RAM</text>
      <text x="585" y="104" textAnchor="middle" className="s">runs the heavy jobs</text>
      <text x="585" y="140" textAnchor="middle">📁 PCBridge\workspace</text>
      <text x="585" y="175" textAnchor="middle" className="s">only jobs sent here use</text>
      <text x="585" y="193" textAnchor="middle" className="s">the PC's memory &amp; GPU</text>

      <path d="M270 85 H450" className="arrow" markerEnd="url(#a)" />
      <text x="360" y="75" textAnchor="middle" className="s">job: “encode video”</text>
      <path d="M450 125 H270" className="arrow" markerEnd="url(#a)" />
      <text x="360" y="145" textAnchor="middle" className="s">output + results</text>
      <text x="360" y="190" textAnchor="middle" className="s">🔒 private Tailscale link</text>
      <defs>
        <marker id="a" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto">
          <path d="M0 0 L10 5 L0 10z" className="arrowhead" />
        </marker>
      </defs>
    </svg>
  );
}

const FAQ: [string, string][] = [
  ["Does this make my Mac use the PC's memory?", "No. Apps open on your Mac — the browser, editors, Claude itself — always use the Mac's own memory (RAM). Only jobs that PC Bridge sends to the PC use the PC's memory and its graphics card. That's why sending big jobs away frees up your Mac: they simply don't run there."],
  ["Which jobs go to the PC?", "Video encodes and renders, AI image/video generation, AI models, long GPU Python jobs — and memory-hungry work like big data processing (pandas/numpy), Docker containers, and large npm/Next.js builds and test suites. Quick things (small scripts, git, editing files) stay on the Mac because sending them would be slower."],
  ["What happens when my Mac is low on memory?", "PC Bridge watches your Mac's memory pressure. When it gets high — or a job is estimated to need more memory than the Mac has free — heavy jobs go to the PC automatically, so your Mac doesn't slow down."],
  ["What if the PC is off or asleep?", "PC Bridge tries to wake it (Wake-on-LAN, when you're on the same home network). If it still can't reach it, the job runs on your Mac instead and you get a notification. Nothing gets stuck."],
  ["Where do results go?", "Back into the same Mac folder you were working in. The PC also keeps a copy in PCBridge\\jobs\\<date>\\ for 14 days."],
  ["Does it work away from home?", "Yes — Tailscale connects your Mac and PC from anywhere, as long as the PC is on. (Waking it from sleep only works at home.) Big files will copy slower over the internet."],
  ["Is it slower for small jobs?", "Sending a job takes a moment, so PC Bridge only redirects when it estimates the PC will be faster overall, including copying files. It measures your connection and learns from past jobs."],
  ["How do I stop it?", "Click the dot in the menu bar → “Pause redirecting”. Everything then runs on the Mac as before. Settings → “Disconnect from Claude” removes it from Claude entirely."],
];

export function Help({ platform }: { platform: Platform }) {
  const [checks, setChecks] = useState<Check[] | null>(null);
  const [running, setRunning] = useState(false);
  const [err, setErr] = useState("");
  const diagnose = async () => {
    setRunning(true);
    setErr("");
    try {
      setChecks(await (platform === "windows" ? api.diagnosticsWin() : api.diagnosticsMac()));
    } catch (e) {
      setErr(errText(e));
    }
    setRunning(false);
  };
  return (
    <div className="stack">
      <Card title="How it works">
        <Diagram />
        <p>Think of your Mac as the <strong>brain</strong> and your PC as the <strong>muscle</strong>. You and Claude work on the Mac. When a job is heavy, PC Bridge sends it to the PC, the PC does the work with its graphics card and memory, and the result appears back in your Mac folder.</p>
        <p className="callout"><strong>About memory (RAM):</strong> apps open on the Mac (browser, editors, Claude) still use the Mac's RAM. Only jobs sent to the PC use the PC's RAM. PC Bridge doesn't add memory to your Mac — it moves heavy jobs away from it.</p>
      </Card>

      <Card title="Something not working?" actions={<button className="btn primary" disabled={running} onClick={diagnose}>{running ? "Checking…" : "Run diagnostics"}</button>}>
        {err && <p className="error">{err}</p>}
        {checks ? (
          <ul className="diag">
            {checks.map((c) => (
              <li key={c.name} className={c.ok ? "ok" : "fail"}>
                <span>{c.ok ? "✅" : "❌"} <strong>{c.name}</strong> {c.detail && <span className="muted small">— {c.detail}</span>}</span>
                {!c.ok && c.fix && <div className="fix">{c.fix}</div>}
              </li>
            ))}
          </ul>
        ) : (
          <p className="muted">Checks every part of PC Bridge and explains any problem in plain English.</p>
        )}
      </Card>

      <Card title="Questions">
        {FAQ.map(([q, a]) => (
          <details key={q} className="faq">
            <summary>{q}</summary>
            <p>{a}</p>
          </details>
        ))}
      </Card>

      <Card title="Is it safe?">
        <ul>
          <li><strong>Private connection.</strong> Your Mac and PC talk only through Tailscale, an encrypted private network that contains just your own devices. PC Bridge on the PC doesn't listen on your home Wi-Fi or the internet at all.</li>
          <li><strong>Pairing code.</strong> The 6-digit code works once and expires after 15 minutes. It's exchanged for a long secret key that's stored in your Mac's Keychain and checked on every request. You never see or copy it.</li>
          <li><strong>One folder only.</strong> On the PC, PC Bridge only reads and writes inside the PCBridge folder and refuses any other location.</li>
          <li><strong>Optional allowlist.</strong> You can limit which programs the PC will run (the “allowlist” setting in PCBridge\config\settings.json).</li>
          <li><strong>The worker Claude</strong> on the PC works in its own scratch folder (PCBridge\worker) using your Claude account.</li>
        </ul>
      </Card>
    </div>
  );
}
