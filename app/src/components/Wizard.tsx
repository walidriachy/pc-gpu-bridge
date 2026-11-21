import { useCallback, useState, type ReactNode } from "react";

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
      <header className="wizard-head">
        <div className="brand">{appName} setup</div>
        <div className="progress-label">
          Step {index + 1} of {steps.length}
        </div>
        <div className="progress">
          <div className="progress-fill" style={{ width: `${((index + 1) / steps.length) * 100}%` }} />
        </div>
        <ol className="step-dots">
          {steps.map((s, i) => (
            <li key={s.id} className={i === index ? "current" : i < index ? "past" : ""} title={s.title} />
          ))}
        </ol>
      </header>

      <main className="wizard-body">
        <h1>{step.title}</h1>
        <div className="whatwhy">
          <p>
            <span className="tag">What</span> {step.what}
          </p>
          <p>
            <span className="tag why">Why</span> {step.why}
          </p>
        </div>
        <div className="step-content">
          {step.body({ setDone, done, next })}
        </div>
      </main>

      <footer className="wizard-foot">
        <button className="btn ghost" disabled={index === 0} onClick={() => go(index - 1)}>
          ← Back
        </button>
        <span className="muted small">You can close the app any time — it remembers where you were.</span>
        <div className="row">
          {step.optional && !done && (
            <button className="btn ghost" onClick={next}>
              Skip for now
            </button>
          )}
          <button className="btn primary" disabled={!done && !step.optional} onClick={next}>
            {last ? "Finish" : "Next →"}
          </button>
        </div>
      </footer>
    </div>
  );
}
