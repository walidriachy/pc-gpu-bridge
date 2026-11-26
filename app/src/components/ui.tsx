import { useRef, useState, type PointerEvent, type ReactNode } from "react";
import { errText } from "../lib/api";
import { gb, usedFraction } from "../lib/format";

/* ------------------------------------------------------------------ Icons
 * Hairline monochrome icons (1.75px strokes in currentColor), drawn inline so the app needs no
 * icon font or network. 24×24 grid.
 */
const PATHS = {
  bridge: "M3 17h18M5 17V9m14 8V9M5 9c2.5 3 11.5 3 14 0M9 17v-5.2M12 17v-4.6M15 17v-5.2M13.2 3.5 10.6 7h2.8l-2.6 3.5",
  status: "M3 12h4l2.5-6 5 12 2.5-6h4",
  jobs: "M4 7.5 12 4l8 3.5-8 3.5-8-3.5ZM4 12l8 3.5 8-3.5M4 16.5 12 20l8-3.5",
  settings: "M4 6h9m4 0h3M4 12h3m4 0h9M4 18h11m4 0h1M15 4v4M9 10v4M17 16v4",
  help: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18ZM9.6 9.3a2.5 2.5 0 0 1 4.9.7c0 1.7-2.5 2.2-2.5 3.7M12 16.8v.2",
  check: "m5 12.5 4.5 4.5L19 7.5",
  x: "M6.5 6.5l11 11m0-11-11 11",
  clock: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18ZM12 7.5V12l3 2",
  stop: "M7 7h10v10H7z",
  alert: "M12 4 2.8 19.5h18.4L12 4ZM12 10v4.2M12 17v.2",
  folder: "M3.5 7.5A1.5 1.5 0 0 1 5 6h4.2l1.8 2H19a1.5 1.5 0 0 1 1.5 1.5v8A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5v-10Z",
  laptop: "M5 6.5A1.5 1.5 0 0 1 6.5 5h11A1.5 1.5 0 0 1 19 6.5V15H5V6.5ZM2.5 18.5h19l-1.5-3.5H4l-1.5 3.5Z",
  monitor: "M3.5 5.5A1.5 1.5 0 0 1 5 4h14a1.5 1.5 0 0 1 1.5 1.5v9A1.5 1.5 0 0 1 19 16H5a1.5 1.5 0 0 1-1.5-1.5v-9ZM9 20h6M12 16v4",
  lock: "M6.5 11h11v9h-11zM8.5 11V8a3.5 3.5 0 0 1 7 0v3",
  copy: "M9 9h10.5v10.5H9zM15 9V4.5H4.5V15H9",
  left: "m14.5 6-6 6 6 6",
  right: "m9.5 6 6 6-6 6",
  refresh: "M19.5 11A7.5 7.5 0 0 0 6 6.6L4.5 8M4.5 4v4h4M4.5 13A7.5 7.5 0 0 0 18 17.4l1.5-1.4m0 4v-4h-4",
  open: "M14 4.5h5.5V10M19.5 4.5 11 13M17 14v4.5a1 1 0 0 1-1 1H5.5a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1H10",
  power: "M12 3.5v8M7 6.3a7.5 7.5 0 1 0 10 0",
  plus: "M12 5v14M5 12h14",
  chip: "M7 7h10v10H7zM10 3.5V7m4-3.5V7m-4 10v3.5m4-3.5v3.5M3.5 10H7m-3.5 4H7m10-4h3.5M17 14h3.5",
  memory: "M3.5 8h17v8h-17zM7 8v8m4-8v8m4-8v8M6 16v3m12-3v3",
  link: "M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1",
  spark: "M12 3.5 13.8 10l6.7 2-6.7 2L12 20.5 10.2 14 3.5 12l6.7-2L12 3.5Z",
  pulse: "M12 12m-3 0a3 3 0 1 0 6 0 3 3 0 1 0-6 0",
  box: "M4 7.5 12 4l8 3.5v9L12 20l-8-3.5v-9ZM4 7.5 12 11l8-3.5M12 11v9",
  terminal: "M3.5 5.5h17v13h-17zM7 10l3 2.5L7 15m5 0h5",
  shield: "M12 3.5 19 6v5.5c0 4.4-3 7.6-7 9-4-1.4-7-4.6-7-9V6l7-2.5Z",
} as const;
export type IconName = keyof typeof PATHS;

export function Icon({ name, size = 16, className }: { name: IconName; size?: number; className?: string }) {
  return (
    <svg
      className={className ? `icon ${className}` : "icon"}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.75}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d={PATHS[name]} />
    </svg>
  );
}

/* ------------------------------------------------------------------ Pills & toggles */
export type Tone = "sapphire" | "cyan" | "teal" | "amber" | "rose" | "neutral";

/** A small status chip: "Done", "Running", "Installed"… */
export function Pill({ tone = "neutral", icon, spin, children, title }: { tone?: Tone; icon?: IconName; spin?: boolean; children: ReactNode; title?: string }) {
  return (
    <span className={`pill tone-${tone}`} title={title}>
      {spin ? <span className="spinner tiny" /> : icon && <Icon name={icon} size={12} />}
      {children}
    </span>
  );
}

/** A macOS-style on/off switch. The real checkbox stays in the page for keyboard and screen readers. */
export function Toggle({ checked, onChange, label }: { checked: boolean; onChange: (v: boolean) => void; label?: ReactNode }) {
  return (
    <label className="switch">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span className="toggle" aria-hidden />
      {label && <span>{label}</span>}
    </label>
  );
}

/** A command shown with a Copy button and a plain-English explanation. */
export function CopyBox({ command, explain }: { command: string; explain?: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="copybox">
      <div className="copybox-row">
        <span className="prompt" aria-hidden>$</span>
        <code>{command}</code>
        <button
          className={copied ? "btn small copied" : "btn small"}
          onClick={async () => {
            await navigator.clipboard.writeText(command);
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          }}
        >
          <Icon name={copied ? "check" : "copy"} size={13} />
          {copied ? "Copied" : "Copy"}
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
        <span className="spinner" /> <span>{state.message || "Working on it…"}</span>
      </div>
    );
  if (state.status === "ok")
    return (
      <div className="check ok">
        <span className="check-icon"><Icon name="check" size={13} /></span>
        <span>{state.message || okText || "All good"}</span>
      </div>
    );
  return (
    <div className="check fail">
      <span className="check-icon"><Icon name="x" size={13} /></span>
      <div>
        <div>{state.message || "Something went wrong"}</div>
        {state.fix && (
          <div className="fix">
            <strong>What to do:</strong> {state.fix}
          </div>
        )}
      </div>
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

/* ------------------------------------------------------------------ Meters */
/** A slim gradient track. `tone` picks the gradient; `value` is 0–1. */
export function Track({ value, tone = "sapphire" }: { value: number; tone?: "sapphire" | "teal" | "amber" | "rose" }) {
  const pct = Math.max(0, Math.min(1, value || 0)) * 100;
  return (
    <div className="membar-track">
      <div className={`membar-fill fill-${tone}`} style={{ width: `${pct}%` }} />
    </div>
  );
}

export function MemoryBar({ label, icon, total, available, hint, badge }: { label: string; icon?: IconName; total?: number; available?: number; hint?: string; badge?: ReactNode }) {
  const f = usedFraction(total, available);
  const tone = f > 0.9 ? "rose" : f > 0.75 ? "amber" : "teal";
  return (
    <div className="membar">
      <div className="membar-head">
        <strong>{icon && <Icon name={icon} size={14} />}{label}</strong>
        {badge}
        <span className="num">
          {total ? (
            <>
              <b>{gb(available)}</b> free / {gb(total, 0)}
            </>
          ) : (
            "not available"
          )}
        </span>
      </div>
      <Track value={f} tone={tone} />
      {hint && <div className="muted small">{hint}</div>}
    </div>
  );
}

/** Mac vs PC free RAM side by side. */
export function MemoryCompare({ mac, pc, pressure }: { mac?: any; pc?: any; pressure?: string }) {
  const pressureText =
    pressure === "critical"
      ? "Your Mac is very short on memory, so heavy jobs go to the PC automatically."
      : pressure === "warn"
        ? "Your Mac's memory is getting full, so PC Bridge prefers the PC for heavy jobs."
        : "Your Mac has enough memory right now.";
  const badge = pressure ? (
    pressure === "critical" ? <Pill tone="rose">HIGH</Pill> : pressure === "warn" ? <Pill tone="amber">ELEVATED</Pill> : <Pill tone="teal">NORMAL</Pill>
  ) : undefined;
  return (
    <div className="memcompare">
      {mac !== undefined && (
        <MemoryBar label="This Mac" icon="laptop" total={mac?.total_bytes} available={mac?.available_bytes} hint={pressure ? pressureText : undefined} badge={badge} />
      )}
      <MemoryBar label="Your PC" icon="monitor" total={pc?.total_bytes} available={pc?.available_bytes} hint="Only jobs sent to the PC use this memory." />
    </div>
  );
}

/* ------------------------------------------------------------------ Cards */
/** Follow the pointer with a faint highlight (CSS variables only, no re-render). */
function sheen(e: PointerEvent<HTMLElement>) {
  const r = e.currentTarget.getBoundingClientRect();
  e.currentTarget.style.setProperty("--mx", `${e.clientX - r.left}px`);
  e.currentTarget.style.setProperty("--my", `${e.clientY - r.top}px`);
}

export function Card({
  title,
  kicker,
  icon,
  children,
  actions,
  className,
  primary,
}: {
  title?: string;
  kicker?: string;
  icon?: IconName;
  children: ReactNode;
  actions?: ReactNode;
  className?: string;
  primary?: boolean;
}) {
  const ref = useRef<HTMLElement>(null);
  return (
    <section ref={ref} className={["card", primary && "sheen", className].filter(Boolean).join(" ")} onPointerMove={primary ? sheen : undefined}>
      {(title || actions || kicker) && (
        <div className="card-head">
          <div className="card-title">
            {kicker && <div className="kicker">{kicker}</div>}
            {title && (
              <h3>
                {icon && <Icon name={icon} size={16} />}
                {title}
              </h3>
            )}
          </div>
          {actions && <div className="card-actions">{actions}</div>}
        </div>
      )}
      {children}
    </section>
  );
}

export function Dot({ color }: { color: "green" | "yellow" | "red" }) {
  return <span className={`dot ${color}`} />;
}
