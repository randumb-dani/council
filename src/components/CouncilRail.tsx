import { memo, useEffect, useMemo, useRef, useState } from "react";
import { UsersThree } from "@phosphor-icons/react/dist/csr/UsersThree";
import type { CouncilState } from "../useCouncil";
import { seatGlyphs, seatInk, seatSlots } from "../seats";
import { fmtElapsed } from "./Panels";
import { ROLE_HINT, ROLE_LABEL, type Verdict } from "../types";

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
    // The rail now draws the per-seat failure record, so a failed turn has to
    // be in the dependency list. It was the omission that let the rail keep
    // showing a healthy seat while the engine was failing on it every round.
    state.seatFailures,
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
  // The left border is the rail's main grouping cue, so it has to survive the
  // monochrome themes: `--seat-N` is defined there and undefined here, which is
  // what makes the fallback resolve to each seat's own colour.
  const slots = useMemo(() => seatSlots(state.delegates), [state.delegates]);
  const glyphs = useMemo(
    () => seatGlyphs(state.delegates.map((d) => ({ id: d.id, role: d.role }))),
    [state.delegates],
  );
  const seatColor = (id: string) => {
    const d = state.delegates.find((x) => x.id === id);
    return `var(--seat-${slots[id] ?? 1}, ${d?.color ?? "#6c8b84"})`;
  };

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

  // Memoized: loops all points per delegate → O(D×N) per rail render.
  // The rail re-renders with the chamber on every streamed token, so
  // recomputing this per render is pure waste when points/tallies are still.
  const spreads = useMemo(() => {
    const out: Record<string, Record<Verdict, number>> = {};
    for (const d of state.delegates) out[d.id] = { win: 0, lose: 0, stalemate: 0 };
    for (const p of state.points) {
      const ratings = state.tallies[p.id]?.ratings;
      if (!ratings) continue;
      for (const [id, v] of Object.entries(ratings)) {
        const bucket = out[id];
        if (bucket && v) bucket[v] += 1;
      }
    }
    return out;
  }, [state.delegates, state.points, state.tallies]);
  // Memoized: the latest failure per delegate, computed once instead of a
  // per-row `[...seatFailures].reverse().find`. The list is append-ordered
  // and capped, so a forward scan with overwrite keeps the newest.
  const lastFailureByDelegate = useMemo(() => {
    const m = new Map<string, (typeof state.seatFailures)[number]>();
    for (const f of state.seatFailures) m.set(f.delegate, f);
    return m;
  }, [state.seatFailures]);

  return (
    <div className="px-3 pt-3 pb-1">
      <div className="mb-1 flex items-baseline justify-between px-1">
        <h2 className="section-title">Council</h2>
        <span className="text-[11px] text-ink-3 tabular-nums">
          {state.delegates.length} delegate{state.delegates.length === 1 ? "" : "s"}
        </span>
      </div>

      {state.delegates.length === 0 && (
        <div className="flex items-center gap-2 px-1 py-3 text-ink-3">
          <UsersThree size={16} />
          <p className="text-[11.5px]">
            {/* Was "add a delegate and a model for each", which told an operator
                to do something the app had already done: `defaultConfig` ships
                a full five-seat roster with the roles filled in. What is
                actually missing is a model per seat and a motion. */}
            The council's seats are listed under Delegates in the setup panel. Give
            each one a model, set the question, then press Convene.
          </p>
        </div>
      )}

      {/* `role="list"`, so the label lands on something. An `aria-label` on a plain
          `div` is not exposed by assistive tech — the roster was labelled only
          in the source. Deliberately not a live region: the whole roster
          re-renders per token, and announcing it would be the loudest possible
          way to report nothing. */}
      <div className="divide-y divide-line" role="list" aria-label="the delegates">
        {state.delegates.map((d) => {
          const speaking = state.live?.delegate === d.id;
          const skippedRounds = state.skipped.filter((s) => s.ids.includes(d.id));
          const skipped = !speaking && skippedRounds.length > 0;
          const onDeck = !speaking && !skipped && nextId === d.id;
          const a = acts(d.id);
          const u = state.usage[d.id];
          const t2 = state.timing[d.id];
          const s = spreads[d.id] ?? { win: 0, lose: 0, stalemate: 0 };
          const rated = s.win + s.lose + s.stalemate;
          const conceded = state.conceded[d.id] ?? 0;
          // From the once-per-render map above, not a per-row scan.
          const lastFailure = lastFailureByDelegate.get(d.id);
          const who = lastFailure?.delegate
            ? (state.delegates.find((x) => x.id === lastFailure.delegate)?.label ?? lastFailure.delegate)
            : "";
          return (
            <div
              key={d.id}
              role="listitem"
              className="py-2.5 first:pt-1.5 last:pb-1.5"
              style={{ borderLeft: `2px solid ${seatColor(d.id)}`, paddingLeft: "10px" }}
            >
              {/* Label takes the slack and truncates; the pills keep their own
                  width. Fixed geometry here is what stops the row shifting as
                  "speaking" comes and goes. */}
              <div className="flex items-center gap-2">
                <span
                  className="avatar size-4 shrink-0 text-[9px]"
                  style={{
                    background: seatColor(d.id),
                    color: seatInk(slots[d.id] ?? 1, d.color),
                  }}
                  title={d.label}
                  aria-hidden
                >
                  {glyphs[d.id] ?? "?"}
                </span>
                <span className="min-w-0 flex-1 truncate text-[13px] font-semibold tracking-tight text-ink">
                  {d.label}
                </span>
<span className="flex shrink-0 items-center gap-1.5">
                  {/* A delegate you argue is not waiting on a model, and the
                      rail is where that is otherwise invisible — its name sits
                      in the same list as five machines. */}
                  {d.human && (
                    <span
                      className="status-pill bg-primary text-on-primary"
                      title="You argue this one. The council waits for you."
                    >
                      you
                    </span>
                  )}
                  {/* Every role gets its hint now. Proponent, opponent and judge were
                      passing `undefined`, which left three of the five pills in
                      the rail with no explanation anywhere. */}
                  <span
                    className="status-pill border border-line-2 text-ink-3"
                    title={ROLE_HINT[d.role] ?? ROLE_HINT.custom}
                  >
                    {ROLE_LABEL[d.role] ?? d.role}
                  </span>
                {speaking && (
                  /* No `role="status"`. It mounts and unmounts once per turn, and the rail
                     holds the speaker on screen through the gap before the next
                     turn — so the announcement outlived the fact by most of a
                     turn, telling the listener someone was speaking when they
                     were not. The per-seat state is visible; the transcript is
                     where "who has the floor" is actually followed. */
                  <span className="status-pill bg-primary-soft text-primary">
                    <span aria-hidden>●</span> speaking
                  </span>
                )}
                {skipped && (
                  <span
                    className="status-pill border border-line-2 text-ink-3"
                    title={`This delegate's last turn was skipped${
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
                    title="This delegate speaks next, in the usual order"
                  >
                    next
                  </span>
                )}
                {/* A seat whose turn failed, kept where it can be found.
                    The reducer accumulates these precisely so a failure is a
                    per-seat record, and nothing rendered it: the only trace was
                    one generic red notice that scrolled off the top of the
                    strip. When a delegate is failing every round — a wrong model
                    name, a server that went away — this is what tells you which
                    one and why, without hunting through notices. */}
                {lastFailure && (
                  <span
                    className="status-pill border border-bad/50 text-bad"
                    title={lastFailure.message}
                  >
                    failing
                  </span>
                )}
                </span>
              </div>

              {lastFailure && (
                <p className="mt-1 text-[11px] leading-relaxed text-bad">
                  <span className="font-semibold">
                    {who ? who : "A turn"} failed:
                  </span>{" "}
                  {lastFailure.message}
                </p>
              )}

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
                  <span title={`Total across ${u.turns} turn${u.turns === 1 ? "" : "s"}, as reported by the server. Only shown when the server counts them.`}>
                    <b className="font-semibold text-ink tabular-nums">{fmt(u.prompt + u.completion)}</b>{" "}
                    tokens
                  </span>
                )}
                {t2 && t2.secs > 0 && (
                  <span
                    title={`Total time spent waiting across ${t2.turns} turn${t2.turns === 1 ? "" : "s"}.`}
                    className="tabular-nums"
                  >
                    <b className="font-semibold text-ink tabular-nums">{fmtElapsed(t2.secs)}</b> on the
                    clock
                  </span>
                )}
              </div>

              {rated > 0 && (
                <div
                  className="spread mt-1.5"
                  role="img"
                  aria-label={`Across the board: ${s.win} supporting, ${s.lose} opposing, ${s.stalemate} neutral`}
                  title={`${s.win} support · ${s.lose} oppose · ${s.stalemate} neutral across every point rated so far`}
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
