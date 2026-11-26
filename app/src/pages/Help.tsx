import { useState } from "react";
import { api, errText, type Check, type Platform } from "../lib/api";
import { Card, Icon, Pill } from "../components/ui";

/** A small folder glyph drawn inside the diagram (same hairline style as the app's icons). */
function FolderGlyph({ x, y }: { x: number; y: number }) {
  return <path className="ico" transform={`translate(${x} ${y}) scale(0.62)`} d="M3.5 7.5A1.5 1.5 0 0 1 5 6h4.2l1.8 2H19a1.5 1.5 0 0 1 1.5 1.5v8A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5v-10Z" />;
}

function Diagram() {
  return (
    <svg viewBox="0 0 720 240" className="diagram" role="img" aria-label="How PC Bridge works">
      <defs>
        <marker id="a-out" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto">
          <path d="M0 0 L10 5 L0 10z" fill="#5298ff" />
        </marker>
        <marker id="a-back" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto">
          <path d="M0 0 L10 5 L0 10z" fill="#2ed3b7" />
        </marker>
      </defs>
      <rect x="10" y="20" width="250" height="200" rx="16" className="box mac" />
      <text x="135" y="48" textAnchor="middle" className="k">YOUR MAC</text>
      <text x="135" y="70" textAnchor="middle" className="h">The brain</text>
      <text x="135" y="100" textAnchor="middle">You + Claude</text>
      <text x="135" y="120" textAnchor="middle" className="s">decides what's heavy</text>
      <FolderGlyph x={78} y={140} />
      <text x="146" y="155" textAnchor="middle">shared folders</text>
      <text x="135" y="186" textAnchor="middle" className="s">browser, editors, Claude use</text>
      <text x="135" y="204" textAnchor="middle" className="s">the Mac's own memory</text>

      <rect x="460" y="20" width="250" height="200" rx="16" className="box pc" />
      <text x="585" y="48" textAnchor="middle" className="k">YOUR PC</text>
      <text x="585" y="70" textAnchor="middle" className="h">The muscle</text>
      <text x="585" y="100" textAnchor="middle">NVIDIA GPU · lots of RAM</text>
      <text x="585" y="120" textAnchor="middle" className="s">runs the heavy jobs</text>
      <FolderGlyph x={510} y={140} />
      <text x="596" y="155" textAnchor="middle">PCBridge\workspace</text>
      <text x="585" y="186" textAnchor="middle" className="s">only jobs sent here use</text>
      <text x="585" y="204" textAnchor="middle" className="s">the PC's memory &amp; GPU</text>

      <path d="M270 92 H450" className="arrow out" markerEnd="url(#a-out)" />
      <text x="360" y="82" textAnchor="middle" className="s">job: “encode video”</text>
      <path d="M450 132 H270" className="arrow back" markerEnd="url(#a-back)" />
      <text x="360" y="152" textAnchor="middle" className="s">output + results</text>
      <path className="ico" transform="translate(296 186) scale(0.58)" d="M6.5 11h11v9h-11zM8.5 11V8a3.5 3.5 0 0 1 7 0v3" />
      <text x="372" y="199" textAnchor="middle" className="s">private Tailscale link</text>
    </svg>
  );
}

const FAQ: [string, string][] = [
  ["Does this make my Mac use the PC's memory?", "No. Apps open on your Mac (the browser, editors, Claude itself) always use the Mac's own memory (RAM). Only jobs that PC Bridge sends to the PC use the PC's memory and its graphics card. That's why sending big jobs away frees up your Mac: they simply don't run there."],
  ["Which jobs go to the PC?", "Video encodes and renders, AI image/video generation, AI models, long GPU Python jobs, plus memory-hungry work like big data processing (pandas/numpy), Docker containers, and large npm/Next.js builds and test suites. Quick things (small scripts, git, editing files) stay on the Mac because sending them would be slower."],
  ["What happens when my Mac is low on memory?", "PC Bridge watches your Mac's memory pressure. When it gets high, or a job is estimated to need more memory than the Mac has free, heavy jobs go to the PC automatically, so your Mac doesn't slow down."],
  ["What if the PC is off or asleep?", "PC Bridge tries to wake it (Wake-on-LAN, when you're on the same home network). If it still can't reach it, the job runs on your Mac instead and you get a notification. Nothing gets stuck."],
  ["Where do results go?", "Back into the same Mac folder you were working in. The PC also keeps a copy in PCBridge\\jobs\\<date>\\ for 14 days."],
  ["Does it work away from home?", "Yes. Tailscale connects your Mac and PC from anywhere, as long as the PC is on. (Waking it from sleep only works at home.) Big files will copy slower over the internet."],
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
      <Card title="How it works" kicker="ARCHITECTURE" icon="bridge" primary>
        <Diagram />
        <p>Think of your Mac as the <strong>brain</strong> and your PC as the <strong>muscle</strong>. You and Claude work on the Mac. When a job is heavy, PC Bridge sends it to the PC, the PC does the work with its graphics card and memory, and the result appears back in your Mac folder.</p>
        <p className="callout"><strong>About memory (RAM):</strong> apps open on the Mac (browser, editors, Claude) still use the Mac's RAM. Only jobs sent to the PC use the PC's RAM. PC Bridge doesn't add memory to your Mac; it moves heavy jobs away from it.</p>
      </Card>

      <Card title="Something not working?" kicker="DIAGNOSTICS" icon="pulse" actions={<button className="btn primary" disabled={running} onClick={diagnose}>{running ? <><span className="spinner tiny" /> Checking…</> : <><Icon name="status" size={14} /> Run diagnostics</>}</button>}>
        {err && <p className="error">{err}</p>}
        {checks ? (
          <ul className="diag">
            {checks.map((c) => (
              <li key={c.name} className={c.ok ? "ok" : "fail"}>
                {c.ok ? <Pill tone="teal" icon="check">OK</Pill> : <Pill tone="rose" icon="x">Fix</Pill>}
                <div className="diag-main">
                  <strong>{c.name}</strong>
                  {c.detail && <span className="muted small">{c.detail}</span>}
                  {!c.ok && c.fix && <div className="fix">{c.fix}</div>}
                </div>
              </li>
            ))}
          </ul>
        ) : (
          <p className="muted">Checks every part of PC Bridge and explains any problem in plain English.</p>
        )}
      </Card>

      <Card title="Questions" kicker="FAQ" icon="help">
        {FAQ.map(([q, a]) => (
          <details key={q} className="faq">
            <summary>{q}<Icon name="right" size={15} /></summary>
            <p>{a}</p>
          </details>
        ))}
      </Card>

      <Card title="Is it safe?" kicker="SECURITY" icon="shield">
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
