import { useState, type ReactNode } from "react";
import { errText } from "../lib/api";
import { gb, usedFraction } from "../lib/format";

/** A command shown with a Copy button and a plain-English explanation. */
export function CopyBox({ command, explain }: { command: string; explain?: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="copybox">
      <div className="copybox-row">
        <code>{command}</code>
        <button
          className="btn small"
          onClick={async () => {
            await navigator.clipboard.writeText(command);
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          }}
        >
          {copied ? "Copied ✓" : "Copy"}
        </button>
      </div>
      {explain && <p className="muted small">{explain}</p>}
    </div>
  );
}

export type CheckState = { status: "idle" | "running" | "ok" | "fail"; message?: string; fix?: string };

export function CheckResult({ state, okText }: { state: CheckState; okText?: string }) {
  if (state.status === "idle") return null;
  if (state.status === "running")
    return (
      <div className="check running">
        <span className="spinner" /> {state.message || "Working on it…"}
      </div>
    );
  if (state.status === "ok")
    return (
      <div className="check ok">
        ✅ {state.message || okText || "All good"}
      </div>
    );
  return (
    <div className="check fail">
      <div>❌ {state.message || "Something went wrong"}</div>
      {state.fix && (
        <div className="fix">
          <strong>What to do:</strong> {state.fix}
        </div>
      )}
    </div>
  );
}

/** Run an async action and track its check state. */
export function useAction<T>() {
  const [state, setState] = useState<CheckState>({ status: "idle" });
  const run = async (fn: () => Promise<T>, running = "Working on it…"): Promise<T | undefined> => {
    setState({ status: "running", message: running });
    try {
      const r = await fn();
      setState({ status: "ok" });
      return r;
    } catch (e) {
      setState({ status: "fail", message: errText(e) });
      return undefined;
    }
  };
  return { state, setState, run };
}

export function MemoryBar({ label, total, available, hint }: { label: string; total?: number; available?: number; hint?: string }) {
  const f = usedFraction(total, available);
  const tone = f > 0.9 ? "bad" : f > 0.75 ? "warn" : "good";
  return (
    <div className="membar">
      <div className="membar-head">
        <strong>{label}</strong>
        <span>
          {total ? `${gb(available)} free of ${gb(total, 0)}` : "not available"}
        </span>
      </div>
      <div className="membar-track">
        <div className={`membar-fill ${tone}`} style={{ width: `${Math.round(f * 100)}%` }} />
      </div>
      {hint && <div className="muted small">{hint}</div>}
    </div>
  );
}

/** Mac vs PC free RAM side by side. */
export function MemoryCompare({ mac, pc, pressure }: { mac?: any; pc?: any; pressure?: string }) {
  const pressureText =
    pressure === "critical"
      ? "Your Mac is very short on memory — heavy jobs go to the PC automatically."
      : pressure === "warn"
        ? "Your Mac's memory is getting full — PC Bridge prefers the PC for heavy jobs."
        : "Your Mac has enough memory right now.";
  return (
    <div className="memcompare">
      {mac !== undefined && <MemoryBar label="This Mac" total={mac?.total_bytes} available={mac?.available_bytes} hint={pressure ? pressureText : undefined} />}
      <MemoryBar label="Your PC" total={pc?.total_bytes} available={pc?.available_bytes} hint="Only jobs sent to the PC use this memory." />
    </div>
  );
}

export function Card({ title, children, actions }: { title?: string; children: ReactNode; actions?: ReactNode }) {
  return (
    <section className="card">
      {(title || actions) && (
        <div className="card-head">
          {title && <h3>{title}</h3>}
          {actions}
        </div>
      )}
      {children}
    </section>
  );
}

export function Dot({ color }: { color: "green" | "yellow" | "red" }) {
  return <span className={`dot ${color}`} />;
}
