/**
 * Theme integrity.
 *
 * These are the tests that keep the theme system honest as themes are added.
 * CSS custom properties *inherit*, which is the whole reason a theme can be a
 * value change and nothing more — and also the reason a theme that forgets a
 * token does not fail. It silently keeps the previous theme's value, so a
 * mis-written `--warn` shows up as one amber verdict in an otherwise grey
 * board, long after the mistake. Nothing else in the build catches that.
 *
 * Run with `npm run test:web`.
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { DEFAULT_THEME, THEMES, isThemeId, themeMeta } from "./themes.ts";
import { seatGlyphs, seatSlots } from "./seats.ts";

const css = readFileSync(fileURLToPath(new URL("./index.css", import.meta.url)), "utf8");

/** Tokens every theme must define, whatever its colours. */
const REQUIRED = [
  "canvas",
  "surface",
  "surface-2",
  "surface-3",
  "line",
  "line-2",
  "ink",
  "ink-2",
  "ink-3",
  "primary",
  "primary-hover",
  "on-primary",
  "primary-soft",
  "ok",
  "warn",
  "bad",
  "bad-soft",
  "focus",
  "shadow-1",
  "shadow-pop",
  "glow",
  "ambient-1",
  "ambient-2",
  "bar-leader-a",
  "bar-leader-b",
  "avatar-ink",
  "avatar-ink-invert",
  "chevron-svg",
] as const;

/** The monochrome seat ramp: five essentials plus six custom seats. */
const SEAT_SLOTS = 12;

function blockFor(selector: string): string {
  const start = css.indexOf(selector);
  assert.ok(start >= 0, `no ${selector} block in index.css`);
  // Walk to the brace that closes the block, ignoring braces in comments.
  const open = css.indexOf("{", start);
  let depth = 0;
  for (let i = open; i < css.length; i += 1) {
    if (css[i] === "{") depth += 1;
    else if (css[i] === "}") {
      depth -= 1;
      if (depth === 0) return css.slice(open, i);
    }
  }
  throw new Error(`unterminated ${selector} block`);
}

function defines(block: string, token: string): boolean {
  return new RegExp(`(^|[\\s;{])${token}\\s*:`).test(block);
}

// ---------------------------------------------------------------------------
// Colour maths, for the ramp checks below.
//
// sRGB is gamma-encoded, so relative luminance has to linearise first — the
// same correction `inkOn` in Panels.tsx makes for avatar ink. Computing this on
// gamma-encoded values is what makes a mid-tone read as pale.

function linear(c: number): number {
  return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

function rgb(hex: string): [number, number, number] {
  const h = hex.replace(/^#/, "");
  return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16) / 255) as [
    number,
    number,
    number,
  ];
}

function luminance(hex: string): number {
  const [r, g, b] = rgb(hex);
  return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
}

function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/**
 * Which of a theme's two inks reads better on `disc` — the fallback path in
 * `seats.ts`, i.e. what a theme with no seat ramp resolves to.
 */
function bestInk(dark: string, light: string, disc: string): string {
  return contrast(dark, disc) >= contrast(light, disc) ? dark : light;
}

/**
 * `rgb` above returns 0–1. These two need the raw 0–255, because the distance
 * is compared against a threshold in those units and the hue maths wants
 * percentages — re-normalising an already-normalised value silently turned every
 * distance into ~0.008.
 */
function rgb255(hex: string): [number, number, number] {
  const h = hex.replace(/^#/, "");
  return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16)) as [
    number,
    number,
    number,
  ];
}

/** RGB distance, to tell "three greys" apart from "three muted hues". */
function rgbDistance(a: string, b: string): number {
  const [ar, ag, ab] = rgb255(a);
  const [br, bg, bb] = rgb255(b);
  return Math.hypot(ar - br, ag - bg, ab - bb);
}

/** Hue in degrees. Used only to require spread, so 0 and 360 count as adjacent. */
function hueDegrees(hex: string): number {
  const [r, g, b] = rgb255(hex).map((c) => c / 255);
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const d = max - min;
  if (d === 0) return 0;
  let h: number;
  if (max === r) h = ((g - b) / d) % 6;
  else if (max === g) h = (b - r) / d + 2;
  else h = (r - g) / d + 4;
  h *= 60;
  return h < 0 ? h + 360 : h;
}

/** The four greys a theme's semantic tokens can end up sitting on. */
const SURFACES = ["canvas", "surface", "surface-2", "surface-3"] as const;

/** Read one `#rrggbb` token out of a theme block, failing loudly if it is absent. */
function readIn(block: string, name: string): string {
  const m = new RegExp(`--${name}:\\s*(#[0-9a-f]{6})`, "i").exec(block);
  assert.ok(m, `no --${name} in this theme block`);
  return m[1]!;
}

// ---------------------------------------------------------------------------

const tests: [string, () => void][] = [
  [
    "every theme defines every token",
    () => {
      // The whole point of this file. Without it a theme block may silently
      // inherit the previous theme's value for anything it omits.
      for (const t of THEMES) {
        const selector = t.id === "sage-light" ? ":root," : `[data-theme="${t.id}"]`;
        const block = blockFor(selector);
        const missing = REQUIRED.filter((token) => !defines(block, `--${token}`));
        assert.deepEqual(missing, [], `${t.id} is missing: ${missing.join(", ")}`);
      }
    },
  ],
  [
    "a theme's seat ramp agrees with its seatRamp flag",
    () => {
      // Components read `var(--seat-N, <the seat's own colour>)`, so defining
      // `--seat-N` in a theme is how it overrides seat colours. The flag has to
      // match reality or the picker would describe a theme the CSS contradicts.
      //
      // Note this is *not* the same as `monochrome`: Amber defines a ramp too,
      // because a teal disc on an amber board gives the theme away.
      for (const t of THEMES) {
        const selector = t.id === "sage-light" ? ":root," : `[data-theme="${t.id}"]`;
        const block = blockFor(selector);
        const hasRamp = defines(block, "--seat-1");
        assert.equal(
          hasRamp,
          !!t.seatRamp,
          `${t.id} ramp presence (${hasRamp}) disagrees with its seatRamp flag (${!!t.seatRamp})`,
        );
        if (t.seatRamp) {
          const missing = Array.from({ length: SEAT_SLOTS }, (_, i) => i + 1).filter(
            (i) => !defines(block, `--seat-${i}`),
          );
          assert.deepEqual(missing, [], `${t.id} seat ramp is missing steps: ${missing}`);
        }
      }
    },
  ],
  [
    "every theme has a css block, and the default has one too",
    () => {
      for (const t of THEMES) {
        const selector = t.id === "sage-light" ? ":root," : `[data-theme="${t.id}"]`;
        assert.ok(css.includes(selector), `index.css has no block for ${t.id}`);
      }
      assert.ok(css.includes("[data-theme=\"sage-dark\"]"), "no sage-dark block");
    },
  ],
  [
    "isThemeId accepts every theme and nothing else",
    () => {
      for (const t of THEMES) assert.ok(isThemeId(t.id), `${t.id} should be valid`);
      for (const junk of ["", "dark", "light", "DARK", "sage", null, undefined, 3, {}]) {
        assert.equal(isThemeId(junk), false, `${String(junk)} should be rejected`);
      }
      // The two legacy ids migrate rather than being accepted as-is.
      assert.equal(isThemeId("dark"), false);
      assert.equal(isThemeId("light"), false);
    },
  ],
  [
    "the default theme is one of the themes",
    () => {
      assert.ok(isThemeId(DEFAULT_THEME), `${DEFAULT_THEME} is not a known theme`);
      assert.ok(themeMeta(DEFAULT_THEME), "no metadata for the default");
    },
  ],
  [
    "theme ids are unique and swatches are usable colours",
    () => {
      const ids = THEMES.map((t) => t.id);
      assert.equal(new Set(ids).size, ids.length, "duplicate theme id");
      for (const t of THEMES) {
        assert.equal(t.swatch.length, 3, `${t.id} swatch`);
        for (const c of t.swatch) {
          assert.match(c, /^#[0-9a-f]{6}$/i, `${t.id} swatch colour ${c}`);
        }
      }
    },
  ],
  [
    "no rule is gated on the removed two-value dark selector",
    () => {
      // A `[data-theme="dark"]` *selector* was the trap: every theme added
      // after it silently skipped the block, so a dark wash or a light poll bar
      // leaked through. Prose may still name it; a selector may not.
      const offending = css
        .split("\n")
        .filter((line) => line.includes('[data-theme="dark"]') && !line.trim().startsWith("*"))
        .filter((line) => !/\/\//.test(line) && !line.trim().startsWith("/*") && !line.trim().startsWith("* "));
      assert.deepEqual(offending, [], `still gated on the old selector: ${offending.join(" ")}`);
    },
  ],
  [
    "every seat ramp is distinct and visible on its own canvas",
    () => {
      // A ramp step that is too close to its neighbour makes two seats
      // indistinguishable, and one that is too dark against the canvas simply
      // disappears — which is worse, because the seat looks absent.
      for (const t of THEMES) {
        if (!t.seatRamp) continue;
        const block = blockFor(t.id === "sage-light" ? ":root," : `[data-theme="${t.id}"]`);
        const steps = Array.from({ length: SEAT_SLOTS }, (_, i) => {
          const m = new RegExp(`--seat-${i + 1}:\\s*(#[0-9a-f]{6})`, "i").exec(block);
          assert.ok(m, `${t.id} has no --seat-${i + 1}`);
          return m[1]!;
        });
        assert.equal(
          new Set(steps).size,
          steps.length,
          `${t.id}: two seats share a ramp step`,
        );
        const canvas = /--canvas:\s*(#[0-9a-f]{6})/i.exec(block)?.[1];
        assert.ok(canvas, `${t.id} has no --canvas`);
        for (const [i, c] of steps.entries()) {
          const ratio = contrast(c, canvas!);
          assert.ok(
            ratio >= 2.5,
            `${t.id} --seat-${i + 1} (${c}) is only ${ratio.toFixed(2)}:1 on its canvas`,
          );
        }
      }
    },
  ],
  [
    "seat glyph ink is legible on every seat it can land on",
    () => {
      // The glyph is a single letter on a filled disc, so it is large text:
      // 3:1 is the bar, and 4.5:1 is the target. Amber's darkest step sits at
      // 3.96:1 and cannot do better — a saturated mid-tone orange is the one
      // colour where neither near-black nor cream separates well, so the ramp
      // is accepted there rather than degraded to fix a number.
      //
      // Both sources of the disc are checked: the ramp steps where the theme
      // defines them, and the stored seat colours otherwise. Critic's teal is
      // the case that motivated this — an ink threshold of 0.35 put pale ink on
      // it at 2.68:1, on the theme every new user sees first.
      const STORED = ["#65ccc6", "#4aa8a1", "#bffffa", "#c9b8f2", "#8ad9d3"];
      for (const t of THEMES) {
        const block = blockFor(t.id === "sage-light" ? ":root," : `[data-theme="${t.id}"]`);
        const read = (name: string) =>
          new RegExp(`--${name}:\\s*(#[0-9a-f]{6})`, "i").exec(block)?.[1];
        const dark = read("avatar-ink");
        const light = read("avatar-ink-invert");
        assert.ok(dark && light, `${t.id} is missing an ink token`);

        const discs: [string, string][] = [];
        for (const c of STORED) discs.push([`stored ${c}`, c]);
        if (t.seatRamp) {
          for (let i = 1; i <= SEAT_SLOTS; i += 1) {
            const step = read(`seat-${i}`);
            assert.ok(step, `${t.id} has no --seat-${i}`);
            const ink = read(`seat-ink-${i}`);
            assert.ok(ink, `${t.id} defines --seat-${i} but no --seat-ink-${i}`);
            discs.push([`--seat-${i} ${step}`, step]);
            // The declared ink must be the one that actually wins.
            const win = contrast(dark, step) >= contrast(light, step) ? dark : light;
            assert.equal(
              ink,
              win,
              `${t.id} --seat-ink-${i} is ${ink}, but ${win} does better on ${step}`,
            );
          }
        }
        for (const [what, disc] of discs) {
          // Resolve the ink the way the component will: the token if the theme
          // defines it, else the crossover in seats.ts.
          const slotMatch = /^--seat-(\d+)/.exec(what);
          const ink = slotMatch ? read(`seat-ink-${slotMatch[1]}`)! : bestInk(dark, light, disc);
          const ratio = contrast(ink, disc);
          assert.ok(
            ratio >= 3,
            `${t.id}: ink ${ink} on ${what} is only ${ratio.toFixed(2)}:1`,
          );
        }
      }
    },
  ],
  [
    "every verdict colour is legible on its own theme's surfaces",
    () => {
      // The regression this exists for: `--ok`, `--warn` and `--bad` were greys
      // in both low-chroma themes, which put `--bad` at 1.77:1 on Graphite and
      // 1.67:1 on Monochrome high contrast. Those are drawn as a 2px left border
      // on the board card *and* as 10px uppercase text — so "did not stand", the
      // verdict that most changes how you read a debate, was the least visible
      // thing on screen. Nothing else in the build would have caught it: the
      // tokens were all defined, so the inheritance check above passed.
      //
      // 4.5:1 for text (WCAG 1.4.3) on the three surfaces these tokens are
      // actually set on, and 3:1 for the graphical floor (1.4.11) on the
      // lightest one, which is where a pill or hover tint can put them.
      for (const t of THEMES) {
        const block = blockFor(`[data-theme="${t.id}"]`);
        for (const role of ["ok", "warn", "bad"] as const) {
          const token = readIn(block, role);
          const textWorst = Math.min(
            ...SURFACES.slice(0, 3).map((s) => contrast(token, readIn(block, s))),
          );
          assert.ok(
            textWorst >= 4.5,
            `${t.id} --${role} ${token} is only ${textWorst.toFixed(2)}:1 as text on its own surfaces`,
          );
          const graphicWorst = contrast(token, readIn(block, "surface-3"));
          assert.ok(
            graphicWorst >= 3,
            `${t.id} --${role} ${token} is only ${graphicWorst.toFixed(2)}:1 on surface-3`,
          );
        }
      }
    },
  ],
  [
    "the three verdicts are told apart by hue, not by which grey",
    () => {
      // Distinctness is the other half of legibility: three colours can all clear
      // 4.5:1 and still be unreadable if nothing distinguishes them. Graphite's
      // greys spanned 3° of hue — they were the same colour, and only their
      // lightness differed, by 1.81:1 between "tied" and "did not stand".
      for (const t of THEMES) {
        const block = blockFor(`[data-theme="${t.id}"]`);
        const hues = (["ok", "warn", "bad"] as const).map((r) => hueDegrees(readIn(block, r)));
        // Circular spread, so a palette that straddles 0°/360° is not penalised.
        const gaps = hues.map((h, i) => {
          const d = Math.abs(h - hues[(i + 1) % 3]);
          return Math.min(d, 360 - d);
        });
        const smallest = Math.min(...gaps);
        assert.ok(
          smallest >= 25,
          `${t.id}: ok/warn/bad hues ${hues.map((h) => h.toFixed(0)).join("/")} are only ${smallest.toFixed(0)}° apart`,
        );
        // And far enough apart in RGB that a low-chroma palette cannot squeak
        // under the hue check on a single pair.
        const dists = [
          rgbDistance(readIn(block, "ok"), readIn(block, "warn")),
          rgbDistance(readIn(block, "ok"), readIn(block, "bad")),
          rgbDistance(readIn(block, "warn"), readIn(block, "bad")),
        ];
        assert.ok(
          Math.min(...dists) >= 30,
          `${t.id}: two verdicts are ${Math.min(...dists).toFixed(0)} apart in RGB`,
        );
      }
    },
  ],
  [
    "a muted verdict colour still reads on the bad-soft tint",
    () => {
      // `--bad-soft` backs `text-bad` in the error banner, so desaturating `--bad`
      // without thinking about the tint would have quietly lowered this pair.
      for (const t of THEMES) {
        const block = blockFor(`[data-theme="${t.id}"]`);
        const ratio = contrast(readIn(block, "bad"), readIn(block, "bad-soft"));
        assert.ok(
          ratio >= 4.5,
          `${t.id}: --bad on --bad-soft is only ${ratio.toFixed(2)}:1`,
        );
      }
    },
  ],
  [
    "the monochrome ramps space their steps by luminance",
    () => {
      // With hue gone, lightness is the only channel left, so the greys have to
      // actually step. (Amber's ramp leans on hue as well and is checked above.)
      for (const id of ["grey", "mono-hc"] as const) {
        const block = blockFor(`[data-theme="${id}"]`);
        const steps = Array.from({ length: SEAT_SLOTS }, (_, i) => {
          const m = new RegExp(`--seat-${i + 1}:\\s*(#[0-9a-f]{6})`, "i").exec(block);
          assert.ok(m, `${id} has no --seat-${i + 1}`);
          return luminance(m[1]!);
        }).sort((a, b) => a - b);
        for (let i = 1; i < steps.length; i += 1) {
          const gap = steps[i]! - steps[i - 1]!;
          assert.ok(
            gap > 0.01,
            `${id}: steps ${i} and ${i + 1} are only ${gap.toFixed(4)} apart in luminance`,
          );
        }
      }
      assert.ok(themeMeta("grey").monochrome, "grey should be flagged monochrome");
    },
  ],
  [
    "seat marks are unique across a full council",
    () => {
      // Five essentials plus six custom seats — the largest council allowed.
      const seats = [
        { id: "a", role: "proponent" },
        { id: "b", role: "opponent" },
        { id: "c", role: "judge" },
        { id: "d", role: "rationale" },
        { id: "s", role: "summarizer" },
        ...Array.from({ length: 6 }, (_, i) => ({ id: `x${i}`, role: "custom" })),
      ];
      const glyphs = seatGlyphs(seats);
      const marks = Object.values(glyphs);
      assert.equal(marks.length, seats.length, "a seat got no mark");
      assert.equal(new Set(marks).size, seats.length, `duplicate marks: ${marks.join(",")}`);
      assert.equal(glyphs.a, "A");
      assert.equal(glyphs.b, "C");
      assert.equal(glyphs.s, "S");
    },
  ],
  [
    "the essentials never collide on a mark",
    () => {
      // Advocate and Arbiter used to both render an "A", because the avatar
      // showed the first letter of the label.
      const essentials = [
        { id: "a", role: "proponent" },
        { id: "c", role: "judge" },
      ];
      const glyphs = seatGlyphs(essentials);
      assert.notEqual(glyphs.a, glyphs.c);
    },
  ],
  [
    "a seat without a role still gets a unique mark",
    () => {
      // A roster predating roles: no role to key off, so every seat falls to
      // the alphabet path and must still be distinct.
      const seats = [{ id: "a" }, { id: "b" }, { id: "c" }];
      const glyphs = seatGlyphs(seats);
      assert.equal(new Set(Object.values(glyphs)).size, seats.length);
    },
  ],
  [
    "ramp slots are 1-based and cover a full council",
    () => {
      const seats = Array.from({ length: 11 }, (_, i) => ({ id: `x${i}` }));
      const slots = seatSlots(seats);
      assert.equal(slots.x0, 1, "slots should start at 1 to match --seat-1");
      assert.equal(Math.max(...Object.values(slots)), 11);
      // Past the ramp, the last slot is reused rather than wrapping to 1, which
      // would put two seats on the same colour.
      const over = seatSlots(Array.from({ length: 15 }, (_, i) => ({ id: `x${i}` })));
      assert.equal(over.x14, 12);
      assert.notEqual(over.x0, over.x14);
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