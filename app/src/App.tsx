import { useEffect, useState } from "react";
import { api, errText, type Platform } from "./lib/api";
import { Wizard } from "./components/Wizard";
import { macSteps } from "./steps/MacSteps";
import { winSteps } from "./steps/WinSteps";
import { Status } from "./pages/Status";
import { Jobs } from "./pages/Jobs";
import { Settings } from "./pages/Settings";
import { Help } from "./pages/Help";
import { Dot, Icon, type IconName } from "./components/ui";
import { usePoll } from "./lib/poll";

type Tab = "status" | "jobs" | "settings" | "help";
const TABS: [Tab, string, IconName][] = [
  ["status", "Status", "status"],
  ["jobs", "Jobs", "jobs"],
  ["settings", "Settings", "settings"],
  ["help", "Help", "help"],
];

type Link = { color: "green" | "yellow" | "red"; host: string; state: string };

/** The connection shown in the sidebar (status dot and footer). A slow poll next to the Status page's own. */
function useLink(platform: Platform): Link | null {
  const { data } = usePoll<Link>(
    `shell-link-${platform}`,
    async () => {
      if (platform === "windows") {
        const s = await api.winStatus();
        const ok = s.agent_running && s.tailscale?.running;
        return {
          color: ok ? "green" : s.agent_running ? "yellow" : "red",
          host: s.tailscale?.self_name || "This PC",
          state: ok ? "Ready for jobs" : s.agent_running ? "Tailscale is off" : "Helper not running",
        };
      }
      const s = await api.statusSnapshot();
      const refused = s.error_kind === "unauthorized";
      return {
        color: s.reachable ? "green" : s.paired && !refused ? "yellow" : "red",
        host: s.pc_hostname || (s.paired ? "Your PC" : "No PC paired"),
        state: s.reachable ? (s.redirect_enabled ? "Connected · sending jobs" : "Connected · paused") : refused ? "Pairing refused" : s.paired ? "Sleeping or offline" : "Not connected",
      };
    },
    15000,
    (l) => l.color === "green",
  );
  return data;
}

function Sidebar({ b, tab, setTab }: { b: Boot; tab: Tab; setTab: (t: Tab) => void }) {
  const link = useLink(b.platform);
  return (
    <aside className="sidebar">
      <div className="sb-brand">
        <span className="emblem"><Icon name="bridge" size={20} /></span>
        <div className="wordmark">
          <strong>PC Bridge</strong>
          <div className="tags">
            <span>v{b.version}</span>
            <span className="plat">{b.platform === "windows" ? "WINDOWS" : "MAC"}</span>
          </div>
        </div>
      </div>
      <nav className="nav" role="tablist">
        <div className="nav-label">CONTROL</div>
        {TABS.map(([id, label, icon]) => (
          <button key={id} role="tab" title={label} aria-selected={tab === id} className={tab === id ? "tab active" : "tab"} onClick={() => setTab(id)}>
            <Icon name={icon} size={17} />
            <span className="label">{label}</span>
            {id === "status" && link && <Dot color={link.color} />}
          </button>
        ))}
      </nav>
      <div className="sb-foot">
        <div className="kicker"><Icon name="lock" size={12} /> TAILSCALE PRIVATE LINK</div>
        <div className="host">
          <Dot color={link?.color ?? "yellow"} />
          <span title={link?.host}>{link?.host ?? "Checking…"}</span>
        </div>
        <div className="state">{link?.state ?? "Looking for the connection"}</div>
      </div>
    </aside>
  );
}

interface Boot {
  platform: Platform;
  version: string;
  setupComplete: boolean;
  wizardStep: number;
}

/** Load where the user is: platform, and whether setup finished (or which step to resume). */
async function boot(): Promise<Boot> {
  const info = await api.appInfo();
  if (info.platform === "windows") {
    const ws = await api.winGetState();
    return { platform: info.platform, version: info.version, setupComplete: ws.setup_complete, wizardStep: ws.wizard_step };
  }
  const cfg = await api.getConfig();
  return { platform: info.platform, version: info.version, setupComplete: cfg.setup_complete, wizardStep: cfg.wizard_step };
}

/** Persist wizard progress so the app resumes at the same step after closing. */
async function saveProgress(platform: Platform, step: number, complete?: boolean) {
  if (platform === "windows") {
    const ws = await api.winGetState();
    await api.winSaveState({ ...ws, wizard_step: step, setup_complete: complete ?? ws.setup_complete });
  } else {
    const cfg = await api.getConfig();
    await api.saveConfig({ ...cfg, wizard_step: step, setup_complete: complete ?? cfg.setup_complete });
  }
}

export function App() {
  const [b, setB] = useState<Boot | null>(null);
  const [err, setErr] = useState("");
  const [tab, setTab] = useState<Tab>("status");
  const [wizard, setWizard] = useState(false);

  useEffect(() => {
    boot()
      .then((x) => {
        setB(x);
        setWizard(!x.setupComplete);
      })
      .catch((e) => setErr(errText(e)));
  }, []);

  if (err) {
    return (
      <div className="center-screen">
        <h2>PC Bridge couldn't start</h2>
        <p className="error">{err}</p>
      </div>
    );
  }
  if (!b) return <div className="center-screen"><span className="spinner" /></div>;

  if (wizard) {
    const steps = b.platform === "windows" ? winSteps : macSteps;
    return (
      <Wizard
        appName={b.platform === "windows" ? "PC Bridge for Windows" : "PC Bridge"}
        steps={steps}
        start={b.setupComplete ? 0 : b.wizardStep}
        onStep={(i) => void saveProgress(b.platform, i)}
        onFinish={async () => {
          await saveProgress(b.platform, 0, true);
          setB({ ...b, setupComplete: true, wizardStep: 0 });
          setWizard(false);
          setTab("status");
        }}
      />
    );
  }

  const title = TABS.find(([id]) => id === tab)?.[1];
  return (
    <div className="shell">
      <Sidebar b={b} tab={tab} setTab={setTab} />
      <main className="page">
        <div className="page-inner">
          <header className="page-head">
            <div>
              <div className="kicker">{b.platform === "windows" ? "THIS PC · THE MUSCLE" : "THIS MAC · THE BRAIN"}</div>
              <h1>{title}</h1>
            </div>
          </header>
          {tab === "status" && <Status platform={b.platform} />}
          {tab === "jobs" && <Jobs platform={b.platform} />}
          {tab === "settings" && <Settings platform={b.platform} onRerunSetup={() => setWizard(true)} />}
          {tab === "help" && <Help platform={b.platform} />}
        </div>
      </main>
    </div>
  );
}
