import { memo, useMemo } from "react";
import { NewspaperClipping } from "@phosphor-icons/react/dist/csr/NewspaperClipping";
import { Md } from "./Markdown";
import { TurnState, actionCountLabel } from "./Panels";
import { seatSlots } from "../seats";
import { READING_LABEL, STATUS_LABEL, type Conclusion } from "../types";
import type { CouncilState } from "../useCouncil";

/**
 * The short status for a finished debate.
 *
 * "Settled early" and "called settled" were indistinguishable to a reader and
 * meant different things: the first is the engine noticing nothing was left to
 * argue, the second is the Summarizer deciding the record was complete. Both
 * now name the cause, so the pill and its tooltip agree.
 */
export function endLabel(state: CouncilState): string {
  if (!state.verdict) return "";
  if (state.verdict.adjournedBy) return "ended early";
  switch (state.verdict.endedHow) {
    case "settled":
      return "all points settled";
    case "calledSettled":
      return "ended by the Summarizer";
    case "stopped":
      return "stopped by you";
    case "complete":
      return "ran every round";
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
      return `Stopped after round ${done} of ${total}, when every point had been decided and no delegate was left to argue`;
    case "calledSettled":
      // "converged" was doing a lot of work here: nothing told the reader
      // whether it was a good outcome, or even what it referred to.
      return (
        state.verdict.settledReason ??
        `The Summarizer decided after round ${done} of ${total} that the debate had said everything it needed to`
      );
    case "stopped":
      return `You stopped this after round ${done} of ${total}`;
    case "complete":
      return `All ${total} rounds were spoken`;
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
function AnswerSection({ conclusion }: { conclusion: Conclusion }) {
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
        <FinishedRecord state={state} />
      )}
    </div>
  );
}

/**
 * The written record, split out and memoized on the verdict itself.
 *
 * This is the whole point when a debate ends while the operator is still on the
 * Record tab: everything after `debateEnded` is noise — resyncs, notices, the
 * last tokens of a closing sentence — and re-parsing the verdict's markdown plus
 * three point lists on each one is work for a page that cannot change again.
 *
 * A verdict is written once and never mutated, so its identity is exactly the
 * right dependency. Passing `state` here instead would defeat it, which is the
 * trap `RunTrace` fell into.
 */
const FinishedRecord = memo(function FinishedRecord({ state }: { state: CouncilState }) {
  const verdict = state.verdict!;
  return (
    <section>
      {verdict.conclusion && <AnswerSection conclusion={verdict.conclusion} />}
      {verdict.finalSummary ? (
        <div className="mb-3 border-b border-line pb-3">
          <h3 className="section-title mb-1.5">End result</h3>
          <Md text={verdict.finalSummary} compact />
        </div>
      ) : null}
      {/* Also markdown, and always was inconsistent with the `finalSummary`
          above it: the same field rendered two different ways in the reader and
          the live chamber, so a summary containing a list or emphasis lost its
          formatting in one of them. */}
      <div className="text-[13px] leading-relaxed text-ink-2">
        <Md text={verdict.summary} />
      </div>
      {verdict.crux && (
        <p className="mt-2 text-[12px] text-ink-3">
          {/* "Crux" is a term of art from classical rhetoric, used here with
              no gloss. Now: the point the whole debate turned on. */}
          <strong className="text-ink-2">The point it turned on</strong> — P
          {verdict.crux.id}: {verdict.crux.text}
        </p>
      )}
      <ul className="mt-3 flex flex-col">
        {verdict.carried.map((p) => (
          <li key={p.id} className="border-t border-line py-2 text-[12px] text-ink-2">
            <b className="font-mono text-ok">P{p.id}</b> stood, {p.score} votes for — {p.text}
          </li>
        ))}
        {verdict.fell.map((p) => (
          <li key={p.id} className="border-t border-line py-2 text-[12px] text-ink-2">
            <b className="font-mono text-bad">P{p.id}</b> did not stand, {p.score} votes against —{" "}
            {p.text}
          </li>
        ))}
        {verdict.stalemate.map((p) => (
          <li key={p.id} className="border-t border-line py-2 text-[12px] text-ink-2">
            <b className="font-mono text-warn">P{p.id}</b> tied — {p.text}
            {p.ruling && <span className="text-ink-3"> (ruled {READING_LABEL[p.ruling]})</span>}
          </li>
        ))}
      </ul>
    </section>
  );
}, areVerdictEqual);

/**
 * Compares only what the finished record draws.
 *
 * Written out rather than relying on `memo`'s default, because the default would
 * compare the one prop there is — the whole `state` — and always find it
 * different. A finished debate still receives tokens, notices and resyncs; none
 * of them touch the page being displayed.
 */
function areVerdictEqual(a: { state: CouncilState }, b: { state: CouncilState }): boolean {
  return a.state.verdict === b.state.verdict && a.state.delegates === b.state.delegates;
}

/**
 * The board's pulse while there is no verdict yet: how many points stand
 * where. The full verdict replaces this the moment the debate closes.
 *
 * Memoized: the chamber re-renders on every streamed token (~25/s), and the
 * triple `points.filter` scan otherwise reran per token for counts that only
 * move when the board does.
 */
const BoardPulse = memo(function BoardPulse({ state }: { state: CouncilState }) {
  const counts = useMemo(() => {
    let carried = 0;
    let fell = 0;
    let stalemate = 0;
    for (const p of state.points) {
      const s = state.tallies[p.id]?.status;
      if (s === "carried") carried += 1;
      else if (s === "fell") fell += 1;
      else if (s === "stalemate") stalemate += 1;
    }
    return { carried, fell, stalemate, total: state.points.length };
  }, [state.points, state.tallies]);
  if (!state.points.length) return null;
  return (
    <p className="mt-2 text-[11.5px] tabular-nums text-ink-3">
      <span className="text-ok">{counts.carried} stood</span>
      {" · "}
      <span className="text-bad">{counts.fell} did not stand</span>
      {" · "}
      <span className="text-warn">{counts.stalemate} tied</span>
      {" · "}
      {counts.total} points
    </p>
  );
});

/**
 * The Record tab before there is a record: who holds the floor right now,
 * what they have done with it, and what the board says so far. The transcript
 * shows every word; this is the glanceable version.
 *
 * Memoized: it lives under the per-token chamber render, and the
 * `Object.entries(acts).sort` + `points.filter(raisedBy)` scans otherwise
 * reran per token for a speaker whose counts rarely move mid-turn.
 */
const LiveRecord = memo(function LiveRecord({ state }: { state: CouncilState }) {
  const live = state.live;
  const liveDelegate = live?.delegate;
  const slots = useMemo(() => seatSlots(state.delegates), [state.delegates]);
  const actsForLive = liveDelegate ? (state.acts[liveDelegate] ?? {}) : null;
  // Sorted once per acts change for this seat, not per streamed token.
  const tools = useMemo(
    () => Object.entries(actsForLive ?? {}).sort((a, b) => b[1] - a[1]),
    [actsForLive],
  );
  // Points this seat raised: rescanned only when the board or the speaker moves.
  const made = useMemo(
    () => (liveDelegate ? state.points.filter((p) => p.raisedBy === liveDelegate) : []),
    [state.points, liveDelegate],
  );
  const slotOf = (id: string) => slots[id] ?? 1;
  if (!live) {
    return (
      <div className="py-8 text-center">
        <p className="text-[12.5px] font-medium text-ink-2">
          {state.running ? `Between turns — round ${Math.max(1, state.round)}.` : "Stopped before a verdict."}
        </p>
        <p className="mx-auto mt-1 max-w-[28ch] text-[11.5px] leading-relaxed text-ink-3">
          {state.running
            ? "The next delegate is about to speak."
            : "The Board tab alongside this one lists every point decided so far."}
        </p>
        <BoardPulse state={state} />
      </div>
    );
  }

  const raised = state.raised[live.delegate] ?? 0;
  const conceded = state.conceded[live.delegate] ?? 0;
  const thinking = live.reasoning.trim();
  const excerpt = thinking.length > 300 ? `${thinking.slice(0, 300).trim()}…` : thinking;

  return (
    <div>
      <h3 className="section-title mb-1.5">Now speaking</h3>
      <div className="mb-1.5 flex flex-wrap items-center gap-x-2 gap-y-1">
        <span
          className="size-2.5 shrink-0 rounded-full"
          style={{ background: `var(--seat-${slotOf(live.delegate)}, ${live.color})` }}
        />
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
          {/* Was printing the engine's own action names — "2 make_point",
              "1 steelman" — because this row read the raw keys of `acts`.
              `actionCountLabel` is the same table the transcript badges use. */}
          {tools.map(([tool, n]) => (
            <span key={tool}>
              <b className="font-semibold text-ink tabular-nums">{n}</b>{" "}
              {actionCountLabel(tool, n)}
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
});
