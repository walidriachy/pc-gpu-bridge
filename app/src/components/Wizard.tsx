import { useCallback, useState, type ReactNode } from "react";
import { Icon } from "./ui";

/**
 * Every wizard step has: what we're doing (one sentence), why it's needed (plain words),
 * a body with a "Do it for me" button or numbered instructions, and an automatic check.
 * `done` controls whether "Next" is enabled; `optional` steps can be skipped.
 */
export interface StepDef {
  id: string;
  title: string;
  what: string;
  why: string;
  optional?: boolean;
  body: (ctx: StepCtx) => ReactNode;
}

export interface StepCtx {
  setDone: (done: boolean) => void;
  done: boolean;
  next: () => void;
}

export function Wizard({
  steps,
  start,
  onStep,
  onFinish,
  appName,
}: {
  steps: StepDef[];
  start: number;
  onStep: (i: number) => void;
  onFinish: () => void;
  appName: string;
}) {
  const [index, setIndex] = useState(Math.min(start, steps.length - 1));
  const [doneMap, setDoneMap] = useState<Record<string, boolean>>({});
  const step = steps[index];
  const done = !!doneMap[step.id];
  const last = index === steps.length - 1;
  const stepId = step.id;
  // Stable per step, and a no-op when unchanged, so steps can call it from effects safely.
  const setDone = useCallback(
    (d: boolean) => setDoneMap((m) => (m[stepId] === d ? m : { ...m, [stepId]: d })),
    [stepId],
  );

  const go = (i: number) => {
    setIndex(i);
    onStep(i);
  };
  const next = () => (last ? onFinish() : go(index + 1));

  return (
    <div className="wizard">
      <div className="card wizard-card">
        <header className="wizard-head">
          <div className="wizard-top">
            <div className="sb-brand">
              <span className="emblem"><Icon name="bridge" size={20} /></span>
              <div className="wordmark">
                <strong>{appName}</strong>
                <span className="kicker">SETUP</span>
              </div>
            </div>
            <div className="progress-label">
              Step <b>{String(index + 1).padStart(2, "0")}</b> / {String(steps.length).padStart(2, "0")}
            </div>
          </div>
          <ol className="steps-bar">
            {steps.map((s, i) => (
              <li key={s.id} className={i === index ? "current" : i < index ? "past" : ""} title={s.title}>
                <span className="n">{i < index ? <Icon name="check" size={13} /> : i + 1}</span>
                {i < steps.length - 1 && <span className="seg" />}
              </li>
            ))}
          </ol>
        </header>

        <main className="wizard-body">
          <h1>{step.title}</h1>
          <div className="whatwhy">
            <p>
              <span className="tag">What</span> <span>{step.what}</span>
            </p>
            <p>
              <span className="tag why">Why</span> <span>{step.why}</span>
            </p>
          </div>
          <div className="step-content">{step.body({ setDone, done, next })}</div>
        </main>

        <footer className="wizard-foot">
          <button className="btn ghost" disabled={index === 0} onClick={() => go(index - 1)}>
            <Icon name="left" size={15} /> Back
          </button>
          <span className="muted small">You can close the app any time. It remembers where you were.</span>
          <div className="row">
            {step.optional && !done && (
              <button className="btn ghost" onClick={next}>
                Skip for now
              </button>
            )}
            <button className="btn primary" disabled={!done && !step.optional} onClick={next}>
              {last ? "Finish" : "Continue"} <Icon name={last ? "check" : "right"} size={15} />
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}
