import { NewspaperClipping } from "@phosphor-icons/react";
import { Md } from "./Markdown";
import { TurnState } from "./Panels";
import { READING_LABEL, STATUS_LABEL, type Conclusion } from "../types";
import type { CouncilState } from "../useCouncil";

export function endLabel(state: CouncilState): string {
  if (!state.verdict) return "";
  if (state.verdict.adjournedBy) return "adjourned";
  switch (state.verdict.endedHow) {
    case "settled":
      return "settled early";
    case "calledSettled":
      return "called settled";
    case "stopped":
      return "stopped";
    case "complete":
      return "completed";
    default:
      return "ended";
  }
}

export function endTitle(state: CouncilState): string {
  if (!state.verdict) return "";
  const total = state.maxRounds;
  const done = state.verdict.rounds;
  if (state.verdict.adjournedBy)
    return `closed by ${councilLabel(state, state.verdict.adjournedBy)} after round ${done} of ${total}`;
  switch (state.verdict.endedHow) {
    case "settled":
      return `every point decided after round ${done} of ${total} — stopped early`;
    case "calledSettled":
      return (
        state.verdict.settledReason ??
        `the Summarizer read the record as converged after round ${done} of ${total}`
      );
    case "stopped":
      return `stopped by the operator after round ${done} of ${total}`;
    case "complete":
      return `all ${total} rounds spoken`;
    default:
      return "";
  }
}

export function councilLabel(state: CouncilState, id: string): string {
  return state.delegates.find((d) => d.id === id)?.label ?? id;
}

/**
 * The bottom line, at the bottom of the page.
 *
 * Placed above the end result because it is the thing the reader came for. A
 * stalemate is stated plainly rather than dressed up: when the council did not
 * converge, the section says so and shows which way the evidence leaned, since
 * "no agreement" is not the same as "nothing happened".
 */
export function AnswerSection({ conclusion }: { conclusion: Conclusion }) {
  const { agreed, answer, lean, tally } = conclusion;
  return (
    <section
      className={`mb-3 border-l-2 pb-3 pl-3 ${
        agreed ? "border-ok" : "border-warn"
      }`}
      aria-label="the council's answer"
    >
      <h3 className="section-title mb-1.5">Answer</h3>
      {agreed ? (
        <>
          <Md text={answer || "The council settled the motion, but left the answer unstated."} />
          <p className="mt-1.5 font-mono text-[10.5px] text-ink-3">{tally}</p>
        </>
      ) : (
        <>
          <p className="text-[13px] leading-relaxed text-ink">
            The council could not agree on an answer.
          </p>
          {lean && (
            <p className="mt-1.5 text-[12.5px] leading-relaxed text-ink-2">
              It leaned <strong className="font-semibold text-ink">{lean}</strong>.
            </p>
          )}
          {tally && (
            <p className="mt-1.5 font-mono text-[10.5px] text-ink-3">{tally}</p>
          )}
        </>
      )}
    </section>
  );
}

/**
 * The record: the answer, the end result, and what the board made of every point.
 *
 * One component, two callers. The live chamber renders it while the debate
 * closes and the reader renders it weeks later; a second copy of this markup
 * would be a second set of drift, and the record is the one thing that has to
 * read the same both times.
 */
export function RecordPanel({ state }: { state: CouncilState }) {
  return (
  <div className="p-4">
    {state.verdict ? (
      <p className="mb-3 border-b border-line pb-3 text-[12px] text-ink-3">
        {endTitle(state) || endLabel(state)}
      </p>
    ) : null}
    {!state.verdict ? (
      state.live || state.running || state.speeches.length > 0 ? (
        <LiveRecord state={state} />
      ) : (
        <div className="py-8 text-center">
          <div className="mx-auto mb-2 flex size-9 items-center justify-center rounded-xl bg-surface-2 text-ink-3">
            <NewspaperClipping size={18} />
          </div>
          <p className="text-[12.5px] font-medium text-ink-2">Nothing to report yet.</p>
          <p className="mx-auto mt-1 max-w-[28ch] text-[11.5px] leading-relaxed text-ink-3">
            The record fills in when the debate ends.
          </p>
        </div>
      )
    ) : (
      <section>
        {state.verdict.conclusion && (
          <AnswerSection conclusion={state.verdict.conclusion} />
        )}
        {state.verdict.finalSummary ? (
          <div className="mb-3 border-b border-line pb-3">
            <h3 className="section-title mb-1.5">End result</h3>
            <Md text={state.verdict.finalSummary} compact />
          </div>
        ) : null}
        <p className="text-[13px] leading-relaxed text-ink-2">
          {state.verdict.summary}
        </p>
        {state.verdict.crux && (
          <p className="mt-2 text-[12px] text-ink-3">
            <strong className="text-ink-2">Crux</strong> — P
            {state.verdict.crux.id}: {state.verdict.crux.text}
          </p>
        )}
        <ul className="mt-3 flex flex-col">
          {state.verdict.carried.map((p) => (
            <li
              key={p.id}
              className="border-t border-line py-2 text-[12px] text-ink-2"
            >
              <b className="font-mono text-ok">P{p.id}</b> carried +{p.score} —{" "}
              {p.text}
            </li>
          ))}
          {state.verdict.fell.map((p) => (
            <li
              key={p.id}
              className="border-t border-line py-2 text-[12px] text-ink-2"
            >
              <b className="font-mono text-bad">P{p.id}</b> fell {p.score} — {p.text}
            </li>
          ))}
          {state.verdict.stalemate.map((p) => (
            <li
              key={p.id}
              className="border-t border-line py-2 text-[12px] text-ink-2"
            >
              <b className="font-mono text-warn">P{p.id}</b> stalemate — {p.text}
              {p.ruling && (
                <span className="text-ink-3"> (ruled {READING_LABEL[p.ruling]})</span>
              )}
            </li>
          ))}
        </ul>
      </section>
    )}
  </div>
  );
}

/**
 * The board's pulse while there is no verdict yet: how many points stand
 * where. The full verdict replaces this the moment the debate closes.
 */
function BoardPulse({ state }: { state: CouncilState }) {
  if (!state.points.length) return null;
  const of = (s: string) => state.points.filter((p) => state.tallies[p.id]?.status === s).length;
  return (
    <p className="mt-2 text-[11.5px] tabular-nums text-ink-3">
      <span className="text-ok">{of("carried")} carried</span>
      {" · "}
      <span className="text-bad">{of("fell")} fell</span>
      {" · "}
      <span className="text-warn">{of("stalemate")} tied</span>
      {" · "}
      {state.points.length} points
    </p>
  );
}

/**
 * The Record tab before there is a record: who holds the floor right now,
 * what they have done with it, and what the board says so far. The transcript
 * shows every word; this is the glanceable version.
 */
function LiveRecord({ state }: { state: CouncilState }) {
  const live = state.live;
  if (!live) {
    return (
      <div className="py-8 text-center">
        <p className="text-[12.5px] font-medium text-ink-2">
          {state.running ? `Between turns — round ${Math.max(1, state.round)}.` : "Stopped before a verdict."}
        </p>
        <p className="mx-auto mt-1 max-w-[28ch] text-[11.5px] leading-relaxed text-ink-3">
          {state.running
            ? "The next seat is taking the floor."
            : "The board below is everything decided so far."}
        </p>
        <BoardPulse state={state} />
      </div>
    );
  }

  const acts = state.acts[live.delegate] ?? {};
  const tools = Object.entries(acts).sort((a, b) => b[1] - a[1]);
  const raised = state.raised[live.delegate] ?? 0;
  const conceded = state.conceded[live.delegate] ?? 0;
  const made = state.points.filter((p) => p.raisedBy === live.delegate);
  const thinking = live.reasoning.trim();
  const excerpt = thinking.length > 300 ? `${thinking.slice(0, 300).trim()}…` : thinking;

  return (
    <div>
      <h3 className="section-title mb-1.5">Now speaking</h3>
      <div className="mb-1.5 flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="size-2.5 shrink-0 rounded-full" style={{ background: live.color }} />
        <strong className="text-[13.5px] font-semibold tracking-tight text-ink">
          {live.label}
        </strong>
        {live.model && (
          <span className="max-w-44 truncate font-mono text-[10.5px] text-ink-3" title={live.model}>
            {live.model}
          </span>
        )}
        <span className="rounded-full bg-surface-2 px-2 py-px text-[10.5px] text-ink-2 tabular-nums">
          R{live.round}
        </span>
        <TurnState live={live} />
      </div>

      {excerpt && (
        <div className="mt-1.5 border-l-2 border-line-2 pl-2.5">
          <div className="text-[9px] font-semibold tracking-[0.08em] text-ink-3 uppercase">
            thinking · full text in the transcript
          </div>
          <p className="mt-0.5 text-[12px] leading-relaxed text-ink-2 [overflow-wrap:anywhere]">
            {excerpt}
          </p>
        </div>
      )}

      {(tools.length > 0 || raised > 0 || conceded > 0) && (
        <div className="mt-2.5 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[11px] text-ink-3">
          <span>
            <b className="font-semibold text-ink tabular-nums">{raised}</b> raised
          </span>
          {tools.map(([tool, n]) => (
            <span key={tool}>
              <b className="font-semibold text-ink tabular-nums">{n}</b> {tool}
            </span>
          ))}
          {conceded > 0 && (
            <span className="text-bad">
              <b className="font-semibold tabular-nums">{conceded}</b> conceded
            </span>
          )}
        </div>
      )}

      {made.length > 0 && (
        <ul className="mt-2 flex flex-col">
          {made.map((p) => {
            const t = state.tallies[p.id];
            return (
              <li key={p.id} className="border-t border-line py-1.5 text-[12px] text-ink-2">
                <b className="font-mono">P{p.id}</b>{" "}
                <span className="text-ink-3">
                  {t ? STATUS_LABEL[t.status] : "open"}
                  {t ? ` ${t.score > 0 ? `+${t.score}` : t.score}` : ""}
                </span>{" "}
                — {p.text.length > 140 ? `${p.text.slice(0, 140).trim()}…` : p.text}
              </li>
            );
          })}
        </ul>
      )}

      <BoardPulse state={state} />
    </div>
  );
}
