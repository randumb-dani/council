// Talking to Rust.
//
// Everything here degrades gracefully: opened in a plain browser (`npm run
// dev`) rather than inside Tauri, the app runs against a scripted stand-in so
// the interface can be worked on without an endpoint to hand.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  DebateConfig,
  EndpointConfig,
  EngineEvent,
  Point,
  PointStatus,
  Poll,
  Speech,
  Tally,
  Verdict,
} from "./types";

export const EVENT_CHANNEL = "council://event";

export const inTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export async function startDebate(config: DebateConfig): Promise<void> {
  if (!inTauri()) return mock.start(config);
  await invoke("start_debate", { config });
}

export async function stopDebate(): Promise<void> {
  if (!inTauri()) return mock.stop();
  await invoke("stop_debate");
}

export async function skipTurn(): Promise<boolean> {
  if (!inTauri()) return false;
  return invoke<boolean>("skip_turn");
}

export async function injectNote(text: string): Promise<void> {
  if (!inTauri()) return;
  await invoke("inject_note", { text });
}

export async function resolveExtension(approved: boolean, rounds: number): Promise<string> {
  if (!inTauri()) return "preview only — no proposal is waiting";
  return invoke<string>("resolve_extension", { approved, rounds });
}

export interface DebateSnapshot {
  motion: string;
  round: number;
  maxRounds?: number;
  delegates?: { id: string; label: string; model: string; color: string; endpoint: string; role: string }[];
  tallies?: Record<number, Tally>;
  points: Point[];
  polls: Poll[];
  speeches: Speech[];
  finished: boolean;
}

export async function debateState(): Promise<DebateSnapshot | null> {
  if (!inTauri()) return null;
  return invoke<DebateSnapshot>("debate_state");
}

export async function listModels(endpoint: EndpointConfig): Promise<string[]> {
  if (!inTauri()) return [];
  return invoke<string[]>("list_models", { endpoint });
}

export async function subscribe(onEvent: (e: EngineEvent) => void): Promise<() => void> {
  if (!inTauri()) return mock.subscribe(onEvent);
  return listen<EngineEvent>(EVENT_CHANNEL, (event) => onEvent(event.payload));
}

// ---------------------------------------------------------------------------
// Scripted stand-in, so `npm run dev` in a browser shows a working council.
// ---------------------------------------------------------------------------

const STATUS_OF = (score: number, votes: number): PointStatus => {
  if (score > 0) return "carried";
  if (score < 0) return "fell";
  return votes > 0 ? "stalemate" : "open";
};

interface MockRead {
  source: string;
  url: string;
  title: string;
  text: string;
}

const SCRIPT: {
  text: string;
  thought?: string;
  call: [string, Record<string, unknown>];
  /** A page the seat reads before acting, as the engine would deliver it. */
  read?: MockRead;
}[] = [
  {
    text: "Opening. The core claim is that structured adjudication beats free-form debate.",
    call: ["make_point", { text: "A scored board produces more reliable conclusions than prose." }],
  },
  {
    text: "I want the strongest version of that before I attack it. Steelmanning it: if every claim is forced onto a board and rated, the council cannot hide behind eloquence.",
    thought: "The claim is procedural, not empirical. Steelman first — attack the strongest version, not the slogan.",
    call: ["steelman", { point: 1, restatement: "Scoring forces claims into the open where they can be refuted." }],
  },
  {
    text: "A second claim, which I expect to lose: that scoring is enough on its own, with no need for anyone to re-read the argument.",
    call: ["make_point", { text: "Scoring alone is sufficient; nobody needs to re-read the argument." }],
  },
  {
    text: "That one falls. Legibility is not a substitute for reading, and the claim says it is — so two seats against is not a close thing.",
    call: ["vote", { point: 2, verdict: "lose", reason: "reading is the substance, scoring is the bookkeeping" }],
  },
  {
    text: "Agreed on the second. It claims scoring substitutes for the argument, which is the one thing scoring cannot do.",
    call: ["vote", { point: 2, verdict: "lose", reason: "the claim conflates the record with the reasoning" }],
  },
  {
    text: "That concedes the hard part. Scoring only helps if the ratings are honest, and there is no mechanism here for that — models vote the way they were prompted to.",
    call: ["vote", { point: 1, verdict: "lose", reason: "the ratings are not independent evidence" }],
  },
  {
    text: "Fair. But the board makes disagreement legible, which is most of the value even with correlated judges.",
    call: ["vote", { point: 1, verdict: "win", reason: "legibility is the point" }],
  },
  {
    // The reference shelf, so the expandable read row can be seen in a browser
    // preview without a model server behind it.
    text: "Before I go further I want the definition straight, not the popular one. Reading first, then voting.",
    read: {
      source: "wikipedia",
      url: "https://en.wikipedia.org/wiki/Adjudication",
      title: "Adjudication",
      text: "Adjudication is the formal process of reaching a binding decision. In law it is the settling of a dispute by a court or other deciding body; more broadly it is the activity of making a choice between options, where the decision is understood to be final for the parties.",
    },
    call: ["vote", { point: 1, verdict: "win", reason: "the process is what makes a result binding" }],
  },
  {
    text: "One question we can actually settle by counting rather than arguing.",
    call: ["poll", { question: "Does the board change the outcome?", options: ["yes", "no"] }],
  },
  {
    text: "Yes, decisively. Two of us now agree it does, and one dissent is on the record rather than buried.",
    call: ["vote", { point: 1, verdict: "win" }],
  },
  {
    // The Arbiter's instrument, so the ruling panel is visible in a preview.
    text: "The record settles P1. I am not weighing it again — I am ruling on it.",
    call: ["rule", { point: 1, ruling: "carries", reason: "the objection was answered in the record" }],
  },
  {
    text: "I will concede the narrower version. The board does not make models independent, but it does make them accountable.",
    call: ["concede", { point: 1, reason: "the strong version does not survive" }],
  },
  {
    text: "The record is now sufficient. I would like it noted.",
    call: ["adjourn", { reason: "the exchange has exhausted itself" }],
  },
];

const mock = (() => {
  // A Set, not a single slot. `subscribe` is async, so React StrictMode's
  // mount → cleanup → mount ran the first unsubscribe *after* the second
  // subscribe had installed its callback — nulling the listener and leaving
  // the whole browser preview dead on "idle".
  const listeners = new Set<(e: EngineEvent) => void>();
  let run = 0;
  let currentRoster: string[] = [];

  const emit = (e: EngineEvent) => {
    for (const l of [...listeners]) l(e);
  };
  const wait = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

  return {
    subscribe(onEvent: (e: EngineEvent) => void) {
      listeners.add(onEvent);
      return () => {
        listeners.delete(onEvent);
      };
    },
    stop() {
      run += 1;
      // Used to emit nothing, so `running` stayed true forever and the preview
      // could only be recovered with a reload.
      emit({ type: "failed", message: "stopped.", fatal: true });
    },
    async start(config: DebateConfig) {
      // Cancel any previous preview run silently: emitting a fatal
      // "stopped." here flashed an error on every fresh Convene.
      run += 1;
      const mine = run;
      let turn = 0;
      let point = 0;
      let pollId = 0;
      let roundsRun = 0;
      let adjournedBy: string | null = null;
      let adjourned = false;
      currentRoster = config.delegates.map((d) => d.id);
      // Real Point objects, so a vote mutates the point instead of replacing
      // it: the old mock rebuilt it from empty and wiped its text and every
      // earlier vote while the tally still reported them.
      const board = new Map<number, Point>();

      emit({
        type: "debateStarted",
        motion: config.motion,
        rounds: config.maxRounds,
        maxExtraRounds: config.maxExtraRounds,
        images: (config.images ?? []).map((im) => ({ id: im.id, name: im.name })),
        delegates: config.delegates.map((d) => ({
          id: d.id,
          label: d.label,
          model: d.model,
          color: d.color,
          endpoint: d.endpointId,
          role: d.seatRole,
        })),
      });

      // The summarizer watches: it lays each round out but never argues.
      const speakers = config.delegates.filter((d) => d.seatRole !== "summarizer");
      const summarizer = config.delegates.find((d) => d.seatRole === "summarizer");

      for (let round = 1; round <= config.maxRounds; round += 1) {
        if (mine !== run) return;
        emit({ type: "roundStarted", round });
        for (const seat of speakers) {
          if (mine !== run) return;
          turn += 1;
          const id = turn;
          const step = SCRIPT[(id - 1) % SCRIPT.length];
          emit({
            type: "turnStarted",
            turn: id,
            delegate: seat.id,
            label: seat.label,
            color: seat.color,
            model: seat.model,
            round,
          });
          if (step.thought) {
            for (const word of step.thought.split(/(?<=\s)/)) {
              await wait(12);
              emit({ type: "reasoning", turn: id, text: word });
            }
          }
          const began = Date.now();
          let tokens = 0;
          for (const word of step.text.split(/(?<=\s)/)) {
            if (mine !== run) return;
            await wait(18);
            tokens += 1;
            emit({ type: "token", turn: id, text: word });
            // Without these the live clock and the "thinking" pill never
            // exercised their only code path in browser preview.
            if (tokens % 4 === 0) {
              emit({
                type: "turnProgress",
                turn: id,
                delegate: seat.id,
                elapsedSecs: Math.round((Date.now() - began) / 1000),
                tokens,
              });
            }
          }
          // A read lands before the act it informs, as it does in the engine.
          if (step.read) {
            const chars = step.read.text.length;
            emit({
              type: "webFetched",
              turn: id,
              delegate: seat.id,
              source: step.read.source,
              url: step.read.url,
              title: step.read.title,
              text: step.read.text,
              truncated: false,
            });
            emit({
              type: "actionTaken",
              turn: id,
              delegate: seat.id,
              action: "read",
              detail: `${seat.label} read ${step.read.source} — ${step.read.title} (${chars} chars)`,
            });
          }
          const [tool, args] = step.call;
          // Said before the call, as the engine says it: this is the stretch
          // where a slow model looks like a hung one, and the preview is the
          // only place the dots are ever exercised without a real server.
          await wait(140);
          emit({
            type: "turnActivity",
            turn: id,
            delegate: seat.id,
            what: `calling \`${tool}\``,
          });
          if (step.read) await wait(320);
          const action = String(args.text ?? args.restatement ?? args.reason ?? "");
          emit({ type: "actionTaken", turn: id, delegate: seat.id, action: tool, detail: action });

          // A ruling settles the point outright, so the mock has to do it or the
          // board would keep showing a tally the record has overruled.
          if (tool === "rule") {
            const target = Number(args.point ?? 1);
            const existing = board.get(target);
            if (existing) {
              const carries = String(args.ruling) === "carries";
              const next = {
                ...existing,
                ruling: { by: seat.id, round, carries, reason: String(args.reason ?? "") },
              };
              board.set(target, next);
              emit({
                type: "pointUpdated",
                point: next,
                tally: tally(next),
                resolved: true,
              });
            }
          }

          if (tool === "make_point") {
            point += 1;
            const p = { ...emptyPoint(point, seat.id, action), round };
            board.set(point, p);
            emit({ type: "pointUpdated", point: p, tally: tally(p), resolved: false });
          } else if (tool === "vote" || tool === "concede") {
            const target = Number(args.point ?? 1);
            const existing = board.get(target);
            if (existing) {
              const v =
                tool === "concede"
                  ? "lose"
                  : String(args.verdict ?? "win") === "lose"
                    ? "lose"
                    : String(args.verdict ?? "win") === "stalemate"
                      ? "stalemate"
                      : "win";
              const verdict = v as Verdict;
              // Mutate in place: a fresh point here wiped the claim's text and
              // every vote already cast on it.
              const p: Point = {
                ...existing,
                votes: {
                  ...existing.votes,
                  [seat.id]: { delegate: seat.id, verdict, reason: null, round },
                },
                concededBy:
                  tool === "concede"
                    ? { ...existing.concededBy, [seat.id]: String(args.reason ?? "") }
                    : existing.concededBy,
              };
              board.set(target, p);
              emit({
                type: "pointUpdated",
                point: p,
                tally: tally(p),
                resolved: tool === "concede",
              });
            }
          } else if (tool === "poll") {
            pollId += 1;
            const poll = {
              id: pollId,
              question: String(args.question ?? "?"),
              options: (args.options as string[]) ?? [],
              openedBy: seat.id,
              round,
              ballots: [],
              closed: false,
            };
            emit({ type: "pollOpened", poll });
            // One ballot per other speaker, so the open poll visibly fills.
            const ballots = speakers
              .filter((d) => d.id !== seat.id)
              .map((d) => ({ delegate: d.id, choice: poll.options[0], reason: null, confidence: null }));
            for (const b of ballots) {
              await wait(120);
              emit({
                type: "pollBallotCast",
                pollId: poll.id,
                delegate: b.delegate,
                choice: b.choice,
                result: poll.options.map((o) => [
                  o,
                  ballots.filter((x) => x.choice === o).length,
                ]),
              });
            }
            const closed = { ...poll, ballots, closed: true };
            emit({ type: "pollClosed", poll: closed, outcome: poll.options[0] ?? null });
          } else if (tool === "adjourn") {
            const reason = String(args.reason ?? "the record is sufficient");
            emit({ type: "adjournMoved", by: seat.id, reason, round });
            let yes = 1;
            let no = 0;
            for (const voter of speakers.filter((d) => d.id !== seat.id)) {
              await wait(120);
              const agree = voter.id === seat.id ? false : yes <= no + 1;
              if (agree) yes += 1;
              else no += 1;
              emit({
                type: "adjournBallotCast",
                delegate: voter.id,
                confirm: agree,
                result: [
                  ["yes", yes],
                  ["no", no],
                ],
              });
            }
            const passed = yes * 2 > speakers.length;
            emit({ type: "adjournDecided", passed, yes, no, reason });
            emit({
              type: "notice",
              text: passed
                ? `council agreed to adjourn (${yes}-${no})`
                : `adjournment voted down (${yes}-${no}) - debate continues`,
              tone: passed ? "info" : "warn",
            });
            adjournedBy = passed ? seat.id : null;
            if (passed) {
              adjourned = true;
              // Emit the speech for the mover before leaving, then stop the
              // outer round loop too (break alone only left the seat loop).
            }
          }

          emit({
            type: "speechFinished",
            turn: id,
            speech: {
              id,
              delegate: seat.id,
              round,
              text: step.text,
              reasoning: step.thought ?? "",
              actions: [action],
              phase: "done",
            },
          });
          if (adjourned) break;
        }

        roundsRun = round;
        if (adjourned) break;

        // The intermission, canned: the round is laid out, no extra rounds asked.
        if (mine !== run) return;
        if (summarizer) {
          turn += 1;
          emit({ type: "intermissionStarted", round });
          emit({
            type: "turnStarted",
            turn,
            delegate: summarizer.id,
            label: summarizer.label,
            color: summarizer.color,
            model: summarizer.model,
            round,
          });
          await wait(150);
          emit({
            type: "speechFinished",
            turn,
            speech: {
              id: turn,
              delegate: summarizer.id,
              round,
              text: `Round ${round} laid out: the board moved, nothing is buried, and the next round knows what to settle.`,
              reasoning: "",
              actions: [`round ${round} summary`],
              phase: "done",
            },
          });
        }
      }

      if (mine !== run) return;
      const carried = [...board.values()]
        .map((p) => ({ p, t: tally(p) }))
        .filter(({ t }) => t.status === "carried")
        .map(({ p, t }) => ({
          id: p.id,
          text: p.text,
          raisedBy: p.raisedBy,
          score: t.score,
          wins: t.wins,
          losses: t.losses,
          stalemates: t.stalemates,
          ruling: null,
        }));
      emit({
        type: "debateEnded",
        verdict: {
          motion: config.motion,
          // Rounds actually spoken, not the plan: the mock used to report the
          // maximum even after adjourning in round one.
          rounds: roundsRun,
          endedHow: adjournedBy ? "adjourned" : "complete",
          carried,
          fell: [],
          stalemate: [],
          withdrawn: [],
          crux: carried[0] ?? null,
          summary: "Demo debate — no models were contacted.",
          adjournedBy,
          finalSummary:
            "The board experiment ends as it began: legibility wins, independence is still an open question, and nobody was polled twice.",
          conclusion: {
            agreed: true,
            answer: "A scored board is the better instrument, though it does not make the judges independent.",
            lean: null,
            tally: "1 carried, 0 fell, 0 deadlocked, 0 undecided",
          },
        },
      });
    },
  };

  /**
   * The same rule the engine uses, computed from the point itself rather than
   * from parallel counters — the parallel maps drifted, and a drifted tally
   * made the board card contradict the seat pills on the same screen.
   */
  function tally(p: Point): Tally {
    const roster = configRoster();
    const ratings: Record<string, Verdict> = {};
    let wins = 0;
    let losses = 0;
    let stalemates = 0;
    for (const id of roster) {
      const v =
        p.votes[id]?.verdict ?? (id === p.raisedBy && !p.withdrawn ? "win" : undefined);
      if (!v) continue;
      ratings[id] = v;
      if (v === "win") wins += 1;
      else if (v === "lose") losses += 1;
      else stalemates += 1;
    }
    const contested = roster.some((id) => id !== p.raisedBy && p.votes[id] !== undefined);
    // Mirror the engine: withdrawn zeroes the board, a ruling settles it.
    if (p.withdrawn) {
      return {
        wins,
        losses,
        stalemates,
        abstentions: Math.max(0, roster.length - Object.keys(ratings).length),
        score: 0,
        status: "withdrawn",
        contested,
        ratings,
      };
    }
    if (p.ruling) {
      const score = wins - losses;
      return {
        wins,
        losses,
        stalemates,
        abstentions: Math.max(0, roster.length - Object.keys(ratings).length),
        score,
        status: p.ruling.carries ? "carried" : "fell",
        contested,
        ratings,
      };
    }
    const score = wins - losses;
    return {
      wins,
      losses,
      stalemates,
      abstentions: Math.max(0, roster.length - Object.keys(ratings).length),
      score,
      status: STATUS_OF(score, Object.keys(ratings).length),
      contested,
      ratings,
    };
  }

  function configRoster(): string[] {
    return currentRoster;
  }

  function emptyPoint(id: number, delegate: string, text: string, round = 1): Point {
    return {
      id,
      text: text || "(demo point)",
      raisedBy: delegate,
      round,
      withdrawn: false,
      withdrawnReason: null,
      beats: [],
      votes: {},
      rebuttals: [],
      steelmen: [],
      evidence: [],
      concededBy: {},
      review: null,
    ruling: null,
    };
  }
})();
