/**
 * The library's arithmetic and its two export formats.
 *
 * Run with `npm run test:web`. The Markdown is the only artifact a debate ever
 * leaves by hand, so a stray `|` in a claim silently corrupting a table column
 * is exactly the kind of thing that should be caught by a test rather than
 * discovered after the fact.
 */

import assert from "node:assert/strict";
import { stripForSave, slug, summarise, toJson, toMarkdown, type SavedDebate } from "./debates.ts";
import type { CouncilState } from "./useCouncil.ts";

function state(over: Partial<CouncilState> = {}): CouncilState {
  const base: CouncilState = {
    running: false,
    round: 3,
    maxRounds: 3,
    motion: "Roses outrank lilies as a gift flower",
    images: [],
    delegates: [
      { id: "a", label: "Advocate", model: "m-a", endpoint: "e", color: "#f55", role: "proponent" },
      { id: "b", label: "Critic", model: "m-b", endpoint: "e", color: "#5af", role: "opponent" },
      { id: "s", label: "Summarizer", model: "m-s", endpoint: "e", color: "#5f5", role: "summarizer" },
    ],
    speeches: [],
    live: null,
    points: [],
    tallies: {},
    polls: [],
    notices: [],
    verdict: null,
    skipped: [],
    acts: {},
    raised: { a: 1 },
    conceded: {},
    seenPoints: [1],
    intermission: null,
    extension: null,
    extraBudget: 0,
    extraTotal: 0,
    reviews: {},
    adjourn: null,
    seatFailures: [],
    reads: {},
  };
  return { ...base, ...over };
}

const point = (id: number, text: string, extra = {}) => ({
  id,
  text,
  raisedBy: "a",
  round: 1,
  withdrawn: false,
  withdrawnReason: null,
  beats: [],
  votes: {},
  rebuttals: [],
  steelmen: [],
  evidence: [],
  concededBy: {},
  review: null,
  ...extra,
});

const tally = (over = {}) => ({
  wins: 1,
  losses: 0,
  stalemates: 0,
  abstentions: 0,
  score: 1,
  status: "carried" as const,
  contested: true,
  ratings: { a: "win" as const, b: "win" as const },
  ...over,
});

const speech = (id: number, delegate: string, round: number, text: string, actions: string[] = []) => ({
  id,
  delegate,
  round,
  text,
  reasoning: "",
  actions,
  phase: "done" as const,
});

/**
 * Cells in a Markdown table row, counting only pipes that actually separate
 * cells. A claim containing "|" is escaped to "\\|", and a naive split counts
 * that escape as a column — which is exactly the bug this helper exists to
 * avoid testing for.
 */
function cells(row: string): number {
  return (row.replace(/\\\|/g, "\u0000").match(/\|/g) ?? []).length;
}

function saved(s: CouncilState): SavedDebate {
  const clean = stripForSave(s);
  return { id: "d1", savedAt: 1_700_000_000_000, summary: summarise(clean), state: clean };
}

// ---------------------------------------------------------------------------

const tests: [string, () => void][] = [
  [
    "a saved debate is never running, in flight, or waiting on the operator",
    () => {
      const out = stripForSave(
        state({
          running: true,
          live: {
            turn: 4,
            delegate: "a",
            label: "Advocate",
            color: "#f55",
            model: "m-a",
            round: 2,
            text: "half a s",
            reasoning: "",
            elapsed: 3,
            tokens: 9,
            skipped: false,
          },
          extension: { round: 2, rounds: 2, reason: "more" },
          adjourn: { by: "b", reason: "", round: 2, yes: 1, no: 0, outstanding: ["c"], passed: null },
          intermission: 2,
          skipped: [{ round: 1, ids: ["b"] }],
        }),
      );
      assert.equal(out.running, false);
      assert.equal(out.live, null);
      assert.equal(out.extension, null);
      assert.equal(out.adjourn, null);
      assert.equal(out.intermission, null);
      // A stale "skipped" badge from a round that is over would reappear on the
      // reopened debate as though it had just happened.
      assert.deepEqual(out.skipped, []);
    },
  ],
  [
    "a few notices survive so a debate that went wrong can be explained later",
    () => {
      const notices = Array.from({ length: 20 }, (_, i) => ({
        id: i,
        text: `n${i}`,
        tone: "info" as const,
      }));
      const out = stripForSave(state({ notices }));
      assert.equal(out.notices.length, 6);
      assert.equal(out.notices[5]!.text, "n19");
    },
  ],
  [
    "the summary counts each status on its own, never by subtraction",
    () => {
      const s = state({
        points: [point(1, "a"), point(2, "b"), point(3, "c"), point(4, "d")],
        tallies: {
          1: tally(),
          2: tally({ status: "fell", score: -1 }),
          3: tally({ status: "stalemate", score: 0 }),
          4: tally({ status: "withdrawn", score: 0 }),
        },
        verdict: {
          motion: "m",
          rounds: 3,
          endedHow: "complete",
          carried: [],
          fell: [],
          stalemate: [],
          withdrawn: [],
          crux: null,
          summary: "",
          adjournedBy: null,
          finalSummary: null,
          conclusion: null,
        },
      });
      const sum = summarise(s);
      assert.equal(sum.carried, 1);
      assert.equal(sum.fell, 1);
      assert.equal(sum.stalemate, 1);
      assert.equal(sum.withdrawn, 1);
      assert.equal(sum.points, 4);
      assert.equal(sum.unfinished, false);
    },
  ],
  [
    "an unfinished debate says so, and counts the rounds actually spoken",
    () => {
      const sum = summarise(state({ round: 2, maxRounds: 6, verdict: null }));
      assert.equal(sum.unfinished, true);
      assert.equal(sum.rounds, 2);
      assert.equal(sum.roundsConcluded, 0);
    },
  ],
  [
    "the summary records model names and nothing else about the endpoint",
    () => {
      // The config holds endpoint API keys. If a seat ever grew one, the
      // summary is what would leak it into a saved file.
      const sum = summarise(state());
      const json = JSON.stringify(sum);
      assert.ok(!json.toLowerCase().includes("apikey"));
      assert.ok(!json.toLowerCase().includes("api_key"));
      assert.deepEqual(sum.seats.map((x) => x.model), ["m-a", "m-b", "m-s"]);
      assert.deepEqual(sum.seats.map((x) => x.role), ["proponent", "opponent", "summarizer"]);
    },
  ],
  [
    "a pipe in a claim cannot break the board table",
    () => {
      const d = saved(
        state({
          points: [point(1, "A | B is a single claim")],
          tallies: { 1: tally() },
        }),
      );
      const md = toMarkdown(d);
      const row = md.split("\n").find((l) => l.includes("A \\| B"))!;
      // Header + separator + this one row: four columns, whatever the claim says.
      const header = md.split("\n").find((l) => l.startsWith("| | point"))!;
      assert.equal(cells(row), cells(header), `row: ${row}\nhead: ${header}`);
      assert.ok(row.includes("A \\| B"), row);
    },
  ],
  [
    "a newline in a claim cannot break the board table",
    () => {
      const d = saved(
        state({ points: [point(1, "line one\n\nline two")], tallies: { 1: tally() } }),
      );
      const md = toMarkdown(d);
      const row = md.split("\n").find((l) => l.includes("line one"))!;
      const header = md.split("\n").find((l) => l.startsWith("| | point"))!;
      assert.equal(cells(row), cells(header), `row: ${row}`);
    },
  ],
  [
    "the markdown leads with the motion and the answer",
    () => {
      const d = saved(
        state({
          verdict: {
            motion: "m",
            rounds: 3,
            endedHow: "complete",
            carried: [],
            fell: [],
            stalemate: [],
            withdrawn: [],
            crux: null,
            summary: "the long version",
            adjournedBy: null,
            finalSummary: "the summarizer's end result",
            conclusion: {
              agreed: false,
              answer: "",
              lean: "toward Plato",
              tally: "1 carried, 1 fell",
            },
          },
        }),
      );
      const md = toMarkdown(d);
      assert.ok(md.startsWith("# Roses outrank lilies"), md.slice(0, 60));
      // No agreement: the lean is the finding, and must not be dropped.
      assert.ok(md.includes("## Answer"), md);
      assert.ok(md.includes("could not agree on an answer"), md);
      assert.ok(md.includes("It leaned **toward Plato**"), md);
      assert.ok(md.includes("1 carried, 1 fell"), md);
      assert.ok(md.includes("the summarizer's end result"), md);
      // A blank answer with no agreement must not print an empty heading body.
      assert.ok(!/## Answer\n\n\n/.test(md));
    },
  ],
  [
    "the transcript is grouped by round, and the summarizer is marked",
    () => {
      const d = saved(
        state({
          speeches: [
            speech(1, "a", 1, "the opening"),
            speech(2, "b", 1, "the attack"),
            speech(3, "s", 1, "round one laid out", ["raised P1: a claim"]),
            speech(4, "a", 2, "the reply"),
          ],
        }),
      );
      const md = toMarkdown(d);
      assert.ok(md.includes("### Round 1"), md);
      assert.ok(md.includes("### Round 2"), md);
      assert.ok(md.includes("#### Summarizer — the floor"), md);
      assert.ok(md.includes("round one laid out"), md);
      assert.ok(md.includes("- raised P1: a claim"), md);
      // Each round header appears once, however many speeches it holds.
      assert.equal(md.split("### Round 1").length - 1, 1);
    },
  ],
  [
    "pages read are listed with their links",
    () => {
      const d = saved(
        state({
          reads: {
            2: [
              { source: "wikipedia", url: "https://en.wikipedia.org/wiki/Cat", title: "Cat", text: "x", truncated: false },
              { source: "web_search", url: "", title: "tautology", text: "hits", truncated: false },
            ],
          },
        }),
      );
      const md = toMarkdown(d);
      assert.ok(md.includes("## Sources read"), md);
      assert.ok(md.includes("<https://en.wikipedia.org/wiki/Cat>"), md);
      assert.ok(md.includes("tautology (search)"), md);
    },
  ],
  [
    "the json is versioned and round-trips the whole state",
    () => {
      const d = saved(state({ points: [point(1, "a")], tallies: { 1: tally() } }));
      const parsed = JSON.parse(toJson(d));
      assert.equal(parsed.format, "council-debate");
      assert.equal(parsed.version, 1);
      assert.equal(parsed.state.points.length, 1);
      assert.equal(parsed.state.points[0].text, "a");
      assert.ok(Number.isFinite(parsed.savedAt));
    },
  ],
  [
    "point lineage survives into the markdown",
    () => {
      const d = saved(
        state({
          points: [point(1, "the old claim"), point(2, "the new claim", { beats: [1] })],
          tallies: { 1: tally({ status: "fell", score: -1 }), 2: tally() },
        }),
      );
      const md = toMarkdown(d);
      assert.ok(md.includes("defeats P1"), md);
    },
  ],
  [
    "a poll records its tally, and an open one says so",
    () => {
      const open = saved(
        state({
          polls: [
            {
              id: 1,
              question: "Does the board change the outcome?",
              options: ["yes", "no"],
              openedBy: "a",
              round: 1,
              ballots: [
                { delegate: "a", choice: "yes", reason: null, confidence: null },
                { delegate: "b", choice: "yes", reason: null, confidence: null },
              ],
              closed: false,
            },
          ],
        }),
      );
      const md = toMarkdown(open);
      assert.ok(md.includes("## Polls"), md);
      assert.ok(md.includes("yes — 2"), md);
      assert.ok(md.includes("_no answer yet_"), md);
    },
  ],
  [
    "attached images are named in the record without their bytes",
    () => {
      const d = saved(state({ images: [{ id: "im1", name: "chart.png" }] }));
      const md = toMarkdown(d);
      assert.ok(md.includes("chart.png"), md);
      assert.ok(!md.includes("base64"), "bytes must never reach the record");
      const json = toJson(d);
      assert.ok(!json.includes("base64"), "bytes must never reach the library");
    },
  ],
  [
    "filenames come out safe from any motion",
    () => {
      assert.equal(slug("Roses outrank lilies?"), "roses-outrank-lilies");
      assert.equal(slug("  ../../etc/passwd  "), "etc-passwd");
      assert.equal(slug("!!!"), "debate");
      assert.equal(slug(""), "debate");
      assert.ok(!slug("a/b/c d").includes("/"));
    },
  ],
  [
    "the beats row spans however many seats sit at the table",
    () => {
      const d = saved(
        state({
          points: [point(1, "old"), point(2, "new", { beats: [1] })],
          tallies: { 1: tally(), 2: tally() },
        }),
      );
      const md = toMarkdown(d);
      const header = md.split("\n").find((l) => l.startsWith("| | point"))!;
      const beat = md.split("\n").find((l) => l.includes("defeats P1"))!;
      assert.equal(cells(beat), cells(header), `beat: ${beat}\nhead: ${header}`);
    },
  ],
  [
    "rulings and reviews survive into the board table",
    () => {
      const d = saved(
        state({
          points: [
            point(1, "ruled", {
              ruling: { by: "a", round: 2, carries: true, reason: "answered" },
            }),
          ],
          tallies: { 1: tally() },
        }),
      );
      const md = toMarkdown(d);
      assert.ok(md.includes("ruled carries"), md);
    },
  ],
];

let failed = 0;
for (const [name, fn] of tests) {
  try {
    fn();
    console.log(`  ok  ${name}`);
  } catch (e) {
    failed += 1;
    console.log(`FAIL  ${name}`);
    console.log(`      ${e instanceof Error ? e.message.split("\n")[0] : String(e)}`);
  }
}
console.log(`\n${tests.length - failed}/${tests.length} passed`);
if (failed) process.exit(1);