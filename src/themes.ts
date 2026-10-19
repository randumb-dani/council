/**
 * The themes, and the metadata the picker needs.
 *
 * Values live in `index.css`, one `[data-theme="…"]` block each. Keeping them
 * there rather than here is deliberate: a theme is CSS, so it works before React
 * mounts and can be inspected and tweaked without touching TypeScript. This
 * module only knows which themes exist, in what order, and what to call them.
 *
 * `swatch` is the three colours shown in the picker — canvas, surface and
 * primary — so a theme can be previewed without being applied.
 */

export type ThemeId =
  | "sage-dark"
  | "sage-light"
  | "orange"
  | "orange-light"
  | "grey"
  | "slate"
  | "slate-light"
  | "mono-hc";

export type ThemeTone = "dark" | "light";

export interface ThemeMeta {
  id: ThemeId;
  /** Shown in the picker. */
  label: string;
  /** One line on what it is for, for anyone who cannot see the difference. */
  note: string;
  /** canvas / surface / primary, for the picker's preview chip. */
  swatch: [string, string, string];
  /**
   * Whether the browser should render form controls and scrollbars dark.
   *
   * Set on the root element from this rather than derived in CSS, because
   * `color-scheme` has to match the scheme or the native scrollbar, caret and
   * select popup render light-on-dark and the whole illusion breaks.
   */
  tone: ThemeTone;
  /**
   * True when the theme's *chrome* carries no hue: neutral greys, with delegates
   * told apart by disc lightness rather than by colour.
   *
   * It says nothing about the semantic tokens. Graphite and Monochrome high
   * contrast both keep a muted green / amber / rose for carried / fell / tied,
   * because three greys cannot carry that distinction — `--bad` as a grey sat at
   * 1.67:1 on black, and the board's "did not stand" edge was invisible in the
   * one theme built to be legible. Muted is not the same as absent, and this flag
   * describes the surfaces, not the verdicts.
   */
  monochrome?: boolean;
  /**
   * True when this theme defines its own `--seat-1` … `--seat-12`.
   *
   * Separate from `monochrome`: Amber defines a ramp too, because a teal disc
   * on an amber board gives the theme away. A theme that defines none of them
   * inherits each seat's stored colour.
   */
  seatRamp?: boolean;
}

export const THEMES: readonly ThemeMeta[] = [
  {
    id: "sage-dark",
    label: "Sage dark",
    note: "deep teal green, for dark rooms",
    swatch: ["#0a1514", "#173331", "#65ccc6"],
    tone: "dark",
  },
  {
    id: "sage-light",
    label: "Sage light",
    note: "the same teal green, on a pale background",
    swatch: ["#e7f3f0", "#dceee9", "#19665f"],
    tone: "light",
  },
  {
    id: "orange",
    label: "Amber",
    note: "warm amber and brown, with orange delegate colours",
    swatch: ["#17110b", "#2a201a", "#f0a868"],
    tone: "dark",
    seatRamp: true,
  },
  {
    id: "orange-light",
    label: "Amber light",
    note: "the same warm amber and brown, on a pale background",
    swatch: ["#ece1cd", "#faf4e8", "#9a5a08"],
    tone: "light",
    seatRamp: true,
  },
  {
    id: "slate",
    label: "Slate",
    note: "graphite's dark grey-blue cousin — one steel-blue accent, delegates in muted blue-greys",
    swatch: ["#12161c", "#232d3a", "#7fb0d8"],
    tone: "dark",
    seatRamp: true,
  },
  {
    id: "slate-light",
    label: "Slate light",
    note: "the same steel-blue on a pale background, for daylight",
    swatch: ["#e3e9f1", "#f3f6fb", "#2f6da3"],
    tone: "light",
    seatRamp: true,
  },
  {
    id: "grey",
    label: "Graphite",
    note: "neutral greys — delegates by disc lightness, verdicts in muted colour",
    swatch: ["#161718", "#232528", "#d7d9dc"],
    tone: "dark",
    monochrome: true,
    seatRamp: true,
  },
  {
    id: "mono-hc",
    label: "Monochrome high contrast",
    note: "pure black on pure white, with the three verdicts in muted colour",
    swatch: ["#000000", "#0d0d0d", "#ffffff"],
    tone: "dark",
    monochrome: true,
    seatRamp: true,
  },
];

export const DEFAULT_THEME: ThemeId = "sage-dark";

const IDS = new Set<string>(THEMES.map((t) => t.id));

export function isThemeId(v: unknown): v is ThemeId {
  return typeof v === "string" && IDS.has(v);
}

export function themeMeta(id: ThemeId): ThemeMeta {
  // Every id in THEMES is a key of it by construction; the fallback is only
  // here so a hand-edited value cannot throw mid-render.
  return THEMES.find((t) => t.id === id) ?? THEMES[0]!;
}

/** Storage key for the operator's choice. */
export const THEME_KEY = "council.theme";