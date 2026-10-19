/**
 * User-facing copy integrity.
 *
 * Every rule here exists because of something that already shipped. The engine
 * names things `make_point`, `win` and `proponent` — those are prompt-side
 * vocabulary and are correct in the reducer. The bug is showing them to a
 * reader, and that failure is invisible in review: the code is right, the tests
 * pass, and the panel just says "2 make_point".
 *
 * So the rules are mechanical rather than editorial. Whether a tooltip reads
 * *well* is a judgement call that will differ between reviewers; catching `win`
 * inside a template literal is not.
 *
 * One test does police wording — the "seat" check — and it is narrow on purpose:
 * it only fires on the word inside a quoted string, so CSS variables, internal
 * ids and the engine-facing `seatRole` are all left alone.
 *
 * Run with `npm run test:web`.
 */

import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { VERDICT_LABEL, ROLE_HINT, type SeatRole } from "./types.ts";

const SRC = new URL("./", import.meta.url);

/**
 * Every source file, `components/` included.
 *
 * This was a flat `readdirSync`, which listed only the top level — so no rule
 * below ever saw `SetupPanel.tsx`, `Panels.tsx` or any other component. Every
 * piece of UI copy lives in there, so the whole test was checking the wrong
 * files, and the rule at the bottom could not find the path it was looking for.
 */
function walk(dir: URL, prefix = ""): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      out.push(...walk(new URL(`${entry.name}/`, dir), rel));
    } else if (/\.(ts|tsx)$/.test(entry.name) && !entry.name.endsWith(".test.ts")) {
      out.push(rel);
    }
  }
  return out;
}

const files = walk(SRC).sort();
const source = new Map(
  files.map((f) => [f, readFileSync(fileURLToPath(new URL(f, SRC)), "utf8")]),
);

/** JSX attributes that put text on screen. */
const TEXT_ATTR =
  /(?:title|aria-label|placeholder)\s*=\s*(?:"([^"]*)"|\{\s*`([\s\S]*?)`\s*\}|\{\s*"([^"]*)"\s*\})/g;

/**
 * The literal copy in a tooltip, with interpolations removed.
 *
 * `${s.win}` is a value being substituted, not the word "win" being written, so
 * the spread bar's "3 supporting, 1 opposing" is correct and must not trip the
 * verdict check.
 *
 * This is also the limit of what these tests can see. They catch a raw
 * identifier written into copy — `title="you win"` fails — but a leak that
 * arrives through a variable, as the vote tooltip's did, leaves no literal
 * behind to find. Those need the label maps to be correct, which is what
 * VERDICT_LABEL and ACTION_LABEL are for, and what review is for.
 */
function tooltips(file: string): string[] {
  const out: string[] = [];
  for (const m of source.get(file)!.matchAll(TEXT_ATTR)) {
    out.push((m[1] ?? m[2] ?? m[3] ?? "").replace(/\$\{[^}]*\}/g, "N"));
  }
  return out;
}

/** Comments and the label maps themselves, which are allowed the raw words. */
function code(file: string): string {
  return source
    .get(file)!
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/\/\/[^\n]*/g, "");
}

const tests: [string, () => void][] = [
  [
    "no tooltip quotes a raw verdict",
    () => {
      // `win` / `lose` / `stalemate` leaked into a vote tooltip as
      // "Arbiter: lose", and into the change-of-mind log as a bare "win → lose".
      // VERDICT_LABEL exists for exactly this.
      for (const file of files) {
        for (const value of tooltips(file)) {
          for (const verdict of ["win", "lose", "stalemate"]) {
            assert.ok(
              !new RegExp(`\\b${verdict}\\b`).test(value),
              `${file}: tooltip contains the raw verdict "${verdict}": ${value.slice(0, 90)}`,
            );
          }
        }
      }
    },
  ],
  [
    "no tooltip quotes a raw SeatRole",
    () => {
      // "no proponent seat" was a blocking error about a classification the user
      // never sees. The UI calls these delegates by name.
      for (const file of files) {
        for (const value of tooltips(file)) {
          for (const role of ["proponent", "opponent", "judge", "summarizer"]) {
            assert.ok(
              !new RegExp(`\\b${role}\\b`).test(value),
              `${file}: tooltip contains the SeatRole value "${role}": ${value.slice(0, 90)}`,
            );
          }
        }
      }
    },
  ],
  [
    "no tooltip renders a bare engine action tag",
    () => {
      // The specific bug: RecordPanel counted `Object.entries(acts)` and printed
      // the keys, so the live panel read "2 make_point".
      //
      // The tag list is read out of the source rather than imported: the map
      // lives in a .tsx file, and pulling a component into a Node strip-types run
      // risks failing on the JSX before any rule executes.
      const panels = source.get("components/Panels.tsx")!;
      const map = /ACTION_LABEL[^=]*=\s*\{([\s\S]*?)\n\};/.exec(panels);
      assert.ok(map, "could not find ACTION_LABEL in Panels.tsx");
      const tags = [...map[1]!.matchAll(/^\s*"?([\w-]+)"?\s*:/gm)]
        .map((m) => m[1]!)
        // Only tags containing a separator are checked. Half of ACTION_LABEL's
        // keys are ordinary English words — `vote`, `search`, `read`, `cite` —
        // and copy is allowed to use those; "not a general web search" is
        // correct prose. A test cannot tell the noun from the identifier, and
        // pretending otherwise would only push the copy somewhere dumber.
        // `make_point` and `re-vote` are unambiguously identifiers.
        .filter((t) => /[_-]/.test(t));
      assert.ok(tags.length >= 2, `expected several multi-word tags, got ${tags.join(", ")}`);
      for (const file of files) {
        for (const value of tooltips(file)) {
          for (const tag of tags) {
            assert.ok(
              !value.includes(tag),
              `${file}: tooltip contains the engine tag "${tag}": ${value.slice(0, 90)}`,
            );
          }
        }
      }
    },
  ],
  [
    "no action count interpolates a raw tag variable",
    () => {
      // Covers the single-word tags the previous test cannot, by checking the
      // shape of the leak instead of the word: a count sitting next to a
      // variable that came straight out of the `acts` map is the bug, whichever
      // tag it happens to hold. This is the line that would have caught
      // `<b>{n}</b> {tool}`.
      for (const [file, src] of source) {
        assert.ok(
          !/\{n\}\s*\{tool\}/.test(src),
          `${file}: renders a count next to the raw tool name "{tool}" — use actionCountLabel(tool, n)`,
        );
        assert.ok(
          !/\{n\}\s*\{tag\}/.test(src),
          `${file}: renders a count next to the raw tag "{tag}" — use actionCountLabel`,
        );
      }
    },
  ],
  [
    "every role the rail can show has a hint",
    () => {
      // The role pill passed `undefined` for proponent, opponent and judge, so
      // three of the five roles were unexplained anywhere in the app.
      const roles: SeatRole[] = [
        "proponent",
        "opponent",
        "judge",
        "rationale",
        "summarizer",
        "custom",
      ];
      for (const role of roles) {
        const hint = ROLE_HINT[role];
        assert.ok(hint, `ROLE_HINT has no entry for "${role}"`);
        assert.ok(
          hint.trim().endsWith("."),
          `ROLE_HINT.${role} does not read as a sentence: "${hint}"`,
        );
      }
    },
  ],
  [
    "every verdict has a word, not just a glyph",
    () => {
      // The glyphs mean nothing to a screen reader, which is what made dropping
      // the enum into a tooltip look reasonable in the first place.
      for (const verdict of ["win", "lose", "stalemate"] as const) {
        const word = VERDICT_LABEL[verdict];
        assert.ok(word, `VERDICT_LABEL has no entry for "${verdict}"`);
        assert.ok(!/^[▲▼◆·]$/.test(word), `VERDICT_LABEL.${verdict} is still a glyph`);
      }
    },
  ],
  [
    "no tooltip is long enough to be unreadable",
    () => {
      // A native `title` is a single line, cannot render a newline, and
      // truncates in the OS. The reasoning-effort hint ran to 250 characters
      // over three paragraphs including two GitHub issue numbers, so it arrived
      // as one run-on line. Past this, it is a caption that belongs in the
      // layout rather than a hover.
      const LIMIT = 200;
      for (const file of files) {
        for (const value of tooltips(file)) {
          if (!value || !/[a-z]{3}/i.test(value)) continue;
          assert.ok(
            value.length <= LIMIT,
            `${file}: tooltip is ${value.length} chars, past the ${LIMIT} limit: ${value.slice(0, 80)}…`,
          );
        }
      }
    },
  ],
  [
    "the jargon that was load-bearing in a hover is gone",
    () => {
      // Each of these appeared in a tooltip with nothing nearby explaining it.
      // They are removed rather than glossed, because the surrounding copy now
      // says the same thing in plain words.
      for (const file of files) {
        for (const value of tooltips(file)) {
          const lower = value.toLowerCase();
          for (const gone of [
            "interject",
            "crux",
            "moved to adjourn",
            "ballots included",
            "a different store",
            "bot challenge",
            "stream_options",
            "reasoning_effort",
          ]) {
            assert.ok(
              !lower.includes(gone),
              `${file}: tooltip still uses "${gone}": ${value.slice(0, 90)}`,
            );
          }
        }
      }
    },
  ],
  [
    "a user never reads the word \"seat\"",
    () => {
      // "delegate", "seat" and "council" all named the same object, sometimes in
      // one view — the rail said "5 seats" directly above a list of delegates.
      // A delegate is the model; the council is the group.
      //
      // Only quoted text is checked, so `--seat-4`, `seat-${i}`, the `seatRole`
      // field and the CSS variables are all left alone: those are internal and
      // renaming them is not what this is for.
      const hits: string[] = [];
      for (const file of files) {
        for (const m of code(file).matchAll(/["'`]([^"'`\n]*\bseats?\b[^"'`\n]*)["'`]/g)) {
          const value = m[1]!.trim();
          // Not prose: a bare identifier, an import path, an id built at
          // runtime (`seat-${i}`), or a CSS variable. Those are internal, and
          // renaming them is a different job from this one.
          // A module specifier — "./seats" is the file, not the word.
          if (/^\.\.?\/[\w./-]+$/.test(value)) continue;
          if (/^[a-z$][\w$./-]*$/i.test(value)) continue;
          if (value.includes("${")) continue;
          hits.push(`${file}: "${value.slice(0, 90)}"`);
        }
      }
      assert.deepEqual(
        hits,
        [],
        `user-facing text still says "seat" — use "delegate" for the model, "council" for the group:\n  ${hits.join("\n  ")}`,
      );
    },
  ],
  [
    "an unknown action tag still reads as a word",
    () => {
      // actionLabel used to fall back to the tag itself, so any engine action
      // without an entry printed `web_search` or whatever. The fallback has to
      // be a word, or the leak returns the moment the engine adds an action.
      const src = source.get("components/Panels.tsx")!;
      assert.ok(
        /ACTION_LABEL\[tag\]\s*\?\?\s*"Action"/.test(src),
        "actionLabel no longer falls back to a plain word",
      );
      assert.ok(
        !/ACTION_LABEL\[tag\]\s*\?\?\s*tag/.test(src),
        "actionLabel falls back to the raw tag again",
      );
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
    const msg = e instanceof Error ? e.message : String(e);
    console.log(`      ${msg.split("\n").join("\n      ")}`);
  }
}
console.log(`\n${tests.length - failed}/${tests.length} passed`);
if (failed) process.exit(1);