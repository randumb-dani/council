// The reducer that turns the engine's event stream into something renderable.
// The engine is authoritative: this never decides anything, it only accumulates.

import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import * as bridge from "./bridge";
import { seatGlyphs, seatSlots } from "./seats";
import type {
  BallotKind,
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
}

/** Longest a piece of activity text may be, since it is shown as it arrives. */
const ACTIVITY_MAX = 60;

/**
 * How much of a fetched page is kept for the preview.
 *
 * The engine may hand back up to `maxFetchChars` per fetch, several times per
 * turn, and those bounds are the operator's to raise — so unbounded `reads` was
 * the one accumulator that could outgrow everything else put together, while
 * being re-serialized into the library on every save. What the UI shows is the
 * citation: source, title, url, and enough text to see what was read.
 */
const READ_PREVIEW_CHARS = 2_000;

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
  /**
   * What the operator is being asked for their own seat, if anything.
   *
   * The engine parks until this is answered, so it is a prompt the user is
   * looking for — not a badge. It is also how a reload or a return from the
   * library lands back on an unanswered question instead of an empty chamber.
   */
  pending: PendingOperatorPrompt | null;
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

/**
 * The operator's own turn, or a ballot their seat must answer.
 *
 * Carried by the engine and mirrored here; the same shapes the engine emits
 * live and puts in the snapshot, so a remount renders the same prompt either
 * way.
 */
export type PendingOperatorPrompt =
  | { kind: "turn"; turn: number; delegate: string; label: string; round: number; brief: string }
  | {
      kind: "ballot";
      delegate: string;
      ballot: BallotKind;
      question: string;
      options: string[];
      pointId: number | null;
    };

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
  pending: null,
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
  /** The operator answered (or deliberately abandoned) their own prompt. */
  | { type: "promptCleared" }
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
        // A skipped turn is not a pending one. Leaving `pending` here left the
        // OperatorPanel on screen for the rest of the round, still reading "your
        // turn", still holding the speech the operator had typed — and every
        // submit answered "no turn is waiting for you", because the engine had
        // already cleared its own pending slot.
        pending: null,
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
      // A parked prompt survives the reload. The snapshot names the seat and
      // the question but carries no brief — the engine no longer holds one to
      // send — so the prompt is rebuilt with the board the snapshot already
      // carries, which is the same text the brief opens with.
      const pending = snap.pending ? promptFromSnapshot(snap) : null;
      return {
        ...initial,
        running: !snap.finished,
        // A finished snapshot is an ending too — stop the clock.
        endedAt: snap.finished ? Date.now() : null,
        // Carried over rather than reset to `initial`'s null. The library-save
        // effect mints a debate id the first time `startedAt` differs from the id it
        // last recorded, and with both sides null (`null !== null` is false) no id
        // was ever minted — so after any reload, adopt or phone handoff the debate
        // was never written to the library at all, and the session clock read null
        // as corroboration. Only set on the first adopt; a resync must not look
        // like a new debate.
        startedAt: state.startedAt ?? Date.now(),
        motion: snap.motion,
        round: snap.round,
        maxRounds: Math.max(snap.maxRounds ?? snap.round ?? 1, 1),
        delegates: snap.delegates ?? [],
        points: snap.points,
        tallies,
        polls: snap.polls,
        speeches: snap.speeches,
        seenPoints: snap.points.map((p) => p.id),
        pending,
        // The snapshot carries none of these, and `...initial` zeroed them. The
        // extension budget went to 0, so the Extend button was permanently disabled
        // and the header lost its `+N` — after a reload, on a debate configured with
        // four extra rounds and none spent. Usage, timing, the reference shelf and
        // the action log are accumulators too: a resync used to wipe the cost
        // figures the operator was reading. The open proposals (extension,
        // adjourn, reviews) and the vote history are likewise absent from the
        // snapshot, so a gap refetch mid-vote must not clear the question the
        // operator is answering or erase how the room voted to get there.
        extraBudget: state.extraBudget,
        extraTotal: state.extraTotal,
        reads: state.reads,
        usage: state.usage,
        timing: state.timing,
        acts: state.acts,
        extension: state.extension,
        adjourn: state.adjourn,
        reviews: state.reviews,
        voteLog: state.voteLog,
        seatFailures: state.seatFailures,
        skipped: state.skipped,
        // Keep the live turn if one is genuinely in flight, or the speech being
        // streamed blinks out of the live pane on every resync.
        live: state.live,
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
      // The page text is the one unbounded accumulator left. The engine caps a
      // turn at `maxFetchChars` (40k) times `maxFetchesPerTurn` (8), and both
      // are operator-settable, so a turn's reads alone could be 320k characters;
      // over a hundred-round plan that is the whole state, and it is
      // re-serialized on every library save. The *reference* is what the UI
      // shows and what makes a citation checkable — the text is a preview. So
      // the preview is capped and marked as clipped, and the full page stays
      // where it was: on the shelf, reachable by url.
      const read: ReadPage = {
        source: event.source,
        url: event.url,
        title: event.title,
        text: event.text.length > READ_PREVIEW_CHARS
          ? `${event.text.slice(0, READ_PREVIEW_CHARS)}\n…(preview truncated)`
          : event.text,
        truncated: event.truncated || event.text.length > READ_PREVIEW_CHARS,
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
    // The operator's own turn: the brief is what a model would have been shown,
    // so the human argues from the same record the council does.
    case "promptCleared":
      return { ...state, pending: null };
    case "yourTurn":
      return {
        ...state,
        pending: {
          kind: "turn",
          turn: event.turn,
          delegate: event.delegate,
          label: event.label,
          round: event.round,
          brief: event.brief,
        },
      };
    // A ballot their seat owes the room. It is held until answered, so the
    // prompt replaces rather than stacks with a turn prompt — the engine only
    // ever has one thing waiting on the operator.
    case "ballotAsked":
      return {
        ...state,
        pending: {
          kind: "ballot",
          delegate: event.delegate,
          ballot: event.kind,
          question: event.question,
          options: event.options,
          pointId: event.pointId,
        },
      };
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
        // Nothing is waiting on the operator once the debate is over; a
        // leftover prompt would be a dead button.
        pending: null,
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
        // Same for a prompt: a debate that ended fatally cannot still be asking
        // the operator a question, and the panel outliving it left the operator
        // looking at a prompt they could never answer.
        pending: event.fatal ? null : state.pending,
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

/**
 * Seat marks and ramp positions, derived from the roster.
 *
 * The mark replaces the avatar's old "first letter of the label", which put
 * Advocate and Arbiter both on an "A". The position is only a key for
 * `var(--seat-N, …)`, which the monochrome themes define and the coloured ones
 * do not.
 */
function seatMarks(state: CouncilState): {
  glyphOf: (id: string) => string;
  slotOf: (id: string) => number;
} {
  const glyphs = seatGlyphs(
    state.delegates.map((d) => ({ id: d.id, role: d.role })),
  );
  const slots = seatSlots(state.delegates);
  const glyphOf = (id: string) => glyphs[id] ?? (id.charAt(0).toUpperCase() || "?");
  const slotOf = (id: string) => slots[id] ?? 1;
  return { glyphOf, slotOf };
}

/**
 * The prompt a remounted window shows for a debate that was parked on the
 * operator.
 *
 * The snapshot names the seat and, for a ballot, the question — but not the
 * board text a turn's brief opened with, because the engine no longer holds
 * the brief to send. Rather than show an empty pane, the motion and the round
 * are given back: enough to say whose turn it is and that they owe the room
 * something, and the live event re-sends the full brief the moment the stream
 * attaches.
 */
function promptFromSnapshot(snap: bridge.DebateSnapshot): PendingOperatorPrompt | null {
  const p = snap.pending;
  if (!p) return null;
  const label = (id: string) => snap.delegates?.find((d) => d.id === id)?.label ?? id;
  if (p.kind === "turn") {
    return {
      kind: "turn",
      turn: p.turn,
      delegate: p.delegate,
      label: label(p.delegate),
      round: p.round,
      brief: `Round ${p.round}. The board as it stands is in the transcript — ${snap.points.length} point${snap.points.length === 1 ? "" : "s"} raised so far.`,
    };
  }
  return {
    kind: "ballot",
    delegate: p.delegate,
    ballot: p.ballot,
    question: p.question,
    options: p.options,
    pointId: p.pointId ?? null,
  };
}

export function useCouncil() {
  const [state, dispatch] = useReducer(reduce, initial);
  const notice = useCallback((text: string, tone: NoticeTone = "info") => {
    dispatch({ type: "notice", text, tone });
  }, []);
  // A prompt the operator answered is gone; the engine confirms it by moving
  // on, and until it does the panel would still offer a dead submit.
  const [submitting, setSubmitting] = useState(false);

  // Adopt a run that started before this window existed. Without it, a reload
  // mid-debate showed an empty chamber while the engine kept spending tokens
  // on a debate nobody could see or stop.
  useEffect(() => {
    let cancelled = false;
    bridge
      .debateState()
      .then((snap) => {
        // `pending` counts. The engine parks a human seat on its first turn, before
        // any point or speech exists, and the server replays nothing — so a reload
        // at that moment dropped the snapshot and the only prompt in existence with
        // it. The seat stayed parked forever with no way to answer: the debate was
        // bricked and the operator's unsent speech was gone.
        if (
          !cancelled &&
          snap &&
          (snap.points.length || snap.speeches.length || snap.pending || !snap.finished)
        ) {
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
    // The served stream marks gaps instead of hiding them: refetch the
    // snapshot, which is authoritative, and let the reducer adopt it.
    const onResync = () => {
      bridge
        .debateState()
        .then((snap) => {
          // Same bar as the mount adopt below: a parked human prompt exists
          // before any point or speech does, and dropping it here bricks the
          // seat the operator is trying to answer.
          if (
            !cancelled &&
            snap &&
            (snap.points.length || snap.speeches.length || snap.pending || !snap.finished)
          ) {
            dispatch({ type: "snapshot", snapshot: snap });
          }
        })
        .catch(() => {
          /* nothing running, or no engine to ask */
        });
    };
    window.addEventListener("council:resync", onResync);
    return () => {
      cancelled = true;
      unlisten?.();
      window.removeEventListener("council:resync", onResync);
    };
  }, [notice]);

  /**
   * Convene a council. Resolves true only once the engine has taken it.
   *
   * The caller needs that distinction: the setup holds a resume record which
   * must be cleared once it is safely with the engine, and cleared *not* at all
   * if the start was refused — a 409 or a validation failure means the operator
   * is about to fix the setup and try again, and a record thrown away in the
   * meantime is the one thing they cannot get back. Same shape as `answer`.
   */
  const start = useCallback(
    async (config: Parameters<typeof bridge.startDebate>[0]): Promise<boolean> => {
      try {
        await bridge.startDebate(config);
        // Adopt immediately rather than waiting for the stream: the engine emits
        // DebateStarted/RoundStarted/TurnStarted within microseconds of the POST
        // returning, and a subscriber that attaches after them has no live turn —
        // every token and heartbeat for it is then dropped, and the debate reads
        // as frozen until the first speech lands. The snapshot is authoritative
        // for "a debate is running"; the stream fills in the rest.
        const snap = await bridge.debateState().catch(() => null);
        if (snap) dispatch({ type: "snapshot", snapshot: snap });
        return true;
      } catch (e) {
        notice(String(e), "error");
        return false;
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

  /**
   * Broadcast a note to the room. Reports whether it landed.
   *
   * The return value is the contract the composer needs: it clears its box only
   * on `true`, so a note typed before Convene, after the verdict, or into a
   * server that has forgotten the debate stays in the box instead of vanishing.
   * The note is the one place an operator speaks to every model at once, so
   * losing it silently means the whole run is missing their input.
   */
  const say = useCallback(
    async (text: string): Promise<boolean> => {
      if (!text.trim()) return false;
      try {
        const landed = await bridge.injectNote(text);
        if (!landed) {
          notice(
            "There is no debate running, so nothing heard that. Press Convene first.",
            "warn",
          );
        }
        return landed;
      } catch (e) {
        notice(String(e), "error");
        return false;
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

  /**
   * Answer a taken seat's turn or ballot.
   *
   * The prompt is cleared on success and only on success: the engine refusing
   * the answer is the operator's cue to fix it, and clearing anyway would
   * throw away the turn they were about to correct.
   */
  /** Whether the engine took it. The operator's panel needs this to know whether to
   *  throw the draft away: a refusal ("cite needs `source`", "no turn is waiting for
   *  you") leaves the prompt open, and clearing on the way out meant the speech,
   *  the reasoning and every staged move were gone with nothing to show for it. */
  /**
   * Bound on how long the engine has to acknowledge an answer.
   *
   * No `serverBridge` fetch carries a timeout, and a phone walking out of Wi-Fi
   * range mid-request leaves the connection established with nothing coming back
   * — so `.finally` never ran, `submitting` stayed true, and both Skip and "say
   * it" were dead for the rest of the session. The engine was still parked and
   * waiting, so the debate could not continue and could not be abandoned.
   *
   * Identical in shape to the guard the extension modal already runs.
   */
  const SUBMIT_DEADLINE_MS = 15_000;
  /**
   * Identifies the in-flight answer, so a response that arrives after its
   * deadline cannot act on state that has moved on.
   *
   * This is the reason the deadline is safe at all. Without it, a slow answer
   * that lands two minutes later dispatches `promptCleared` and wipes whatever
   * prompt is on screen by then — which, after a retry, is the operator's
   * *second* turn. The stale response would destroy work it never saw.
   */
  const submitToken = useRef(0);
  const [submitStalled, setSubmitStalled] = useState(false);
  const answer = useCallback(
    async (payload: { turn: import("./types").HumanTurnSubmit } | { ballot: import("./types").HumanBallotSubmit }) => {
      submitToken.current += 1;
      const token = submitToken.current;
      setSubmitting(true);
      setSubmitStalled(false);
      let settled = false;
      // Whichever comes first, the panel becomes usable again. The timer is
      // cleared either way, so a fast answer does not leave one pending.
      const release = () => {
        if (settled) return;
        settled = true;
        window.clearTimeout(deadline);
        setSubmitting(false);
      };
      const deadline = window.setTimeout(() => {
        if (settled) return;
        settled = true;
        setSubmitting(false);
        setSubmitStalled(true);
        // Deliberately not an error: the engine may well have taken it, and the
        // server answers a duplicate with "no turn is waiting for you", which
        // settles the question. Saying so would also be a lie in the other
        // direction, and the operator cannot tell which happened.
        notice(
          "The local server has not acknowledged that yet. Your text is still here — send it again if the council is still waiting on you.",
          "warn",
        );
      }, SUBMIT_DEADLINE_MS);
      try {
        if ("turn" in payload) await bridge.submitTurn(payload.turn);
        else await bridge.submitBallot(payload.ballot);
        // A response for a superseded submission must not touch the current
        // prompt: after a retry it belongs to a turn already replaced.
        if (token !== submitToken.current) return false;
        dispatch({ type: "promptCleared" });
        return true;
      } catch (e) {
        if (token !== submitToken.current) return false;
        notice(String(e instanceof Error ? e.message : e), "error");
        return false;
      } finally {
        release();
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

  const marks = useMemo(() => seatMarks(state), [state.delegates]);

  return {
    state,
    start,
    stop,
    skip,
    say,
    resolve,
    answer,
    submitting,
    /** A past deadline released the last answer; the panel offers a resend. */
    submitStalled,
    colorOf,
    labelOf,
    glyphOf: marks.glyphOf,
    slotOf: marks.slotOf,
    notice,
  };
}
