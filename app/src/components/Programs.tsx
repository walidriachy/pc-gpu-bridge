import { useEffect, useState } from "react";
import { api, errText, type PcProgram, type ProgramsInfo } from "../lib/api";
import { driveName, hasRoom, sizeText, spaceNeededGb } from "../lib/format";
import { Card, CheckResult, Icon, Pill, type CheckState } from "./ui";

/** Below this much free space we warn, even before any install is tried. */
const LOW_SPACE_GB = 10;

export const WHERE_PROGRAMS_GO =
  "Programs install in their normal Windows place (like C:\\Program Files), not in the PC Bridge folder or your shared folders.";

export const YES_PROMPT_NOTE = "If Windows asks “Do you want to allow this app to make changes?”, click Yes.";

export type InstallState = CheckState & { log?: string; restart?: boolean };

/** The helper's program list (with free space), reloaded after each install. */
export function usePrograms() {
  const [info, setInfo] = useState<ProgramsInfo | null>(null);
  const [error, setError] = useState("");
  const reload = () =>
    api
      .winPrograms()
      .then((i) => {
        setInfo(i);
        setError("");
        return i;
      })
      .catch((e) => {
        setError(errText(e));
        return null;
      });
  useEffect(() => {
    reload();
  }, []);
  return { info, error, reload };
}

/** Install programs one at a time, with a friendly message for each. */
export function useProgramInstall(onFinished?: () => void) {
  const [states, setStates] = useState<Record<string, InstallState>>({});
  const [busy, setBusy] = useState("");
  const set = (key: string, s: InstallState) => setStates((all) => ({ ...all, [key]: s }));

  const install = async (p: PcProgram, info: ProgramsInfo | null): Promise<boolean> => {
    if (busy) return false;
    if (info && !hasRoom(info.free_gb, p.size_gb)) {
      set(p.key, {
        status: "fail",
        message: `Not enough free space for ${p.name}: ${info.free_gb} GB free on the ${driveName(info.drive)} drive, about ${spaceNeededGb(p.size_gb)} GB needed.`,
        fix: "Free up some space (Windows Settings → System → Storage → Cleanup recommendations), then try again.",
      });
      return false;
    }
    setBusy(p.key);
    set(p.key, { status: "running", message: `Installing ${p.name}… this can take a few minutes. ${YES_PROMPT_NOTE}` });
    let ok = false;
    try {
      const r = await api.winInstallProgram(p.key);
      ok = r.ok;
      set(
        p.key,
        r.ok && r.restart_needed
          ? { status: "ok", restart: true, message: `${p.name} is installed. Restart the PC to finish the install, then it's ready to use.` }
          : r.ok
          ? { status: "ok", message: `${p.name} is installed and ready to use.` }
          : {
              status: "fail",
              message: `${p.name} didn't finish installing.`,
              fix: "Check the internet connection and try again. If Windows asked for permission, click Yes next time.",
              log: r.log_tail,
            },
      );
    } catch (e) {
      set(p.key, { status: "fail", message: errText(e) });
    } finally {
      setBusy("");
      onFinished?.();
    }
    return ok;
  };

  return { states, busy, install };
}

/** The result of an install, with the installer's last lines tucked away for anyone curious. */
export function InstallStatus({ state }: { state?: InstallState }) {
  if (!state) return null;
  return (
    <>
      <CheckResult state={state} />
      {state.log && (
        <details>
          <summary className="muted small">Show what the installer said</summary>
          <pre className="small">{state.log}</pre>
        </details>
      )}
    </>
  );
}

function ProgramRow({ p, info, busy, state, onInstall }: { p: PcProgram; info: ProgramsInfo; busy: string; state?: InstallState; onInstall: () => void }) {
  const room = hasRoom(info.free_gb, p.size_gb);
  return (
    <li className={p.installed ? "program-tile installed" : "program-tile"}>
      <div className="program-top">
        <strong><Icon name="box" size={16} />{p.name}</strong>
        {p.installed && <Pill tone="teal" icon="check">Installed</Pill>}
      </div>
      <p className="program-why">{p.why}</p>
      <div className="program-info">
        {state?.restart ? (
          <InstallStatus state={state} />
        ) : state?.status === "ok" ? (
          !p.installed && (
            <CheckResult
              state={{ status: "ok", message: `${p.name} installed, but the helper can't see it yet. Click “Restart the helper” below, then “Check again”.` }}
            />
          )
        ) : (
          <InstallStatus state={state} />
        )}
      </div>
      <div className="program-foot">
        <span className="muted">
          {sizeText(p.size_gb)}
          {!p.installed && !room && ` · needs about ${spaceNeededGb(p.size_gb)} GB free`}
        </span>
        {!p.installed && (
          <button className="btn small" disabled={!!busy || !room} onClick={onInstall}>
            {busy === p.key ? <><span className="spinner tiny" /> Installing…</> : <><Icon name="plus" size={13} /> Install</>}
          </button>
        )}
      </div>
    </li>
  );
}

/** Settings → Programs: what's on the PC, free space, and one-click installs. */
export function ProgramsCard() {
  const { info, error, reload } = usePrograms();
  const { states, busy, install } = useProgramInstall(() => {
    reload();
  });
  const drive = info ? driveName(info.drive) : "C:";
  const low = info && info.free_gb >= 0 && info.free_gb < LOW_SPACE_GB;
  return (
    <Card title="Programs" kicker="TOOLCHAIN" icon="box" actions={<button className="btn small ghost" disabled={!!busy} onClick={() => reload()}><Icon name="refresh" size={13} /> Check again</button>}>
      <p>Programs your Mac can use on this PC. Click “Install” to add one; Claude can then send that kind of work here.</p>
      {error && <CheckResult state={{ status: "fail", message: error }} />}
      {info && (
        <>
          <p className="muted small">
            Free space on the {drive} drive: <strong>{info.free_gb >= 0 ? `${info.free_gb} GB` : "unknown"}</strong>
          </p>
          {low && (
            <p className="advice">
              The {drive} drive is almost full. Big programs like PyTorch may not fit. Free up space in Windows Settings → System → Storage first.
            </p>
          )}
          <ul className="program-grid">
            {info.programs.map((p) => (
              <ProgramRow key={p.key} p={p} info={info} busy={busy} state={states[p.key]} onInstall={() => install(p, info)} />
            ))}
          </ul>
          <p className="muted small">Installs can take a few minutes. {YES_PROMPT_NOTE}</p>
          <p className="muted small">{WHERE_PROGRAMS_GO}</p>
          <p className="muted small">Still shows as missing after installing? Click “Restart the helper” below.</p>
        </>
      )}
    </Card>
  );
}
