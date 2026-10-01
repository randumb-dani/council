import { memo, useEffect, useMemo, useRef, useState } from "react";
import { UsersThree } from "@phosphor-icons/react";
import type { CouncilState } from "../useCouncil";
import { ROLE_LABEL, type Verdict } from "../types";

/** mm:ss once it passes a minute; seconds are noise above an hour anyway. */
function mmss(secs: number): string {
  const total = Math.max(0, Math.floor(secs));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return m ? `${m}m ${String(s).padStart(2, "0")}s` : `${s}s`;
}

/** 12,400 rather than 12.4k: a cost meter you cannot add up in your head is
 * not one you will reason about. */
function fmt(n: number): string {
  return n.toLocaleString();
}

/**
 * The council rail: who is in the room, and what they have actually done.
 * Deliberately flat — rows separated by hairlines, status as small type,
 * color doing the grouping instead of boxes.
 *
 * Wrapper exists to cut the render rate, not the code: a reasoning model
 * streams dozens of chunks a second, none of which this rail shows, yet every
 * one of them used to re-render all four seat rows — a visible stutter on the
 * one panel you watch while waiting.
 */
export function CouncilRail({ state }: { state: CouncilState }) {
  // Turn boundaries clear `live` on speechFinished milliseconds before the
  // next turnStarted lands. Without a grace period the speaking badge unmounts
  // and remounts every turn — and every row reflows twice. Hold the last
  // speaker briefly so the badge rides straight through the gap.
  const displayDelegate = useHeldSpeaker(state.live?.delegate ?? null, state.running);
  // Rebuilt only when something this rail actually draws changes. Live turn
  // text, reasoning and the elapsed clock are excluded on purpose: they belong
  // to the transcript, and reading them here was what made the panel twitch.
  const view = useMemo(() => {
    let live = state.live
      ? { ...state.live, text: "", reasoning: "", tokens: 0, elapsed: 0, activity: null }
      : null;
    if (!live && displayDelegate && state.running) {
      // The held speaker, still on screen through the gap. Never shown for a
      // seat that was skipped: it carries its own badge already.
      const d = state.delegates.find((x) => x.id === displayDelegate);
      const wasSkipped = state.skipped.some((s) => s.ids.includes(displayDelegate));
      if (d && !wasSkipped) {
        live = {
          turn: -1,
          delegate: d.id,
          label: d.label,
          color: d.color,
          model: d.model,
          round: state.round,
          text: "",
          reasoning: "",
          elapsed: 0,
          tokens: 0,
          activity: null,
          skipped: [],
        };
      }
    }
    return { ...state, live };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    state.delegates,
    state.live?.delegate,
    displayDelegate,
    state.skipped,
    state.acts,
    state.usage,
    state.timing,
    state.points,
    state.tallies,
    state.conceded,
    state.raised,
    state.running,
    state.round,
    state.speeches,
  ]);
  return <RailRows state={view} />;
}

/**
 * The last live speaker, held briefly after their turn ends. Returns the live
 * delegate while anyone holds the floor, the previous one for a beat after it
 * clears, and null once the debate stops or the beat passes.
 */
function useHeldSpeaker(liveDelegate: string | null, running: boolean): string | null {
  const [held, setHeld] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    if (liveDelegate) {
      if (timer.current) {
        clearTimeout(timer.current);
        timer.current = null;
      }
      setHeld(liveDelegate);
    } else if (running) {
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => {
        setHeld(null);
        timer.current = null;
      }, 600);
      return () => {
        if (timer.current) {
          clearTimeout(timer.current);
          timer.current = null;
        }
      };
    } else {
      if (timer.current) {
        clearTimeout(timer.current);
        timer.current = null;
      }
      setHeld(null);
    }
  }, [liveDelegate, running]);
  return liveDelegate ?? held;
}

const RailRows = memo(function RailRows({ state }: { state: CouncilState }) {
  const acts = (id: string) => state.acts[id] ?? {};

  // Round-robin is the speaking order, so the seat after whoever spoke last is
  // visibly "on deck". Cheap, and it makes the turn order legible.
  const order = state.delegates.map((d) => d.id);
  const lastId =
    state.live?.delegate ?? state.speeches[state.speeches.length - 1]?.delegate;
  const at = lastId ? order.indexOf(lastId) : -1;
  // indexOf returns -1 for a speaker no longer on the roster, which would
  // badge seat 0 as "next" for the rest of the debate.
  const nextId =
    state.running && at >= 0 && order.length
      ? order[(at + 1) % order.length]
      : undefined;

  const spread = (id: string) => {
    const out: Record<Verdict, number> = { win: 0, lose: 0, stalemate: 0 };
    for (const p of state.points) {
      const v = state.tallies[p.id]?.ratings[id];
      if (v) out[v] += 1;
    }
    return out;
  };

  return (
    <div className="px-3 pt-3 pb-1">
      <div className="mb-1 flex items-baseline justify-between px-1">
        <h2 className="section-title">Council</h2>
        <span className="text-[11px] text-ink-3 tabular-nums">
          {state.delegates.length} seat{state.delegates.length === 1 ? "" : "s"}
        </span>
      </div>

      {state.delegates.length === 0 && (
        <div className="flex items-center gap-2 px-1 py-3 text-ink-3">
          <UsersThree size={16} />
          <p className="text-[11.5px]">Add delegates below, then press convene.</p>
        </div>
      )}

      {/* Only the live status is announced — the whole roster re-rendering
          per token as a single live region spammed screen readers. */}
      <div className="divide-y divide-line" aria-label="the council">
        {state.delegates.map((d) => {
          const speaking = state.live?.delegate === d.id;
          const skippedRounds = state.skipped.filter((s) => s.ids.includes(d.id));
          const skipped = !speaking && skippedRounds.length > 0;
          const onDeck = !speaking && !skipped && nextId === d.id;
          const a = acts(d.id);
          const u = state.usage[d.id];
          const t2 = state.timing[d.id];
          const s = spread(d.id);
          const rated = s.win + s.lose + s.stalemate;
          const conceded = state.conceded[d.id] ?? 0;
          return (
            <div
              key={d.id}
              className="py-2.5 first:pt-1.5 last:pb-1.5"
              style={{ borderLeft: `2px solid ${d.color}`, paddingLeft: "10px" }}
            >
              {/* Label takes the slack and truncates; the pills keep their own
                  width. Fixed geometry here is what stops the row shifting as
                  "speaking" comes and goes. */}
              <div className="flex items-center gap-2">
                <span className="min-w-0 flex-1 truncate text-[13px] font-semibold tracking-tight text-ink">
                  {d.label}
                </span>
                <span className="flex shrink-0 items-center gap-1.5">
                  <span
                    className="status-pill border border-line-2 text-ink-3"
                    title={
                      d.role === "summarizer"
                        ? "lays out each round and the end result; never argues"
                        : d.role === "custom"
                          ? "added seat — argues the motion"
                          : undefined
                    }
                  >
                    {ROLE_LABEL[d.role] ?? d.role}
                  </span>
                {speaking && (
                  <span className="status-pill bg-primary-soft text-primary" role="status">
                    <span aria-hidden>●</span> speaking
                  </span>
                )}
                {skipped && (
                  <span
                    className="status-pill border border-line-2 text-ink-3"
                    title={`this seat's last turn was skipped${
                      skippedRounds.length
                        ? ` (R${skippedRounds[skippedRounds.length - 1]!.round})`
                        : ""
                    }`}
                  >
                    skipped
                  </span>
                )}
                {onDeck && (
                  <span
                    className="status-pill bg-surface-3 text-ink-2"
                    title="speaks next, in seat order"
                  >
                    next
                  </span>
                )}
                </span>
              </div>

              <div
                className="mt-0.5 truncate font-mono text-[10.5px] text-ink-3"
                title={d.model || "no model set"}
              >
                {d.model || "no model set"}
              </div>

              <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[11px] text-ink-3">
                <span>
                  <b className="font-semibold text-ink tabular-nums">
                    {state.raised[d.id] ?? 0}
                  </b>{" "}
                  raised
                </span>
                <span>
                  <b className="font-semibold text-ink tabular-nums">{a.vote ?? 0}</b> votes
                </span>
                {conceded > 0 && (
                  <span className="text-bad">
                    <b className="font-semibold tabular-nums">{conceded}</b> conceded
                  </span>
                )}
                {/* Only when the endpoint reported. No key means the server
                    never said, and "unknown" is not "nothing". */}
                {u && (
                  <span title={`${u.turns} measured turn${u.turns === 1 ? "" : "s"}`}>
                    <b className="font-semibold text-ink tabular-nums">{fmt(u.prompt + u.completion)}</b>{" "}
                    tokens
                  </span>
                )}
                {t2 && t2.secs > 0 && (
                  <span
                    title={`${t2.turns} timed turn${t2.turns === 1 ? "" : "s"}`}
                    className="tabular-nums"
                  >
                    <b className="font-semibold text-ink tabular-nums">{mmss(t2.secs)}</b> on the
                    clock
                  </span>
                )}
              </div>

              {rated > 0 && (
                <div
                  className="spread mt-1.5"
                  role="img"
                  aria-label={`across the board: ${s.win} points won, ${s.lose} lost, ${s.stalemate} undecided`}
                  title={`${s.win} win · ${s.lose} lose · ${s.stalemate} stalemate across the board`}
                >
                  {s.win > 0 && (
                    <i style={{ width: `${(s.win / rated) * 100}%`, background: "var(--ok)" }} />
                  )}
                  {s.lose > 0 && (
                    <i style={{ width: `${(s.lose / rated) * 100}%`, background: "var(--bad)" }} />
                  )}
                  {s.stalemate > 0 && (
                    <i
                      style={{ width: `${(s.stalemate / rated) * 100}%`, background: "var(--warn)" }}
                    />
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
});
