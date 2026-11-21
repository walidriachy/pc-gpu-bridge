import { useEffect, useState } from "react";
import { api, errText, type Platform } from "./lib/api";
import { Wizard } from "./components/Wizard";
import { macSteps } from "./steps/MacSteps";
import { winSteps } from "./steps/WinSteps";
import { Status } from "./pages/Status";
import { Jobs } from "./pages/Jobs";
import { Settings } from "./pages/Settings";
import { Help } from "./pages/Help";

type Tab = "status" | "jobs" | "settings" | "help";
const TABS: [Tab, string][] = [
  ["status", "Status"],
  ["jobs", "Jobs"],
  ["settings", "Settings"],
  ["help", "Help"],
];

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

  return (
    <div className="shell">
      <nav className="topnav">
        <div className="brand">
          <span className="logo">⇄</span> PC Bridge
        </div>
        <div className="tabs" role="tablist">
          {TABS.map(([id, label]) => (
            <button key={id} role="tab" aria-selected={tab === id} className={tab === id ? "tab active" : "tab"} onClick={() => setTab(id)}>
              {label}
            </button>
          ))}
        </div>
        <span className="muted small">v{b.version}</span>
      </nav>
      <main className="page">
        {tab === "status" && <Status platform={b.platform} />}
        {tab === "jobs" && <Jobs platform={b.platform} />}
        {tab === "settings" && <Settings platform={b.platform} onRerunSetup={() => setWizard(true)} />}
        {tab === "help" && <Help platform={b.platform} />}
      </main>
    </div>
  );
}
