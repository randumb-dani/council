/**
 * Resuming an unfinished debate.
 *
 * Run with `npm run test:web`.
 *
 * The rule worth pinning is what a resume is *not*: it does not restore the old
 * board. The library stores state and model names and deliberately stores no
 * config and no keys, so the board that the old debate built was only ever in
 * Rust's memory — and rehydrating a JSON copy of it would be inventing a record
 * and calling it the same one. What carries over is the argument, as prose, in
 * the one field a prompt can hold.
 */

import assert from "node:assert/strict";
import {
  applyResumeDraft,
  clearResumeDraft,
  SETUP_MAX_ROUNDS,
  draftFrom,
  peekResumeDraft,
  recordConsumed,
  recordForResume,
  saveResumeDraft,
  takeResumeDraft,
} from "./resume.ts";
import type { DebateConfig, DelegateBrief, SeatRole } from "./types.ts";
import type { CouncilState } from "./useCouncil.ts";

// A localStorage stand-in, since this runs under bare node. Small and explicit
// beats a dependency: the module's contract is only "clears on read".
let store = new Map<string, string>();
(globalThis as { window?: unknown }).window = {
  localStorage: {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
  },
};

const roster: DelegateBrief[] = [
  { id: "a", label: "Advocate", model: "m-a", color: "#65ccc6", endpoint: "e", role: "proponent" },
  { id: "b", label: "Critic", model: "m-b", color: "#4aa8a1", endpoint: "e", role: "opponent" },
  { id: "j", label: "Arbiter", model: "m-j", color: "#c9b8f2", endpoint: "e", role: "judge" },
  { id: "s", label: "Summarizer", model: "m-s", color: "#8ad9d3", endpoint: "e", role: "summarizer" },
];

function state(over: Partial<CouncilState> = {}): CouncilState {
  return {
    running: false,
    round: 2,
    maxRounds: 3,
    motion: "Roses outrank lilies",
    images: [],
    delegates: roster,
    speeches: [
      { id: 1, delegate: "a", round: 1, text: "First: the stem count settles it.", reasoning: "", actions: [], phase: "done" },
      { id: 2, delegate: "b", round: 1, text: "That is unfalsifiable as stated.", reasoning: "", actions: [], phase: "done" },
      { id: 3, delegate: "a", round: 2, text: "Then: petals last longer.", reasoning: "", actions: [], phase: "done" },
    ],
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
    startedAt: null,
    endedAt: null,
    ...over,
  } as CouncilState;
}

const summary = {
  motion: "Roses outrank lilies",
  rounds: 2,
  roundsConcluded: 2,
  carried: 0,
  fell: 0,
  stalemate: 0,
  withdrawn: 0,
  points: 0,
  speeches: 3,
  unfinished: true,
  endedHow: null,
  closedBy: null,
  seats: roster.map((d) => ({ label: d.label, model: d.model, role: d.role })),
  answer: null,
};

// --- the draft ---------------------------------------------------------------

const draft = draftFrom("deb-1", state(), summary);
assert.ok(draft, "an unfinished debate with speeches should be resumable");
assert.equal(draft.motion, "Roses outrank lilies");
assert.equal(draft.delegates.length, 4);
assert.deepEqual(
  draft.delegates.map((d) => d.role as SeatRole),
  ["proponent", "opponent", "judge", "summarizer"],
  "the roster must come back with its roles intact",
);
assert.ok(draft.record.includes("petals last longer"), "the record must carry the speeches");
assert.ok(draft.record.includes("R2 Advocate"), "speeches must say who said them and when");

// No speeches: nothing to continue, so no button.
assert.equal(
  draftFrom("deb-2", state({ speeches: [] }), summary),
  null,
  "an empty debate must not offer a resume",
);

// --- truncated records keep the end, not the beginning -----------------------

const huge = state({
  speeches: Array.from({ length: 200 }, (_, i) => ({
    id: i + 1,
    delegate: "a",
    round: 1,
    text: `speech ${i} ${"padding ".repeat(120)}`,
    reasoning: "",
    actions: [],
    phase: "done" as const,
  })),
});
const record = recordForResume(huge, { ...summary, speeches: 200 });
assert.ok(record.length < 12000, `the record must be capped, got ${record.length} chars`);
assert.ok(
  record.includes("speech 199"),
  "a truncated record must keep the newest speeches — that is where the thread was cut",
);
assert.ok(record.includes("omitted"), "the omission has to be stated, not silent");

// --- folding into a config ----------------------------------------------------

const base: DebateConfig = {
  motion: "something else",
  extraInstructions: "always answer in British English",
  endpoints: [
    {
      id: "ep-1",
      name: "Local",
      provider: "openaiCompatible",
      baseUrl: "http://127.0.0.1:8888",
      apiKey: null,
      headers: {},
      timeoutSecs: 300,
      reasoningWire: "serverDefault",
      reasoningEffort: null,
      thinking: null,
      nativeTools: true,
      stream: true,
      allowInvalidCerts: false,
      includeUsage: true,
    },
  ],
  delegates: [
    {
      id: "old-seat",
      label: "Old label",
      model: "old-model",
      endpointId: "ep-old",
      color: "#123456",
      seatRole: "proponent",
      role: null,
      persona: null,
      temperature: 0.9,
    },
  ],
  maxRounds: 1,
  autoAdjourn: false,
  closingStatements: true,
  decision: { provider: "seat", baseUrl: "", apiKey: null, model: "", timeoutSecs: 30 },
  maxExtraRounds: 0,
  allowWeb: false,
  maxFetchesPerTurn: 3,
  maxFetchChars: 6000,
  images: [],
};

const resumed = applyResumeDraft(base, draft!);
assert.equal(resumed.motion, "Roses outrank lilies", "the motion comes back");
assert.equal(resumed.delegates.length, 4, "the whole roster comes back");
assert.deepEqual(
  resumed.delegates.map((d) => d.label),
  ["Advocate", "Critic", "Arbiter", "Summarizer"],
);
assert.deepEqual(
  resumed.delegates.map((d) => d.seatRole),
  ["proponent", "opponent", "judge", "summarizer"],
  "roles must survive, or the council cannot convene",
);
// Models blank: a server that is not running the old one now would fail on the
// first turn, and the operator is the one who knows what it is serving.
assert.deepEqual(
  resumed.delegates.map((d) => d.model),
  ["", "", "", ""],
  "models must be chosen again, not pre-selected from a stale debate",
);
// And nothing from the old setup leaks: no stale endpoint, no stale tuning.
assert.deepEqual(
  [...new Set(resumed.delegates.map((d) => d.endpointId))],
  ["ep-1"],
  "every delegate must point at a server that exists now",
);
assert.deepEqual(
  resumed.delegates.map((d) => d.temperature),
  [null, null, null, null],
  "the old setup's tuning must not carry over",
);
// Its own field. It used to overwrite `extraInstructions`, which is the operator's
// standing instructions and is saved — so a resume destroyed them, and the
// replacement was persisted, meaning the *next* unrelated debate was still handed a
// dead debate's transcript.
assert.equal(
  resumed.resumeRecord,
  draft.record,
  "the record must travel in its own field",
);
assert.equal(
  base.extraInstructions,
  "always answer in British English",
  "the operator's own instructions must survive a resume untouched",
);
assert.equal(
  resumed.extraInstructions,
  "always answer in British English",
  "and must not be replaced by the record",
);
assert.ok(resumed.maxRounds > summary.rounds, "a cut-short debate should afford another round");
assert.equal(
  resumed.maxRounds,
  Math.min(summary.rounds + 1, SETUP_MAX_ROUNDS),
  "and stay inside the setup panel's own slider ceiling, or the control cannot show it",
);
// The old colours are kept where the new roster has one at that position: they
// are cosmetic and keep the seat recognisable.
assert.equal(resumed.delegates[0]!.color, "#123456");
assert.equal(base.delegates[0]!.endpointId, "ep-old", "the old config must not be mutated");

// --- a draft must not be able to blank the window -----------------------------

// `applyResumeDraft` is reached from a state initializer, i.e. during render, and
// there is no error boundary in this app. The old check validated `motion` and
// nothing else, so any of these reached `draft.delegates.map(...)` and threw.
store.set("council.resume.v1", JSON.stringify({ motion: "x" }));
assert.equal(takeResumeDraft(), null, "a draft with no delegates is discarded, not thrown");
store.set(
  "council.resume.v1",
  JSON.stringify({ motion: "x", delegates: [], record: "", roundsSpoken: 0, fromDebateId: "d" }),
);
assert.equal(takeResumeDraft(), null, "an empty roster is discarded");
store.set(
  "council.resume.v1",
  JSON.stringify({
    motion: "x",
    delegates: [{ label: "Advocate", role: "wizard", model: "m" }],
    record: "",
    roundsSpoken: 1,
    fromDebateId: "d",
  }),
);
assert.equal(
  takeResumeDraft(),
  null,
  "a seat role this build no longer knows must be discarded, not passed to the engine",
);
store.set(
  "council.resume.v1",
  JSON.stringify({
    motion: "x",
    delegates: [{ label: "Advocate", role: "proponent", model: 7 }],
    record: "",
    roundsSpoken: 1,
    fromDebateId: "d",
  }),
);
assert.equal(takeResumeDraft(), null, "a hand-edited model name is discarded");
store.set("council.resume.v1", JSON.stringify({ motion: "x", roundsSpoken: "two" }));
assert.equal(takeResumeDraft(), null, "a non-numeric round count is discarded");

// Peeking must not consume: React double-invokes `useState` initializers under
// StrictMode and keeps the second, so the discarded first mount read *and deleted*
// the draft — which is why resume silently did nothing in dev.
assert.equal(saveResumeDraft(draft!), true);
assert.ok(peekResumeDraft(), "a draft is peekable");
assert.ok(peekResumeDraft(), "peeking twice is fine — reading must not consume");
assert.ok(takeResumeDraft(), "and it is still there to consume");

// --- the handoff -------------------------------------------------------------

clearResumeDraft();
assert.equal(peekResumeDraft(), null);
assert.equal(saveResumeDraft(draft!), true);
assert.ok(peekResumeDraft(), "a saved draft is peekable without consuming it");
assert.ok(takeResumeDraft(), "taking it returns the draft");
assert.equal(
  takeResumeDraft(),
  null,
  "a resume is a one-shot handoff: a second read must not re-seed the setup",
);
assert.equal(peekResumeDraft(), null, "and the draft is gone from storage");

// A corrupt draft must not brick the setup panel.
store.set("council.resume.v1", "{not json");
assert.equal(takeResumeDraft(), null, "a corrupt draft is discarded, not thrown");
store.set("council.resume.v1", JSON.stringify({ nope: true }));
assert.equal(takeResumeDraft(), null, "a draft with no motion is discarded");

// --- taken seats stay taken ----------------------------------------------------
const held = state({
  delegates: roster.map((d, i) => (i === 0 ? { ...d, human: true } : d)),
});
const heldDraft = draftFrom("deb-held", held, summary);
assert.ok(heldDraft?.delegates[0]?.human === true, "the draft carries the taken seat");
const heldBack = applyResumeDraft(base, heldDraft!);
assert.equal(
  heldBack.delegates[0]?.human,
  true,
  "a resumed seat stays held — silently automating it would hand the operator's arguments to a model",
);
// Drafts written before the field existed still validate, and read as
// automatic, as they always did.
store.set(
  "council.resume.v1",
  JSON.stringify({
    motion: "x",
    delegates: [{ label: "Advocate", role: "proponent", model: "m" }],
    record: "",
    roundsSpoken: 1,
    fromDebateId: "d",
  }),
);
const legacy = takeResumeDraft();
assert.ok(legacy, "a draft without the field is still a draft");
assert.equal(
  applyResumeDraft(base, legacy!).delegates[0]?.human,
  false,
  "and it resumes automatic, as it always did",
);

// --- the record does not outlive its debate -----------------------------------
//
// The bug this pins: the record is written into the *saved* setup by
// `applyResumeDraft` and nothing ever cleared it, so it came back on the next
// visit to the setup panel and rode into every unrelated motion afterwards —
// where `prompt.rs` puts it in every delegate's opening brief and tells them
// they are resuming a debate they never heard of. Clearing at convene is the
// fix; this is the order that has to hold.
const applied = applyResumeDraft(base, draft!);
assert.ok(applied.resumeRecord, "the record is present while the setup is being prepared");

const cleared = recordConsumed(applied);
assert.equal(
  cleared.resumeRecord,
  null,
  "once the engine has the config the record must be gone from the setup",
);
// Everything else about the resumed setup is untouched by the clear.
assert.equal(cleared.motion, applied.motion, "the motion survives the clear");
assert.deepEqual(
  cleared.delegates.map((d) => d.label),
  applied.delegates.map((d) => d.label),
  "so does the roster",
);
assert.equal(cleared.maxRounds, applied.maxRounds, "and the round budget");
assert.notEqual(
  cleared,
  applied,
  "the old config must not be mutated in place — the start call still holds it",
);
assert.ok(
  applied.resumeRecord,
  "the original keeps its record: the POST that carried it has already been built",
);

// Idempotent, and identity-preserving when there is nothing to clear, so the
// autosave dependency does not churn on every fresh setup.
assert.equal(recordConsumed(cleared), cleared, "clearing twice is a no-op");
assert.equal(
  recordConsumed(base),
  base,
  "a setup with no record is returned as-is, not a fresh copy",
);

console.log("resume: draft, cap, handoff, one-shot record — all pinned");