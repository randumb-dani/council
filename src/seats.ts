/**
 * The seat mark and the colour ramp.
 *
 * Kept out of `Panels.tsx` so `useCouncil` can derive marks without importing a
 * component module — that import would pull the whole transcript into the
 * reducer's dependency graph for two functions.
 *
 * Neither of these knows a theme exists. The mark is text; the slot is a number
 * used only as a key for `var(--seat-N, <the seat's own colour>)`, which the
 * monochrome themes define and the coloured ones deliberately do not.
 */

/** Fixed marks for the essentials, so seat identity survives losing hue. */
const ROLE_GLYPH: Record<string, string> = {
  proponent: "A",
  opponent: "C",
  judge: "R",
  rationale: "N",
  summarizer: "S",
  custom: "D",
};

/** Spare marks for added seats. "A"–"Z" minus those already taken above. */
const GLYPH_ALPHABET = ["D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "O", "P", "Q", "T", "U"];

/** First unused colour ramp entry, so an added delegate never copies Advocate. */
export const DELEGATE_PALETTE = [
  "#65ccc6",
  "#2e8b83",
  "#bffffa",
  "#4aa8a1",
  "#8ad9d3",
  "#1f6b64",
  "#d9fffc",
  "#0f4a45",
];

/** Seat marks by delegate id. Unique across the roster, essentials included. */
export function seatGlyphs(delegates: { id: string; role?: string }[]): Record<string, string> {
  const out: Record<string, string> = {};
  const taken = new Set<string>();
  // Essentials first, so a custom seat never steals an essential's mark.
  for (const d of delegates) {
    const g = d.role ? ROLE_GLYPH[d.role] : undefined;
    if (g && !taken.has(g)) {
      out[d.id] = g;
      taken.add(g);
    }
  }
  let i = 0;
  for (const d of delegates) {
    if (out[d.id]) continue;
    while (i < GLYPH_ALPHABET.length && taken.has(GLYPH_ALPHABET[i]!)) i += 1;
    const g = GLYPH_ALPHABET[i] ?? "?";
    out[d.id] = g;
    taken.add(g);
    i += 1;
  }
  return out;
}

/**
 * The seat's 1-based place in the monochrome ramp, capped at the ramp's length.
 *
 * Eleven seats can sit at a table (five essentials plus six custom); the ramp
 * defines twelve steps, so nothing has to wrap and reuse step one.
 */
export function seatSlots(delegates: { id: string }[]): Record<string, number> {
  const out: Record<string, number> = {};
  delegates.forEach((d, i) => {
    out[d.id] = Math.min(i + 1, 12);
  });
  return out;
}

/** Relative luminance of a hex colour, for choosing ink. */
function luminance(hex: string): number | null {
  const raw = hex.trim().replace(/^#/, "");
  // Short hex is expanded first. `#5af` parsed as-is gives `NaN` for the green
  // channel, and every comparison built on it silently falls the wrong way.
  const m =
    raw.length === 3
      ? raw
          .split("")
          .map((c) => c + c)
          .join("")
      : raw;
  if (!/^[0-9a-f]{6}$/i.test(m.slice(0, 6))) return null;
  const channel = (i: number) => {
    const c = parseInt(m.slice(i, i + 2), 16) / 255;
    // sRGB is gamma-encoded; linearise before weighting, or a mid-tone reads as pale.
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  const lum = 0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4);
  return Number.isFinite(lum) ? lum : null;
}

/**
 * Where dark ink stops beating pale ink: the geometric mean of the two inks'
 * (luminance + 0.05), which is the exact point where their contrast ratios meet.
 *
 * Every theme's ink pair crosses between 0.17 and 0.19, so one constant serves
 * all five — and `themes.test.ts` checks that against each theme's real values
 * rather than trusting it.
 *
 * This is deliberately *not* a round 0.35. That older cut-off put pale ink on
 * Critic's teal (#4aa8a1, luminance 0.320) at 2.68:1 when dark ink reached
 * 6.77:1 — the glyph was washed out on the default theme, which is the one
 * everyone sees first.
 */
export const INK_CROSSOVER = 0.18;

/**
 * Which of a theme's two ink tokens reads better on `color`.
 *
 * Exported so `themes.test.ts` can check the real rule against each theme's
 * actual ink values, rather than checking an ideal the component never applies.
 */
export function inkTokenFor(color: string): string {
  const lum = luminance(color);
  if (lum === null) return "var(--avatar-ink)";
  return lum > INK_CROSSOVER ? "var(--avatar-ink)" : "var(--avatar-ink-invert)";
}

/**
 * The glyph colour for a seat avatar.
 *
 * Themes with a seat ramp also define `--seat-ink-N` beside each step, and that
 * token wins when it exists: the glyph then sits on the ramp colour, which JS
 * cannot resolve, so no amount of inspecting the stored colour says what is
 * actually behind the letter. `inkOn` is only the fallback for the themes with
 * no ramp.
 *
 * Returns a token rather than a literal, so the glyph follows the theme.
 */
export function seatInk(slot: number, color: string): string {
  return `var(--seat-ink-${slot}, ${inkOn(color)})`;
}

function inkOn(hex: string): string {
  return inkTokenFor(hex);
}