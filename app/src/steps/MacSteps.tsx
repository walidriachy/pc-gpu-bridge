import { useEffect, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { openUrl, openPath } from "@tauri-apps/plugin-opener";
import { api, errText } from "../lib/api";
import { cleanCode, duration, speedup } from "../lib/format";
import { CheckResult, useAction, type CheckState } from "../components/ui";
import type { StepCtx, StepDef } from "../components/Wizard";

function Welcome({ setDone }: StepCtx) {
  useEffect(() => setDone(true), [setDone]);
  return (
    <div className="prose">
      <p>PC Bridge lets Claude on this Mac send heavy work — video encodes, AI image generation, big data jobs, large builds — to your Windows PC with its powerful graphics card. You keep working on the Mac; the PC does the heavy lifting and the results appear back in your folders.</p>
      <h3>What this app will do on your Mac</h3>
      <ul>
        <li>Help you install <strong>Tailscale</strong> — a free app that creates a private, encrypted connection between your two computers.</li>
        <li>Pair with your PC using a 6-digit code (no passwords or keys to copy).</li>
        <li>Keep the folders you choose in sync with the PC using <strong>Syncthing</strong> (stored in a hidden folder called <code>.pcbridge</code> in your home folder).</li>
        <li>Connect Claude Code and Claude Desktop to PC Bridge — only after you've seen exactly what changes.</li>
      </ul>
      <p className="muted">Before you start: install PC Bridge on your Windows PC and get to its pairing-code screen. It's fine to do both at the same time.</p>
    </div>
  );
}

function TailscaleStep({ setDone }: StepCtx) {
  const [state, setState] = useState<CheckState>({ status: "idle" });
  const check = async () => {
    setState({ status: "running", message: "Checking Tailscale…" });
    try {
      const t = await api.tailscaleCheck();
      if (!t.installed) {
        setState({ status: "fail", message: "Tailscale isn't installed yet.", fix: "Click “Download Tailscale”, install it, then click “Check again”." });
        setDone(false);
      } else if (t.unreadable) {
        setState({ status: "fail", message: "PC Bridge couldn't read Tailscale's status.", fix: `Make sure the Tailscale app is open, then click “Check again”. If this keeps happening, use Help → Run diagnostics. (Details: ${t.detail || "no answer"})` });
        setDone(false);
      } else if (!t.running) {
        setState({ status: "fail", message: "Tailscale is installed but not connected.", fix: "Click the Tailscale icon in the menu bar at the top of the screen and choose “Log in…”. Use the same account you'll use on the PC." });
        setDone(false);
      } else {
        setState({ status: "ok", message: `Connected as “${t.self_name}”.` });
        setDone(true);
      }
    } catch (e) {
      setState({ status: "fail", message: errText(e) });
    }
  };
  useEffect(() => {
    check();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return (
    <div className="prose">
      <button className="btn primary big" onClick={() => openUrl("https://tailscale.com/download/mac")}>
        Download Tailscale
      </button>
      <ol>
        <li>Click <strong>Download Tailscale</strong>. Your browser opens the official Tailscale page — choose the Mac download (or the App Store).</li>
        <li>Open the downloaded file and drag Tailscale to Applications, then open it.</li>
        <li>Click the Tailscale icon in the menu bar (top of the screen) → <strong>Log in</strong>. Pick Google, Microsoft or Apple — <strong>use the same account on the PC</strong>.</li>
        <li>If macOS asks to allow a “VPN configuration”, click <strong>Allow</strong>. (Tailscale only connects your own devices; it doesn't change your internet.)</li>
      </ol>
      <button className="btn" onClick={check}>Check again</button>
      <CheckResult state={state} />
    </div>
  );
}

function PairStep({ setDone }: StepCtx) {
  const [peers, setPeers] = useState<any[]>([]);
  const [host, setHost] = useState("");
  const [code, setCode] = useState("");
  const { state, run } = useAction<any>();
  useEffect(() => {
    api.tailscaleCheck().then((t) => {
      const win = (t.peers || []).filter((p: any) => String(p.os).toLowerCase().includes("windows"));
      setPeers(win);
      if (win.length === 1) setHost(win[0].dns || win[0].name);
    });
  }, []);
  const pair = async () => {
    const r = await run(() => api.pairPc(host, code), "Pairing with your PC…");
    if (r) setDone(true);
  };
  return (
    <div className="prose">
      <ol>
        <li>On the PC, finish the PC Bridge setup until you see a big <strong>6-digit code</strong>.</li>
        <li>Choose your PC below (it's listed because it's on your Tailscale network).</li>
        <li>Type the code and click <strong>Pair</strong>. PC Bridge swaps a secret key automatically and saves it privately on this Mac — you never handle it.</li>
      </ol>
      <label className="field">
        <span>Your PC</span>
        {peers.length > 0 ? (
          <select value={host} onChange={(e) => setHost(e.target.value)}>
            <option value="">Choose…</option>
            {peers.map((p) => (
              <option key={p.ip} value={p.dns || p.name}>
                {p.name} {p.online ? "(online)" : "(offline)"}
              </option>
            ))}
          </select>
        ) : (
          <input placeholder="PC name from Tailscale, e.g. gaming-pc" value={host} onChange={(e) => setHost(e.target.value)} />
        )}
      </label>
      {peers.length === 0 && <p className="muted small">No Windows PC found on Tailscale yet. Make sure Tailscale on the PC is logged in with the same account, or type its name.</p>}
      <label className="field">
        <span>6-digit code</span>
        <input className="code-input" inputMode="numeric" placeholder="123456" value={code} onChange={(e) => setCode(cleanCode(e.target.value))} />
      </label>
      <button className="btn primary big" disabled={!host || code.length !== 6} onClick={pair}>Pair</button>
      <CheckResult state={state.status === "ok" ? { status: "ok", message: "Paired! Your Mac and PC now trust each other." } : { ...state, fix: state.status === "fail" ? "Check the code on the PC screen (it changes after 15 minutes). Make sure PC Bridge is open on the PC and Tailscale shows “Connected” on both." : undefined }} />
    </div>
  );
}

function FoldersStep({ setDone }: StepCtx) {
  const [folders, setFolders] = useState<string[]>([]);
  const { state, run } = useAction<any>();
  useEffect(() => {
    api.getConfig().then((c) => setFolders(c.shared_folders.map((f) => f.mac_path)));
  }, []);
  const add = async () => {
    const picked = await openDialog({ directory: true, multiple: true, title: "Choose folders to share with your PC" });
    const list = Array.isArray(picked) ? picked : picked ? [picked] : [];
    setFolders((f) => Array.from(new Set([...f, ...list.map(String)])));
  };
  const share = async () => {
    const r = await run(() => api.shareFolders(folders), "Setting up syncing (first time downloads Syncthing, about 15 MB)…");
    if (r) setDone(true);
  };
  return (
    <div className="prose">
      <p><strong>Syncing</strong> means a folder exists on both computers and stays identical: add or change a file on one, and it appears on the other a moment later. Your PC keeps its copies inside <code>PCBridge\workspace\</code>.</p>
      <p>Pick the folders where you keep the things the PC will work on — for example <em>Videos</em>, or a project folder. Avoid huge folders you don't need (like your whole home folder).</p>
      <button className="btn" onClick={add}>+ Add folder…</button>
      <ul className="folder-list">
        {folders.map((f) => (
          <li key={f}>
            📁 {f}
            <button className="btn small ghost" onClick={() => setFolders((l) => l.filter((x) => x !== f))}>Remove</button>
          </li>
        ))}
      </ul>
      <button className="btn primary big" disabled={folders.length === 0} onClick={share}>Share these folders</button>
      <CheckResult state={state.status === "ok" ? { status: "ok", message: `Sharing ${folders.length} folder(s). The first sync can take a while for big folders — you can keep going.` } : { ...state, fix: state.status === "fail" ? "Make sure the PC is on and paired (previous step), then try again." : undefined }} />
    </div>
  );
}

function ClaudeStep({ setDone }: StepCtx) {
  const [preview, setPreview] = useState<any>(null);
  const [md, setMd] = useState(true);
  const [shims, setShims] = useState(true);
  const [done, setDoneList] = useState<string[]>([]);
  const { state, run } = useAction<string[]>();
  useEffect(() => {
    api.claudePreview().then(setPreview);
  }, []);
  const connect = async () => {
    const r = await run(() => api.claudeInstall(md, shims), "Connecting…");
    if (r) {
      setDoneList(r);
      setDone(true);
    }
  };
  return (
    <div className="prose">
      <p>This tells Claude (in Claude Code and the Claude Desktop app) that your PC exists and when to use it. Here is everything that will change — a backup copy of each file is kept.</p>
      <ul className="changes">
        {preview?.changes.map((c: any) => (
          <li key={c.what}>
            {c.what}
            <div className="muted small mono">{String(c.path)}</div>
          </li>
        ))}
      </ul>
      <label className="check-row"><input type="checkbox" checked={md} onChange={(e) => setMd(e.target.checked)} /> Add these routing rules to my CLAUDE.md</label>
      <details open>
        <summary>Show the rules text</summary>
        <pre className="snippet">{preview?.claude_md_snippet}</pre>
      </details>
      <label className="check-row"><input type="checkbox" checked={shims} onChange={(e) => setShims(e.target.checked)} /> Install helper commands so <code>ffmpeg</code>, <code>python</code>, <code>blender</code> etc. automatically use the PC for heavy jobs (Docker and npm only in projects you opt in)</label>
      <button className="btn primary big" onClick={connect}>Connect to Claude</button>
      <CheckResult state={state.status === "ok" ? { status: "ok", message: done.join(" · ") } : state} />
      {state.status === "ok" && <p className="muted small">Restart Claude Desktop, and open a new Terminal window, for the changes to take effect.</p>}
    </div>
  );
}

function NetworkStep({ setDone }: StepCtx) {
  const [res, setRes] = useState<any>(null);
  const { state, run } = useAction<any>();
  const go = async () => {
    const r = await run(() => api.networkCheck(), "Measuring the connection (sends 20 MB each way)…");
    if (r) {
      setRes(r);
      setDone(true);
    }
  };
  return (
    <div className="prose">
      <button className="btn primary big" onClick={go}>Measure my connection</button>
      <CheckResult state={state.status === "ok" ? { status: "ok", message: `Upload ${res?.up_mbps.toFixed(0)} Mbit/s · Download ${res?.down_mbps.toFixed(0)} Mbit/s` } : state} />
      {res && <p className="advice">{res.advice}</p>}
    </div>
  );
}

function FinalTestStep({ setDone }: StepCtx) {
  const [res, setRes] = useState<any>(null);
  const { state, run } = useAction<any>();
  const go = async () => {
    const r = await run(() => api.finalTest(), "Running a real job on your PC's graphics card…");
    if (r) {
      setRes(r);
      setDone(true);
    }
  };
  return (
    <div className="prose">
      <p>We'll make a 20-second HD test video on the PC using its NVIDIA video encoder, bring it back to your shared folder, and check whether AI (Python GPU) tools are ready.</p>
      <button className="btn primary big" onClick={go}>Run the test</button>
      <CheckResult state={{ ...state, fix: state.status === "fail" ? "Open Help → Run diagnostics to see which part isn't ready." : undefined }} />
      {res && (
        <div className="result-grid">
          <div><div className="big-num">{duration(res.encode_secs)}</div><div className="muted">on your PC</div></div>
          <div><div className="big-num">~{duration(res.mac_est_secs)}</div><div className="muted">estimated on this Mac</div></div>
          <div><div className="big-num">{speedup(res.encode_secs, res.mac_est_secs)}</div><div className="muted">including the file transfer</div></div>
          <div className="span">
            <button className="btn small" onClick={() => openPath(res.output)}>Open the test video</button>
            <p className="muted small">{res.gpu_python.ok ? `AI tools: ${res.gpu_python.text}` : res.gpu_python.text}</p>
          </div>
        </div>
      )}
    </div>
  );
}

function DoneStep({ setDone }: StepCtx) {
  useEffect(() => setDone(true), [setDone]);
  return (
    <div className="prose">
      <h3>How it works day to day</h3>
      <ol className="bullets">
        <li><strong>Just ask Claude.</strong> “Encode this video”, “generate 4 images”, “run the test suite” — Claude sends heavy work to the PC by itself.</li>
        <li><strong>Watch the menu bar dot:</strong> 🟢 PC ready · 🟡 PC asleep (it will be woken) · 🔴 not connected.</li>
        <li><strong>Results come back by themselves</strong> into the same folder, usually within seconds of the job finishing.</li>
        <li><strong>Memory-smart:</strong> when your Mac is short on memory, or a job needs lots of it, PC Bridge sends it to the PC's memory. Apps you have open on the Mac still use the Mac's memory.</li>
        <li><strong>Never stuck:</strong> if the PC can't be reached, jobs run on the Mac instead and you get a notification. Use “Pause redirecting” in the menu bar any time.</li>
      </ol>
    </div>
  );
}

export const macSteps: StepDef[] = [
  { id: "welcome", title: "Welcome to PC Bridge", what: "A quick look at what PC Bridge will set up on this Mac.", why: "So you know exactly what changes before anything happens.", body: (c) => <Welcome {...c} /> },
  { id: "tailscale", title: "Connect your computers privately", what: "Install Tailscale on this Mac and sign in.", why: "Tailscale creates a private, encrypted link between your Mac and PC that works at home and away — nobody else on the internet can reach your PC through it.", body: (c) => <TailscaleStep {...c} /> },
  { id: "pair", title: "Pair with your PC", what: "Enter the 6-digit code shown on your PC.", why: "Pairing makes your Mac and PC trust each other, so only your Mac can send jobs to your PC.", body: (c) => <PairStep {...c} /> },
  { id: "folders", title: "Choose folders to share", what: "Pick which Mac folders the PC can work on.", why: "The PC needs a copy of your files to work on them, and its results need to come back to you.", body: (c) => <FoldersStep {...c} /> },
  { id: "claude", title: "Connect to Claude", what: "Let Claude Code and Claude Desktop use your PC.", why: "This gives Claude new tools (like “run on PC”) and tells it which jobs belong on the PC. An “MCP connection” is simply how apps give Claude extra abilities.", body: (c) => <ClaudeStep {...c} /> },
  { id: "network", title: "Check the connection speed", what: "Measure how fast files move between your Mac and PC.", why: "PC Bridge uses this to decide when sending a job is worth it — copying a huge file over slow Wi-Fi can take longer than doing the job on the Mac.", optional: true, body: (c) => <NetworkStep {...c} /> },
  { id: "test", title: "Final test", what: "Run a real job on your PC's graphics card.", why: "Proves everything works end-to-end and shows you how much time the PC saves.", optional: true, body: (c) => <FinalTestStep {...c} /> },
  { id: "done", title: "You're done! 🎉", what: "PC Bridge is ready.", why: "Here's what to expect from now on.", body: (c) => <DoneStep {...c} /> },
];
