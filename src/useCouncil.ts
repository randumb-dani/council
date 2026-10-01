// The reducer that turns the engine's event stream into something renderable.
// The engine is authoritative: this never decides anything, it only accumulates.

import { useCallback, useEffect, useMemo, useReducer } from "react";
import * as bridge from "./bridge";
import type {
  DebateVerdict,
  DelegateBrief,
  EngineEvent,
  NoticeTone,
  Point,
  Poll,
  Speech,
  StalemateReview,
  Tally,
  Verdict,
} from "./types";

export interface LiveTurn {
  turn: number;
  delegate: string;
  label: string;
  color: string;
  model: string;
  round: number;
  text: string;
  /** Thinking streamed so far this turn. */
  reasoning: string;
  /** Seconds the current request has been in flight, from heartbeats. */
  elapsed: number;
  /**
   * Streamed pieces so far this turn — an aliveness signal, not a bill.
   *
   * The server only reports real token counts once a turn finishes, and those
   * arrive as `turnUsage` and are summed per seat in `usage`. Reading this as
   * a token count is what made the live figure look like a guess.
   */
  tokens: number;
  /**
   * What the engine is doing right now, in its own words.
   *
   * Null while the model is simply writing. Set the moment the seat reaches for
   * a tool, and left up while the engine asks other seats a question — because a
   * seat waiting on a poll looks exactly like a seat that has hung, and the
   * difference is the difference between "wait" and "something is wrong".
   */
  activity: string | null;
  skipped: string[];
}

/** Longest a piece of activity text may be, since it is shown as it arrives. */
const ACTIVITY_MAX = 60;

/** Must match the engine's marker, character for character. */
const RESET_MARKER = "\u{0}reset\u{0}";

/**
 * Count concessions across the board rather than incrementing a counter. A
 * seat that conceded and then reversed itself during a deadlock review kept
 * its "2 conceded" badge for the rest of the debate.
 */
function nextConceded(points: Point[], changed: Point): Record<string, number> {
  const out: Record<string, number> = {};
  for (const p of points) {
    const source = p.id === changed.id ? changed : p;
    for (const who of Object.keys(source.concededBy)) {
      out[who] = (out[who] ?? 0) + 1;
    }
  }
  return out;
}

export interface PendingExtension {
  rounds: number;
  reason: string;
  round: number;
  /**
   * Every seat asked about the request, in the order they were asked.
   *
   * Kept on the proposal rather than beside it because the proposal is what the
   * operator is looking at: they need to see the count build to know whether
   * their answer still matters.
   */
  ballots: { delegate: string; grant: boolean; reason: string | null }[];
}

export interface CouncilState {
  running: boolean;
  /**
   * When the debate started and stopped, for the session clock.
   *
   * Both are wall-clock rather than accumulated turn times, because the time
   * somebody wants is the time they have been sitting there. A debate that
   * spends two minutes waiting on one seat took two minutes.
   */
  startedAt: number | null;
  endedAt: number | null;
  round: number;
  maxRounds: number;
  motion: string;
  delegates: DelegateBrief[];
  /** Images attached to the motion, without the bytes. */
  images: { id: string; name: string }[];
  speeches: Speech[];
  live: LiveTurn | null;
  points: Point[];
  /** The engine's own scoring, keyed by point id. Never recomputed here. */
  tallies: Record<number, Tally>;
  polls: Poll[];
  notices: { id: number; text: string; tone: NoticeTone }[];
  verdict: DebateVerdict | null;
  /** Skips by round, so the badge expires when the seat argues again. */
  skipped: { round: number; ids: string[] }[];
  /** Per-seat activity, for the council rail. */
  acts: Record<string, Record<string, number>>;
  raised: Record<string, number>;
  conceded: Record<string, number>;
  seenPoints: number[];
  /** Round whose intermission is on the floor, if any. */
  intermission: number | null;
  /** The summarizer's proposal waiting on the operator, if any. */
  extension: PendingExtension | null;
  /** Extra rounds still unspent from the convened budget. */
  extraBudget: number;
  /** The convened budget, for the "x of y" line. */
  extraTotal: number;
  /**
   * Live deadlock reviews by point id. These used to be dropped on the floor,
   * which made a review look like nothing happening for the length of a round
   * trip per seat.
   */
  reviews: Record<number, StalemateReview>;
  /** The adjournment motion while it is being answered, if any. */
  adjourn: AdjournMotion | null;
  /** Seats whose turn failed, kept out of `running` so Stop survives. */
  seatFailures: { delegate: string; message: string }[];
  /**
   * Tokens per seat, when the endpoint was asked to report them.
   *
   * Absent keys mean the server never answered, which is a different fact from
   * a seat that cost nothing. The rail shows the difference rather than a zero.
   */
  usage: Record<string, { prompt: number; completion: number; turns: number }>;
  /**
   * Every change of mind, per point.
   *
   * `pointUpdated` replaces a point wholesale, so the engine's per-vote `round`
   * was thrown away on every update and a seat changing its vote looked exactly
   * like a seat that had held the same view all along. Diffing old against new
   * recovers it, and a mind changed mid-debate is the most interesting thing on
   * the board.
   */
  voteLog: Record<number, VoteChange[]>;
  /** Seconds on the clock per seat, from the turn heartbeats. */
  timing: Record<string, { secs: number; turns: number }>;
  /**
   * Pages read off the reference shelf, by the turn that read them.
   *
   * Kept beside the transcript rather than inside it, because the action row is
   * a summary of what a seat did while this is the thing it was given: a seat
   * citing a page the operator cannot open is no more trustworthy than one
   * inventing it, and inventing it at least stays consistent.
   */
  reads: Record<number, ReadPage[]>;
}

export interface VoteChange {
  round: number;
  delegate: string;
  from: Verdict | null;
  to: Verdict;
  reason: string | null;
}

export interface ReadPage {
  source: string;
  url: string;
  title: string;
  text: string;
  truncated: boolean;
}

export interface AdjournMotion {
  by: string;
  reason: string;
  round: number;
  yes: number;
  no: number;
  /** Voters still to answer. */
  outstanding: string[];
  passed: boolean | null;
}

const initial: CouncilState = {
  running: false,
  startedAt: null,
  endedAt: null,
  round: 0,
  maxRounds: 0,
  motion: "",
  delegates: [],
  images: [],
  speeches: [],
  live: null,
  points: [],
  tallies: {},
  polls: [],
  notices: [],
  verdict: null,
  skipped: [],
  acts: {},
  raised: {},
  conceded: {},
  seenPoints: [],
  intermission: null,
  extension: null,
  extraBudget: 0,
  extraTotal: 0,
  reviews: {},
  adjourn: null,
  seatFailures: [],
  reads: {},
  usage: {},
  voteLog: {},
  timing: {},
};

let noticeSeq = Date.now() % 1000000;

/**
 * Events the UI synthesises for itself. Deliberately not part of
 * `EngineEvent`: the engine never emits a snapshot, because it never loses one.
 */
type UiEvent =
  | { type: "snapshot"; snapshot: bridge.DebateSnapshot }
  | Extract<EngineEvent, { type: "notice" }>;

function reduce(state: CouncilState, event: EngineEvent | UiEvent): CouncilState {
  switch (event.type) {
    case "debateStarted":
      return {
        ...initial,
        running: true,
        // Restarted rather than resumed: this is a new run, and a clock that
        // carried over from the last one would be answering a different question.
        startedAt: Date.now(),
        endedAt: null,
        motion: event.motion,
        maxRounds: event.rounds,
        round: 0,
        delegates: event.delegates,
        images: event.images ?? [],
        extraBudget: event.maxExtraRounds,
        extraTotal: event.maxExtraRounds,
      };
    case "roundStarted":
      return { ...state, round: event.round, intermission: null };
    case "intermissionStarted":
      return { ...state, intermission: event.round };
    case "turnStarted":
      return {
        ...state,
        intermission: null,
        live: {
          turn: event.turn,
          delegate: event.delegate,
          label: event.label,
          color: event.color,
          model: event.model,
          round: event.round,
          text: "",
          reasoning: "",
          elapsed: 0,
          tokens: 0,
          activity: null,
          skipped: [],
        },
      };
    case "turnActivity": {
      // Activity belongs to the turn it was reported for: a ballot asked during
      // the opener's turn keeps its turn id, but a leftover line from the
      // previous seat would read as if this one were stuck on it.
      if (!state.live || state.live.turn !== event.turn) return state;
      return {
        ...state,
        live: {
          ...state.live,
          activity: event.what.slice(0, ACTIVITY_MAX),
        },
      };
    }
    case "turnProgress": {
      if (!state.live || state.live.turn !== event.turn) return state;
      return {
        ...state,
        live: { ...state.live, elapsed: event.elapsedSecs, tokens: event.tokens },
      };
    }
    case "turnSkipped": {
      // Keyed by round: a permanent badge made a seat that skipped one turn
      // look like it had failed to argue for the rest of the debate.
      const round = state.round;
      const existing = state.skipped.find((r) => r.round === round);
      const merged = existing
        ? state.skipped.map((r) =>
            r.round === round && !r.ids.includes(event.delegate)
              ? { ...r, ids: [...r.ids, event.delegate] }
              : r,
          )
        : [...state.skipped.filter((r) => r.round !== round), { round, ids: [event.delegate] }];
      return {
        ...state,
        live: state.live?.turn === event.turn ? null : state.live,
        skipped: merged.slice(-20),
      };
    }
    case "token": {
      if (!state.live || state.live.turn !== event.turn) return state;
      // The engine's marker for "discard what you have": a request retried in
      // another mode. Appending here made the transcript repeat itself.
      if (event.text === RESET_MARKER) {
        return { ...state, live: { ...state.live, text: "", tokens: 0 } };
      }
      return {
        ...state,
        live: { ...state.live, text: state.live.text + event.text, activity: null },
      };
    }
    case "snapshot": {
      // Reconciliation after a reload: the engine is still running a debate
      // this window never saw, so re-adopt it instead of offering Convene
      // over a run nobody can stop.
      const snap = event.snapshot as bridge.DebateSnapshot & {
        delegates?: DelegateBrief[];
        tallies?: Record<number, Tally>;
        maxRounds?: number;
      };
      const tallies: Record<number, Tally> = (snap.tallies as Record<number, Tally> | undefined) ?? {};
      return {
        ...initial,
        running: !snap.finished,
        // A finished snapshot is an ending too — stop the clock.
        endedAt: snap.finished ? Date.now() : null,
        motion: snap.motion,
        round: snap.round,
        maxRounds: Math.max(snap.maxRounds ?? snap.round ?? 1, 1),
        delegates: snap.delegates ?? [],
        points: snap.points,
        tallies,
        polls: snap.polls,
        speeches: snap.speeches,
        seenPoints: snap.points.map((p) => p.id),
      };
    }
    case "reasoning": {
      if (!state.live || state.live.turn !== event.turn) return state;
      return {
        ...state,
        live: { ...state.live, reasoning: state.live.reasoning + event.text },
      };
    }
    case "speechFinished": {
      // The heartbeat's last elapsed reading is the closest thing to a turn's
      // duration, and it is the only one that exists. Recorded when the turn
      // closes rather than sampled during it, so a stream of updates does not
      // each add to the total.
      const secs = state.live?.turn === event.turn ? (state.live.elapsed ?? 0) : 0;
      const timing =
        secs > 0
          ? {
              ...state.timing,
              [event.speech.delegate]: {
                secs: (state.timing[event.speech.delegate]?.secs ?? 0) + secs,
                turns: (state.timing[event.speech.delegate]?.turns ?? 0) + 1,
              },
            }
          : state.timing;
      // The speech is authoritative; drop the streamed copy of it.
      const speeches = state.speeches.filter((s) => s.id !== event.speech.id);
      return {
        ...state,
        speeches: [...speeches, event.speech].sort((a, b) => a.round - b.round || a.id - b.id),
        timing,
        live: state.live?.turn === event.turn ? null : state.live,
      };
    }
    case "turnUsage": {
      const prev = state.usage[event.delegate] ?? { prompt: 0, completion: 0, turns: 0 };
      return {
        ...state,
        usage: {
          ...state.usage,
          [event.delegate]: {
            prompt: prev.prompt + event.promptTokens,
            completion: prev.completion + event.completionTokens,
            turns: prev.turns + 1,
          },
        },
      };
    }
    case "webFetched": {
      const read: ReadPage = {
        source: event.source,
        url: event.url,
        title: event.title,
        text: event.text,
        truncated: event.truncated,
      };
      return {
        ...state,
        reads: { ...state.reads, [event.turn]: [...(state.reads[event.turn] ?? []), read] },
      };
    }
    case "pointUpdated": {
      const points = [...state.points.filter((p) => p.id !== event.point.id), event.point].sort(
        (a, b) => a.id - b.id,
      );
      const isNew = !state.seenPoints.includes(event.point.id);

      // What changed, seat by seat. Only real transitions are logged: a
      // repeated update carrying an unchanged verdict is a re-send, not a
      // second change of mind, and logging it would pad the timeline with
      // noise the moment an update arrives twice.
      const before = state.points.find((p) => p.id === event.point.id)?.votes ?? {};
      const moves: VoteChange[] = [];
      for (const [delegate, v] of Object.entries(event.point.votes)) {
        const was = before[delegate]?.verdict ?? null;
        if (was !== v.verdict) {
          moves.push({
            round: v.round,
            delegate,
            from: was,
            to: v.verdict,
            reason: v.reason,
          });
        }
      }
      const voteLog = moves.length
        ? {
            ...state.voteLog,
            [event.point.id]: [...(state.voteLog[event.point.id] ?? []), ...moves],
          }
        : state.voteLog;
      return {
        ...state,
        points,
        tallies: { ...state.tallies, [event.point.id]: event.tally },
        seenPoints: isNew ? [...state.seenPoints, event.point.id] : state.seenPoints,
        voteLog,
        raised: isNew
          ? { ...state.raised, [event.point.raisedBy]: (state.raised[event.point.raisedBy] ?? 0) + 1 }
          : state.raised,
        conceded: nextConceded(points, event.point),
      };
    }
    case "actionRejected":
      // A rejected tool call used to surface only at the end of the turn, as
      // a line in the action log — so during the turn nothing explained why a
      // seat appeared to have done nothing.
      return {
        ...state,
        notices: [
          ...state.notices,
          {
            id: (noticeSeq += 1),
            text: `${event.delegate}: ${event.reason}`,
            tone: "warn" as NoticeTone,
          },
        ].slice(-60),
      };
    case "reviewOpened":
      return { ...state, reviews: { ...state.reviews, [event.pointId]: event.review } };
    case "reviewBallotCast": {
      const open = state.reviews[event.pointId];
      if (!open) return state;
      // Use the wire delegate — inventing one misattributes the vote.
      if (open.ballots.some((b) => b.delegate === event.delegate)) return state;
      return {
        ...state,
        reviews: {
          ...state.reviews,
          [event.pointId]: {
            ...open,
            ballots: [
              ...open.ballots,
              { delegate: event.delegate, reading: event.reading, reason: null },
            ],
          },
        },
      };
    }
    case "reviewClosed": {
      return {
        ...state,
        reviews: { ...state.reviews, [event.pointId]: event.review },
      };
    }
    case "adjournMoved": {
      // Only voting seats answer: the Summarizer abstains by construction.
      const voterIds = state.delegates
        .filter((d) => d.role !== "summarizer")
        .map((d) => d.id)
        .filter((id) => id !== event.by);
      return {
        ...state,
        adjourn: {
          by: event.by,
          reason: event.reason,
          round: event.round,
          yes: 1,
          no: 0,
          outstanding: voterIds,
          passed: null,
        },
      };
    }
    case "adjournBallotCast": {
      if (!state.adjourn) return state;
      const a = state.adjourn;
      if (!a.outstanding.includes(event.delegate)) return state;
      return {
        ...state,
        adjourn: {
          ...a,
          yes: event.confirm ? a.yes + 1 : a.yes,
          no: event.confirm ? a.no : a.no + 1,
          outstanding: a.outstanding.filter((id) => id !== event.delegate),
        },
      };
    }
    case "adjournDecided":
      return {
        ...state,
        adjourn: state.adjourn
          ? {
              ...state.adjourn,
              yes: event.yes,
              no: event.no,
              passed: event.passed,
              outstanding: [],
            }
          : null,
      };
    case "actionTaken": {
      const forSeat = state.acts[event.delegate] ?? {};
      // Conceded counts are derived from the board (nextConceded on
      // pointUpdated), so counting here double-counts. Track activity only.
      return {
        ...state,
        acts: {
          ...state.acts,
          [event.delegate]: {
            ...forSeat,
            [event.action]: (forSeat[event.action] ?? 0) + 1,
          },
        },
      };
    }
    case "pollOpened": {
      const polls = state.polls.filter((p) => p.id !== event.poll.id);
      return { ...state, polls: [...polls, event.poll] };
    }
    case "pollBallotCast": {
      // Ballots arrive one per voter while the poll is open; fold each into
      // the open poll so the tally visibly fills rather than appearing whole.
      return {
        ...state,
        polls: state.polls.map((p) =>
          p.id === event.pollId
            ? {
                ...p,
                ballots: p.ballots.some((b) => b.delegate === event.delegate)
                  ? p.ballots.map((b) =>
                      b.delegate === event.delegate
                        ? { ...b, choice: event.choice }
                        : b,
                    )
                  : [
                      ...p.ballots,
                      {
                        delegate: event.delegate,
                        choice: event.choice,
                        reason: null,
                        confidence: null,
                      },
                    ],
              }
            : p,
        ),
      };
    }
    case "pollClosed": {
      const polls = state.polls.filter((p) => p.id !== event.poll.id);
      return { ...state, polls: [...polls, event.poll] };
    }
    case "rosterUpdated":
      return event.delegates.length ? { ...state, delegates: event.delegates } : state;
    case "extensionBallotCast": {
      // The request is still open, or the answer no longer matters.
      if (!state.extension) return state;
      if (state.extension.ballots.some((b) => b.delegate === event.delegate)) return state;
      return {
        ...state,
        extension: {
          ...state.extension,
          ballots: [
            ...state.extension.ballots,
            {
              delegate: event.delegate,
              grant: event.grant,
              reason: event.reason,
            },
          ],
        },
      };
    }
    case "extensionProposed":
      return {
        ...state,
        extension: {
          rounds: event.rounds,
          reason: event.reason,
          round: event.round,
          ballots: [],
        },
      };
    case "extensionGranted":
      return {
        ...state,
        extension: null,
        maxRounds: event.total,
        extraBudget: event.budgetLeft,
      };
    case "extensionDeclined":
      return { ...state, extension: null };
    case "debateEnded":
      return {
        ...state,
        running: false,
        endedAt: Date.now(),
        live: null,
        verdict: event.verdict,
        intermission: null,
        extension: null,
        adjourn: null,
      };
    case "notice":
      return {
        ...state,
        notices: [
          ...state.notices,
          { id: (noticeSeq += 1), text: event.text, tone: event.tone },
        ].slice(-60),
      };
    case "failed":
      // A per-seat failure does not end the debate. Clearing `running` here
      // swapped Skip and Stop for Convene while the transcript was still
      // streaming, leaving no way to stop it at all.
      return {
        ...state,
        running: event.fatal ? false : state.running,
        // A fatal failure is an ending too, so the clock should stop rather
        // than keep counting a debate that is over.
        endedAt: event.fatal ? Date.now() : state.endedAt,
        live: event.fatal ? null : state.live,
        intermission: event.fatal ? null : state.intermission,
        extension: event.fatal ? null : state.extension,
        seatFailures: event.fatal
          ? state.seatFailures
          : [
              ...state.seatFailures,
              {
                delegate: event.message.includes(":")
                  ? (event.message.split(":")[0]?.trim() ?? "")
                  : "",
                message: event.message,
              },
            ].slice(-8),
        notices: [
          ...state.notices,
          { id: (noticeSeq += 1), text: event.message, tone: "error" as NoticeTone },
        ].slice(-60),
      };
    default:
      return state;
  }
}

export function useCouncil() {
  const [state, dispatch] = useReducer(reduce, initial);
  const notice = useCallback((text: string, tone: NoticeTone = "info") => {
    dispatch({ type: "notice", text, tone });
  }, []);

  // Adopt a run that started before this window existed. Without it, a reload
  // mid-debate showed an empty chamber while the engine kept spending tokens
  // on a debate nobody could see or stop.
  useEffect(() => {
    let cancelled = false;
    bridge
      .debateState()
      .then((snap) => {
        if (!cancelled && snap && (snap.points.length || snap.speeches.length)) {
          dispatch({ type: "snapshot", snapshot: snap });
        }
      })
      .catch(() => {
        /* nothing running, or no engine to ask */
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    bridge
      .subscribe((e) => dispatch(e))
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => notice("Could not attach to the engine event stream.", "error"));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notice]);

  const start = useCallback(
    async (config: Parameters<typeof bridge.startDebate>[0]) => {
      try {
        await bridge.startDebate(config);
      } catch (e) {
        notice(String(e), "error");
      }
    },
    [notice],
  );

  const stop = useCallback(async () => {
    try {
      await bridge.stopDebate();
    } catch (e) {
      notice(String(e), "error");
    }
  }, [notice]);

  const skip = useCallback(
    async () => {
      try {
        const done = await bridge.skipTurn();
        if (!done) notice("Nobody is mid-turn to skip.", "info");
      } catch (e) {
        notice(String(e), "error");
      }
    },
    [notice],
  );

  const say = useCallback(
    async (text: string) => {
      if (!text.trim()) return;
      try {
        await bridge.injectNote(text);
      } catch (e) {
        notice(String(e), "error");
      }
    },
    [notice],
  );

  const resolve = useCallback(
    async (approved: boolean, rounds: number) => {
      try {
        await bridge.resolveExtension(approved, rounds);
      } catch (e) {
        notice(String(e), "error");
      }
    },
    [notice],
  );

  const colorOf = useCallback(
    (id: string) => state.delegates.find((d) => d.id === id)?.color ?? "#6c8b84",
    [state.delegates],
  );

  const labelOf = useCallback(
    (id: string) => state.delegates.find((d) => d.id === id)?.label ?? id,
    [state.delegates],
  );

  const ordered = useMemo(() => state.speeches, [state.speeches]);
  return { state, ordered, start, stop, skip, say, resolve, colorOf, labelOf, notice };
}
