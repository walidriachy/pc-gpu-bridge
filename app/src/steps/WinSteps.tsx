import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, errText, type WinState } from "../lib/api";
import { CheckResult, CopyBox, useAction, type CheckState } from "../components/ui";
import { InstallStatus, usePrograms, useProgramInstall, WHERE_PROGRAMS_GO } from "../components/Programs";
import { driveName, hasRoom, sizeText, spaceNeededGb } from "../lib/format";
import type { StepCtx, StepDef } from "../components/Wizard";

function useWinState(): [WinState | null, (s: WinState) => void] {
  const [s, set] = useState<WinState | null>(null);
  useEffect(() => {
    api.winGetState().then(set);
  }, []);
  const save = (n: WinState) => {
    set(n);
    api.winSaveState(n);
  };
  return [s, save];
}

function Welcome({ setDone }: StepCtx) {
  const [ws, save] = useWinState();
  useEffect(() => setDone(true), [setDone]);
  if (!ws) return null;
  return (
    <div className="prose">
      <p>This PC will become the “muscle” for your Mac: when Claude on your Mac has heavy work (video encodes, AI image generation, big builds), it sends the job here, and the results go back to the Mac.</p>
      <h3>Where PC Bridge keeps its files</h3>
      <p>Everything lives in <strong>one folder</strong>. PC Bridge never writes anywhere else (except the programs below, which install normally).</p>
      <div className="row">
        {["D:\\PCBridge", "C:\\PCBridge"].map((r) => (
          <label key={r} className="radio">
            <input type="radio" checked={ws.root === r} onChange={() => save({ ...ws, root: r })} /> {r}
          </label>
        ))}
      </div>
      <h3>Programs this setup installs (with your OK at each step)</h3>
      <ul>
        <li><strong>Tailscale</strong> — private, encrypted connection to your Mac</li>
        <li><strong>FFmpeg</strong> — video tool that can use your NVIDIA card</li>
        <li><strong>Python</strong> — runs the PC Bridge helper and AI scripts</li>
        <li><strong>Syncthing</strong> — keeps shared folders identical on both computers</li>
        <li><strong>Claude Code</strong> — a “worker Claude” that can do whole tasks on this PC</li>
        <li>Optional: <strong>Ollama</strong> (local AI chat models), <strong>PyTorch</strong> (AI in Python) and <strong>ComfyUI</strong> (image/video generation)</li>
      </ul>
    </div>
  );
}

function GpuStep({ setDone }: StepCtx) {
  const [state, setState] = useState<CheckState>({ status: "idle" });
  const check = async () => {
    setState({ status: "running", message: "Looking for your NVIDIA graphics card…" });
    try {
      const g = await api.winCheckGpu();
      if (!g.found) {
        setState({ status: "fail", message: "No NVIDIA driver found.", fix: "Click “Get the NVIDIA driver”, install the NVIDIA App or the latest Game Ready / Studio driver, restart the PC, then click “Check again”." });
        setDone(false);
      } else if (!g.driver_ok) {
        setState({ status: "fail", message: `Found ${g.name}, but the driver (${g.driver}) is old.`, fix: "Update the driver with “Get the NVIDIA driver” — newer drivers make video encoding and AI tools work properly. You can also continue and update later." });
        setDone(true);
      } else {
        setState({ status: "ok", message: `${g.name} with ${(g.vram_mb / 1024).toFixed(0)} GB of video memory — driver ${g.driver}.` });
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
      <CheckResult state={state} />
      <div className="row">
        <button className="btn" onClick={check}>Check again</button>
        <button className="btn ghost" onClick={() => openUrl("https://www.nvidia.com/en-us/software/nvidia-app/")}>Get the NVIDIA driver</button>
      </div>
    </div>
  );
}

function TailscaleStep({ setDone }: StepCtx) {
  const install = useAction<any>();
  const [state, setState] = useState<CheckState>({ status: "idle" });
  const check = async () => {
    setState({ status: "running", message: "Checking Tailscale…" });
    const t = await api.winTailscaleCheck().catch(() => null);
    if (!t?.installed) {
      setState({ status: "fail", message: "Tailscale isn't installed yet.", fix: "Click “Do it for me”." });
      setDone(false);
    } else if (!t.running) {
      setState({ status: "fail", message: "Tailscale is installed but not signed in.", fix: "Click “Open Tailscale to sign in” and log in with the SAME account you use on your Mac." });
      setDone(false);
    } else {
      setState({ status: "ok", message: `Connected as “${t.self_name}”. Your Mac will find this PC by that name.` });
      setDone(true);
    }
  };
  useEffect(() => {
    check();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return (
    <div className="prose">
      <button className="btn primary big" onClick={async () => { await install.run(() => api.winInstall("tailscale"), "Installing Tailscale (Windows may ask “Do you want to allow…?” — click Yes)…"); check(); }}>Do it for me</button>
      <CheckResult state={install.state} okText="Tailscale installed" />
      <ol>
        <li>After installing, click <strong>Open Tailscale to sign in</strong>. A browser window opens.</li>
        <li>Log in with the <strong>same</strong> Google / Microsoft / Apple account you'll use on the Mac.</li>
        <li>Come back here and click <strong>Check again</strong>.</li>
      </ol>
      <div className="row">
        <button className="btn" onClick={() => api.winOpenTailscale().catch((e) => setState({ status: "fail", message: errText(e) }))}>Open Tailscale to sign in</button>
        <button className="btn ghost" onClick={check}>Check again</button>
      </div>
      <CheckResult state={state} />
    </div>
  );
}

const TOOLS = [
  { id: "ffmpeg", name: "FFmpeg", desc: "video encoding with the NVIDIA encoder" },
  { id: "python", name: "Python", desc: "runs the PC Bridge helper and AI scripts" },
  { id: "syncthing", name: "Syncthing", desc: "keeps shared folders in sync" },
];

function ToolsStep({ setDone }: StepCtx) {
  const [status, setStatus] = useState<Record<string, CheckState>>({});
  const agent = useAction<any>();
  const refresh = async () => {
    const out: Record<string, CheckState> = {};
    for (const t of TOOLS) {
      const r = await api.winCheckTool(t.id).catch(() => null);
      out[t.id] = r?.installed ? { status: "ok", message: `${t.name} is ready ${r.detail ? `(${r.detail})` : ""}` } : { status: "idle" };
    }
    setStatus(out);
    return out;
  };
  useEffect(() => {
    refresh();
     
  }, []);
  const installAll = async () => {
    for (const t of TOOLS) {
      if (status[t.id]?.status === "ok") continue;
      setStatus((s) => ({ ...s, [t.id]: { status: "running", message: `Installing ${t.name}…` } }));
      try {
        await api.winInstall(t.id);
        setStatus((s) => ({ ...s, [t.id]: { status: "ok", message: `${t.name} installed` } }));
      } catch (e) {
        setStatus((s) => ({ ...s, [t.id]: { status: "fail", message: `${t.name}: ${errText(e)}`, fix: "Check your internet connection and click “Install everything” again." } }));
        return;
      }
    }
    const r = await agent.run(() => api.winSetupAgent(), "Installing and starting the PC Bridge helper…");
    if (r) setDone(true);
  };
  return (
    <div className="prose">
      <p>One click installs these using Windows' built-in installer (<em>winget</em>), then installs and starts the PC Bridge helper in the background. It can take a few minutes.</p>
      <button className="btn primary big" onClick={installAll}>Install everything</button>
      <ul className="tool-list">
        {TOOLS.map((t) => (
          <li key={t.id}>
            <strong>{t.name}</strong> — {t.desc}
            <CheckResult state={status[t.id] || { status: "idle" }} />
          </li>
        ))}
        <li>
          <strong>PC Bridge helper</strong> — receives jobs from your Mac
          <CheckResult state={agent.state.status === "ok" ? { status: "ok", message: "Running in the background" } : agent.state} />
        </li>
      </ul>
    </div>
  );
}

function ClaudeStep({ setDone }: StepCtx) {
  const install = useAction<any>();
  const [state, setState] = useState<CheckState>({ status: "idle" });
  const check = async () => {
    setState({ status: "running", message: "Asking Claude Code on this PC to say “ready”…" });
    const r = await api.winClaudeCheck().catch(() => null);
    if (!r?.installed) {
      setState({ status: "fail", message: "Claude Code isn't installed yet.", fix: "Click “Do it for me”." });
      setDone(false);
    } else if (!r.logged_in) {
      setState({ status: "fail", message: "Claude Code is installed but not logged in.", fix: "Click “Log in”, follow the steps in the black window and your browser, then click “Check again”." });
      setDone(false);
    } else {
      setState({ status: "ok", message: "The worker Claude is ready." });
      setDone(true);
    }
  };
  return (
    <div className="prose">
      <p>The <strong>worker Claude</strong> lets Claude on your Mac hand over a whole task (“make thumbnails for all these videos”) to this PC. It uses your existing Claude account.</p>
      <button className="btn primary big" onClick={async () => { await install.run(() => api.winInstall("claude"), "Installing Claude Code…"); }}>Do it for me</button>
      <CheckResult state={install.state} okText="Claude Code installed" />
      <ol>
        <li>Click <strong>Log in</strong>. A black window opens and starts Claude Code.</li>
        <li>Follow its instructions — your browser opens so you can sign in to your Claude account.</li>
        <li>When it says you're logged in, close the black window and click <strong>Check again</strong>.</li>
      </ol>
      <div className="row">
        <button className="btn" onClick={() => api.winClaudeLogin()}>Log in</button>
        <button className="btn ghost" onClick={check}>Check again</button>
      </div>
      <CheckResult state={state} />
    </div>
  );
}

function PytorchOption({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  const progs = usePrograms();
  const torch = useProgramInstall(() => {
    progs.reload();
  });
  const [missing, setMissing] = useState(false);
  const info = progs.info;
  const p = info?.programs.find((x) => x.key === "pytorch");
  const roomy = !info || !p || hasRoom(info.free_gb, p.size_gb);
  const install = async () => {
    const latest = info ?? (await progs.reload());
    const prog = latest?.programs.find((x) => x.key === "pytorch");
    setMissing(!!latest && !prog);
    if (prog) await torch.install(prog, latest);
  };
  return (
    <div className="option">
      <label className="check-row">
        <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
        <span><strong>PyTorch (GPU)</strong> — needed for AI in Python, like Whisper (speech to text) and Stable Diffusion. It's big ({sizeText(p?.size_gb ?? 6)}).</span>
      </label>
      {checked && (
        <div>
          {p?.installed ? (
            <CheckResult state={{ status: "ok", message: "PyTorch is installed and can use your graphics card." }} />
          ) : (
            <>
              <button className="btn" disabled={!!torch.busy || !roomy} onClick={install}>
                {torch.busy ? "Installing…" : "Install PyTorch"}
              </button>
              {info && p && (
                <p className={roomy ? "muted small" : "advice"}>
                  {roomy
                    ? `Free space on the ${driveName(info.drive)} drive: ${info.free_gb >= 0 ? `${info.free_gb} GB` : "unknown"} (needs about ${spaceNeededGb(p.size_gb)} GB).`
                    : `⚠️ Not enough free space: ${info.free_gb} GB free on the ${driveName(info.drive)} drive, about ${spaceNeededGb(p.size_gb)} GB needed. Free up space in Windows Settings → System → Storage, then come back.`}
                </p>
              )}
              <InstallStatus state={torch.states.pytorch} />
              {!torch.states.pytorch && progs.error && <CheckResult state={{ status: "fail", message: progs.error }} />}
              {missing && (
                <CheckResult state={{ status: "fail", message: "The helper on this PC is out of date.", fix: "Go back to step 4 and click “Install everything” again to update it." }} />
              )}
            </>
          )}
          <p className="muted small">{WHERE_PROGRAMS_GO}</p>
        </div>
      )}
    </div>
  );
}

function OptionalStep({ setDone }: StepCtx) {
  const [ws, save] = useWinState();
  const ollama = useAction<any>();
  useEffect(() => setDone(true), [setDone]);
  if (!ws) return null;
  return (
    <div className="prose">
      <div className="option">
        <label className="check-row">
          <input type="checkbox" checked={ws.want_ollama} onChange={(e) => save({ ...ws, want_ollama: e.target.checked })} />
          <span><strong>Ollama</strong> — runs AI chat models (like Llama or Qwen) on your graphics card, privately, with no internet needed once downloaded. Claude can ask them questions.</span>
        </label>
        {ws.want_ollama && (
          <>
            <button className="btn" onClick={() => ollama.run(() => api.winInstall("ollama"), "Installing Ollama…")}>Install Ollama</button>
            <CheckResult state={ollama.state} okText="Ollama installed. Models download the first time they're used (several GB each)." />
          </>
        )}
      </div>
      <PytorchOption checked={!!ws.want_pytorch} onChange={(v) => save({ ...ws, want_pytorch: v })} />
      <div className="option">
        <label className="check-row">
          <input type="checkbox" checked={ws.want_comfy} onChange={(e) => save({ ...ws, want_comfy: e.target.checked })} />
          <span><strong>ComfyUI</strong> — creates images and videos with AI models (like Stable Diffusion / Flux). Claude can send it image requests.</span>
        </label>
        {ws.want_comfy && (
          <div>
            <button className="btn" onClick={() => openUrl("https://www.comfy.org/download")}>Open the ComfyUI download page</button>
            <p className="muted small">Download “ComfyUI Desktop for Windows (NVIDIA)”, install it, and open it once. PC Bridge talks to it automatically while it's running.</p>
          </div>
        )}
      </div>
      <details className="option">
        <summary><strong>Advanced:</strong> run AI tools inside WSL2 (Linux on Windows)</summary>
        <p className="muted">Some AI tools run better on Linux. WSL2 lets Windows run Ubuntu with full access to your graphics card. Most people don't need this — leave it off unless a tool asks for Linux.</p>
        <CopyBox command="wsl --install -d Ubuntu" explain="Paste into “Terminal (Admin)”: right-click the Start button → Terminal (Admin). It installs Ubuntu; restart when asked. Then use the AI tool's Linux instructions inside Ubuntu." />
      </details>
    </div>
  );
}

function WolStep({ setDone }: StepCtx) {
  const wol = useAction<{ adapter: string; mac: string }>();
  const [res, setRes] = useState<{ adapter: string; mac: string } | null>(null);
  return (
    <div className="prose">
      <p><strong>Wake-on-LAN</strong> means your Mac can switch the PC on from sleep by sending a special “wake up” signal over your home network. The PC can then save power when idle.</p>
      <button className="btn primary big" onClick={async () => { const r = await wol.run(() => api.winConfigureWol(), "Changing the network card setting (click “Yes” on the Windows prompt)…"); if (r) { setRes(r); setDone(true); } }}>Do it for me</button>
      <CheckResult state={wol.state.status === "ok" ? { status: "ok", message: `Turned on for “${res?.adapter}”. Windows “Fast startup” was also turned off so waking works.` } : { ...wol.state, fix: wol.state.status === "fail" ? "Wake-on-LAN needs the PC plugged in with an Ethernet cable. You can skip this — the PC then just needs to stay on." : undefined }} />
      <details>
        <summary>Also check the BIOS setting (optional, needs a restart)</summary>
        <div className="bios">
          <svg viewBox="0 0 420 110" className="bios-svg" aria-hidden>
            <rect x="5" y="10" width="120" height="80" rx="8" className="b1" />
            <text x="65" y="48" textAnchor="middle">Restart PC</text>
            <text x="65" y="66" textAnchor="middle" className="s">press Del / F2</text>
            <path d="M130 50h30" className="arrow" />
            <rect x="165" y="10" width="120" height="80" rx="8" className="b1" />
            <text x="225" y="40" textAnchor="middle">Find “Power”</text>
            <text x="225" y="58" textAnchor="middle" className="s">or “APM” /</text>
            <text x="225" y="74" textAnchor="middle" className="s">“Advanced”</text>
            <path d="M290 50h30" className="arrow" />
            <rect x="325" y="10" width="90" height="80" rx="8" className="b2" />
            <text x="370" y="40" textAnchor="middle">Enable</text>
            <text x="370" y="58" textAnchor="middle" className="s">“Wake on LAN”</text>
            <text x="370" y="74" textAnchor="middle" className="s">Save &amp; Exit</text>
          </svg>
          <ol>
            <li>Restart the PC. As soon as the screen lights up, tap <kbd>Delete</kbd> (or <kbd>F2</kbd>) repeatedly until the setup screen (BIOS) appears.</li>
            <li>Look for a menu called <strong>Power</strong>, <strong>APM</strong> or <strong>Advanced</strong>.</li>
            <li>Find <strong>Wake on LAN</strong>, <strong>Power On by PCI-E</strong> or <strong>Resume by LAN</strong> and set it to <strong>Enabled</strong>.</li>
            <li>Press <kbd>F10</kbd> to save and exit. If you can't find it, don't worry — waking from sleep usually works without it.</li>
          </ol>
        </div>
      </details>
    </div>
  );
}

function PowerStep({ setDone }: StepCtx) {
  const power = useAction<any>();
  return (
    <div className="prose">
      <p>We'll let the PC go to sleep after 30 minutes of doing nothing (saving electricity), allow it to be woken up, keep it awake while a job is running, and start PC Bridge automatically when Windows starts.</p>
      <button
        className="btn primary big"
        onClick={async () => {
          const r = await power.run(async () => {
            await api.winPower();
            await api.winSetAutostart(true);
            return true;
          }, "Applying power settings…");
          if (r) setDone(true);
        }}
      >
        Do it for me
      </button>
      <CheckResult state={power.state} okText="Sleep after 30 idle minutes · wakes for jobs · starts with Windows" />
    </div>
  );
}

function PairCodeStep({ setDone }: StepCtx) {
  const [info, setInfo] = useState<{ code: string; pc_name: string; pc_ip: string } | null>(null);
  const [err, setErr] = useState("");
  const load = (fresh: boolean) =>
    api.winPairingCode(fresh).then(setInfo).catch((e) => setErr(errText(e)));
  useEffect(() => {
    load(false);
    setDone(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return (
    <div className="prose center">
      {info ? (
        <>
          <p>On your Mac, open PC Bridge and enter this code when asked:</p>
          <div className="pair-code">{info.code.slice(0, 3)} {info.code.slice(3)}</div>
          <p>Your PC's name on Tailscale: <strong>{info.pc_name}</strong></p>
          <p className="muted small">The code works once and expires after 15 minutes.</p>
          <button className="btn ghost" onClick={() => load(true)}>Get a new code</button>
        </>
      ) : (
        <CheckResult state={err ? { status: "fail", message: err, fix: "Go back to step 4 and make sure the PC Bridge helper is installed." } : { status: "running", message: "Getting a code…" }} />
      )}
    </div>
  );
}

export const winSteps: StepDef[] = [
  { id: "welcome", title: "Welcome to PC Bridge", what: "A quick look at what PC Bridge will do to this PC.", why: "So you know exactly what gets installed and where files go before anything happens.", body: (c) => <Welcome {...c} /> },
  { id: "gpu", title: "Check your graphics card", what: "Make sure your NVIDIA card and its driver are working.", why: "The graphics card (GPU) is what makes this PC fast at video and AI work; the driver is the software that lets programs use it.", body: (c) => <GpuStep {...c} /> },
  { id: "tailscale", title: "Connect to your Mac privately", what: "Install Tailscale and sign in.", why: "Tailscale makes a private, encrypted link between this PC and your Mac — it works even when you're away from home, and nobody else can reach the PC through it.", body: (c) => <TailscaleStep {...c} /> },
  { id: "tools", title: "Install the tools", what: "Install FFmpeg, Python, Syncthing and the PC Bridge helper.", why: "These are the programs that actually do the work and keep your files in sync.", body: (c) => <ToolsStep {...c} /> },
  { id: "claude", title: "Set up the worker Claude", what: "Install Claude Code on this PC and log in.", why: "So Claude on your Mac can hand whole tasks to a Claude running right next to the graphics card.", optional: true, body: (c) => <ClaudeStep {...c} /> },
  { id: "optional", title: "Optional AI tools", what: "Choose whether to add local AI models.", why: "These are only needed if you want AI chat models or image generation running on your own PC. They're off by default.", optional: true, body: (c) => <OptionalStep {...c} /> },
  { id: "wol", title: "Wake-on-LAN", what: "Let your Mac wake this PC up when there's a job.", why: "So the PC can sleep (and save power) when idle, but still be there when you need it.", optional: true, body: (c) => <WolStep {...c} /> },
  { id: "power", title: "Power settings", what: "Allow sleep, but wake up for jobs — and start with Windows.", why: "Keeps electricity costs down without you having to remember to switch anything on.", optional: true, body: (c) => <PowerStep {...c} /> },
  { id: "pair", title: "Pair with your Mac", what: "Show the 6-digit code to type on your Mac.", why: "Pairing makes the two computers trust each other — only your Mac can send jobs here.", body: (c) => <PairCodeStep {...c} /> },
];
