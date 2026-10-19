/**
 * The library: every debate kept, and how it leaves.
 *
 * A debate here is the entire output of an afternoon — four models, six
 * rounds, real minutes — and until this existed it lived only in the window.
 * Closing the app lost it, and there was no way to take it anywhere else.
 *
 * Two rules shaped the module:
 *
 * - **Nothing secret is written.** `DebateConfig` holds endpoint API keys. It is
 *   never persisted; only the state is, and only the model *names* are recorded
 *   from the config, so a saved debate says who sat in the room without handing
 *   over the keys to the room.
 * - **The whole state object is saved, not a hand-picked list.** Later work adds
 *   fields the export must carry, and a field list is a field list that goes
 *   stale silently. `stripForSave` removes what cannot be restored; everything
 *   else rides along and picks itself up in the Markdown for free.
 */

import type { CouncilState } from "./useCouncil";
import type { Poll, Tally } from "./types";

const KEY = "council.debates.v1";

/** How many debates to keep before the oldest goes. */
const MAX_KEPT = 20;

export interface SavedDebate {
  id: string;
  /** Epoch ms of the last write, for ordering and the list's date line. */
  savedAt: number;
  /** A copy of the summary, so the library renders without loading each record. */
  summary: DebateSummary;
  state: CouncilState;
}

export interface DebateSummary {
  motion: string;
  /** Rounds actually spoken. */
  rounds: number;
  roundsConcluded: number;
  carried: number;
  fell: number;
  stalemate: number;
  withdrawn: number;
  points: number;
  speeches: number;
  unfinished: boolean;
  endedHow: string | null;
  closedBy: string | null;
  seats: { label: string; model: string; role: string }[];
  /** A few words for the card, from the answer when there was one. */
  answer: string | null;
}

// ---------------------------------------------------------------------------
// storing
// ---------------------------------------------------------------------------

/**
 * What can be restored later. A saved debate is never running, has no turn in
 * flight and is not waiting on the operator: reopening one must not look like a
 * debate that is somehow still going.
 */
export function stripForSave(state: CouncilState): CouncilState {
  return {
    ...state,
    running: false,
    live: null,
    extension: null,
    intermission: null,
    adjourn: null,
    reviews: {},
    skipped: [],
    // Notices are diagnostics. The last few explain a debate that went wrong,
    // which is exactly the one you will want to read later; the rest is scroll.
    notices: state.notices.slice(-6),
    seatFailures: [],
  };
}

function read(): SavedDebate[] {
  try {
    const raw = window.localStorage.getItem(KEY);
    if (!raw) {
      readCache = null;
      return [];
    }
    // The library is re-read on every save, every library render, and every
    // capacity check — a full parse plus per-speech/per-point validation each
    // time, over up to 20 full debates. The raw string is the cache key: same
    // bytes, same library, so repeated reads within one debate cost a string
    // compare. A write from another tab changes the bytes and re-parses, so no
    // tab can go stale. Callers must not mutate what comes back — `saveDebate`
    // and `listDebates` both work on copies (see below).
    if (readCache && readCache.raw === raw) return readCache.parsed;
    const parsed = validate(raw);
    readCache = { raw, parsed };
    return parsed;
  } catch {
    // A corrupt or half-written store must not take the app down with it.
    readCache = null;
    return [];
  }
}

/** The last parsed library, keyed by the exact bytes it was parsed from. */
let readCache: { raw: string; parsed: SavedDebate[] } | null = null;

function validate(raw: string): SavedDebate[] {
  const parsed: unknown = JSON.parse(raw);
  if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (d): d is SavedDebate => {
        if (!d || typeof d !== "object") return false;
        const s = d as SavedDebate;
        if (typeof s.id !== "string" || !s.state || typeof s.state !== "object") return false;
        const st = s.state as unknown as Record<string, unknown>;
        // A half-written or corrupt record must not blank the whole window when the
        // reader or exporter walks it. This check used to stop at the top level,
        // while three separate places assume more than it verified: `HomeView` and
        // `ReaderView` read `debate.summary.motion`, `Panels` reads `speech.actions`
        // for every record, and `toMarkdown` reads `speech.reads` and `point.beats`.
        // One entry missing any of those blanked the whole app — there is no error
        // boundary — while reading as a perfectly good library.
        if (!s.summary || typeof s.summary !== "object") return false;
        const sum = s.summary as unknown as Record<string, unknown>;
        if (typeof sum.motion !== "string") return false;
        if (typeof sum.unfinished !== "boolean") return false;
        if (typeof sum.rounds !== "number") return false;
        if (
          !(
            Array.isArray(st.points) &&
            Array.isArray(st.speeches) &&
            Array.isArray(st.delegates) &&
            Array.isArray(st.polls)
          )
        ) {
          return false;
        }
        if (typeof st.tallies !== "object" || st.tallies === null) return false;
        if (st.reads !== undefined && (typeof st.reads !== "object" || st.reads === null)) {
          return false;
        }
        // Every speech needs the arrays the renderers index into.
        for (const sp of st.speeches as unknown[]) {
          if (!sp || typeof sp !== "object") return false;
          const r = sp as Record<string, unknown>;
          if (!Array.isArray(r.actions)) return false;
        }
        // And every point the ones the exporter walks.
        for (const pt of st.points as unknown[]) {
          if (!pt || typeof pt !== "object") return false;
          const r = pt as Record<string, unknown>;
          if (!Array.isArray(r.beats)) return false;
        }
        return true;
      },
    );
}

function write(all: SavedDebate[]): boolean {
  try {
    window.localStorage.setItem(KEY, JSON.stringify(all));
    return true;
  } catch {
    return false;
  }
}

export function listDebates(): SavedDebate[] {
  // A copy: `sort` works in place, and the array belongs to the read cache.
  return [...read()].sort((a, b) => b.savedAt - a.savedAt);
}

/** How many debates the library holds, and how many it is willing to hold. */
export function libraryCapacity(): { kept: number; max: number } {
  return { kept: read().length, max: MAX_KEPT };
}

export function loadDebate(id: string): SavedDebate | null {
  return read().find((d) => d.id === id) ?? null;
}

export function deleteDebate(id: string): void {
  write(read().filter((d) => d.id !== id));
}

export function newDebateId(): string {
  return `d${Date.now().toString(36)}${Math.random().toString(36).slice(2, 7)}`;
}

const labelOf = (state: CouncilState, id: string) =>
  state.delegates.find((d) => d.id === id)?.label ?? id;

export function summarise(state: CouncilState): DebateSummary {
  // One pass, not four filters: this runs on every library save, and the
  // points list only grows.
  let carried = 0;
  let fell = 0;
  let stalemate = 0;
  let withdrawn = 0;
  for (const p of state.points) {
    switch (state.tallies[p.id]?.status) {
      case "carried":
        carried += 1;
        break;
      case "fell":
        fell += 1;
        break;
      case "stalemate":
        stalemate += 1;
        break;
      case "withdrawn":
        withdrawn += 1;
        break;
    }
  }
  return {
    motion: state.motion,
    rounds: Math.max(state.round, state.verdict?.rounds ?? 0),
    roundsConcluded: state.verdict?.rounds ?? 0,
    carried,
    fell,
    stalemate,
    withdrawn,
    points: state.points.length,
    speeches: state.speeches.length,
    unfinished: !state.verdict,
    endedHow: state.verdict?.endedHow ?? null,
    closedBy: state.verdict?.adjournedBy ? labelOf(state, state.verdict.adjournedBy) : null,
    // Model names only. The endpoint that served them, and its key, are not
    // part of the record.
    seats: state.delegates.map((d) => ({ label: d.label, model: d.model, role: d.role })),
    answer: state.verdict?.conclusion?.answer?.trim() || null,
  };
}

/**
 * Write the debate into the library, replacing any earlier copy of the same id.
 *
 * `localStorage` is about 5MB and reasoning text is the bulk of a debate, so
 * this sheds the oldest entries rather than failing. On a hard failure it trims
 * the current debate's *thinking* — never its speeches — because a record
 * without the argument is not a record.
 *
 * Returns false when even that was not enough, so the caller can say so.
 */
export function saveDebate(state: CouncilState, id: string): boolean {
  const clean = stripForSave(state);
  const summary = summarise(clean);
  const now = Date.now();
  const entry: SavedDebate = { id, savedAt: now, summary, state: clean };

  let all = read().filter((d) => d.id !== id);
  all.push(entry);

  // Keep the newest MAX_KEPT, always including this one.
  if (all.length > MAX_KEPT) {
    all = all.sort((a, b) => a.savedAt - b.savedAt).slice(-MAX_KEPT);
  }
  if (write(all)) return true;

  // Out of room. Drop the oldest, and if that is not enough, shed thinking
  // from the oldest entries first — the speeches are what the debate *was*.
  // Rebuilt, not mutated: `all` shares its entry objects with the read cache,
  // so assigning into one would corrupt later reads of bytes that never
  // changed.
  all = all.sort((a, b) => a.savedAt - b.savedAt).slice(-Math.max(1, MAX_KEPT >> 1));
  for (let i = 0; i < all.length - 1 && !write(all); i += 1) {
    // Drop the oldest, then shed thinking from the new oldest. Rebuilt, not
    // mutated (see above).
    const [, ...rest] = all;
    const next = rest[0]!;
    all = [
      { ...next, state: { ...next.state, speeches: withoutReasoning(next.state.speeches) } },
      ...rest.slice(1),
    ];
  }
  if (write(all)) return true;

  // Strip the debate being saved, not the oldest one. `all` is sorted oldest-first,
  // so the last resort used to strip somebody else's record while the current
  // debate — the one holding six 2.5 MB images — stayed too big to save.
  const newest = all.find((d) => d.id === id);
  if (!newest) return false;
  return write(
    all.map((d) =>
      d.id === id
        ? { ...d, state: { ...d.state, speeches: withoutReasoning(d.state.speeches) } }
        : d,
    ),
  );
}

function withoutReasoning(speeches: CouncilState["speeches"]): CouncilState["speeches"] {
  return speeches.map((s) => (s.reasoning ? { ...s, reasoning: "" } : s));
}

// ---------------------------------------------------------------------------
// taking one out
// ---------------------------------------------------------------------------

export function toJson(d: SavedDebate): string {
  return JSON.stringify({ format: "council-debate", version: 1, ...d }, null, 2);
}

function mdEscape(s: string): string {
  return s.replace(/\|/g, "\\|").replace(/\n+/g, " ").trim();
}

function score(t: Tally | undefined): string {
  if (!t) return "—";
  if (t.score > 0) return `+${t.score}`;
  return String(t.score);
}

const STATUS_WORD: Record<string, string> = {
  carried: "carried",
  fell: "fell",
  stalemate: "stalemate",
  withdrawn: "withdrawn",
  open: "open",
};

/**
 * The readable record: motion, how it ended, the answer, the scored board, every
 * speech grouped by round, the polls, and the pages anyone read.
 */
export function toMarkdown(d: SavedDebate): string {
  const s = d.state;
  const sum = d.summary;
  const out: string[] = [];

  out.push(`# ${sum.motion || "untitled motion"}`, "");

  const meta: string[] = [];
  meta.push(
    sum.unfinished
      ? `unfinished — ${sum.rounds} round${sum.rounds === 1 ? "" : "s"} spoken`
      : `${sum.endedHow ?? "ended"} after ${sum.roundsConcluded} round${
          sum.roundsConcluded === 1 ? "" : "s"
        }${sum.closedBy ? `, closed by ${sum.closedBy}` : ""}`,
  );
  meta.push(
    `${sum.carried} carried · ${sum.fell} fell · ${sum.stalemate} tied${
      sum.withdrawn ? ` · ${sum.withdrawn} withdrawn` : ""
    }`,
  );
  meta.push(`${sum.points} points · ${sum.speeches} speeches`);
  out.push(`*${meta.join(" · ")}*`, "");
  out.push(
    `*Council: ${sum.seats.map((x) => `${x.label} (${x.model || "no model"})`).join(", ")}*`,
    "",
  );

  // Names only: bytes never reach the library, so the record says what was
  // argued over without carrying it.
  const attached = s.images ?? [];
  if (attached.length) {
    out.push(`*Images argued over: ${attached.map((x) => x.name).join(", ")}*`, "");
  }

  // Present whenever there is a conclusion, not whenever the answer text is
  // non-empty. When the council did not agree the answer is empty *by design*
  // and the lean is the entire finding — keying this on the answer dropped the
  // one thing a reader of an undecided debate came for.
  const c = s.verdict?.conclusion;
  if (c) {
    out.push("## Answer", "");
    if (c.agreed) {
      out.push(c.answer.trim() || "The council settled the motion, but left the answer unstated.", "");
    } else {
      out.push("The council could not agree on an answer.", "");
      if (c.lean) out.push(`It leaned **${c.lean.trim()}**.`, "");
    }
    if (c.tally) out.push(`*${c.tally}*`, "");
  }
  if (s.verdict?.finalSummary) out.push("## End result", "", s.verdict.finalSummary.trim(), "");
  if (s.verdict?.summary && !s.verdict.finalSummary) {
    out.push("## End result", "", s.verdict.summary.trim(), "");
  }

  if (s.points.length) {
    out.push("## The board", "");
    // Voting seats only: the Summarizer casts no votes, so a column for it
    // reads as a voter that stayed silent rather than a non-voter.
    const seats = s.delegates.filter((x) => x.role !== "summarizer");
    const cols = seats.map((x) => x.label);
    out.push(
      `| | point | status | score | ${cols.join(" | ")} |`,
      `|---|---|---|---|${cols.map(() => "---").join("|")}|`,
    );
    for (const p of s.points) {
      const t = s.tallies[p.id];
      const cells = seats.map((x) => t?.ratings[x.id] ?? "·");
      const suffix: string[] = [];
      if (p.ruling) suffix.push(p.ruling.carries ? "ruled carries" : "ruled falls");
      if (p.review?.outcome) suffix.push(`review: ${p.review.outcome}`);
      if (p.withdrawnReason) suffix.push(`withdrawn: ${p.withdrawnReason}`);
      const statusWord = `${STATUS_WORD[t?.status ?? "open"]}${suffix.length ? ` (${suffix.join("; ")})` : ""}`;
      out.push(
        `| P${p.id} | ${mdEscape(p.text)} | ${statusWord} | ${score(t)} | ${cells.join(
          " | ",
        )} |`,
      );
      if (p.beats.length) {
        const span = ["", `↳ defeats P${p.beats.join(", P")}`, "", "", ...cols.map(() => "")].slice(
          0,
          4 + cols.length,
        );
        out.push(`| ${span.join(" | ")} |`);
      }
    }
    out.push("");
  }

  const polls = s.polls ?? [];
  if (polls.length) {
    out.push("## Polls", "");
    for (const poll of polls) out.push(...pollMarkdown(poll, labelOf(s, poll.openedBy)));
    out.push("");
  }

  const reads = Object.values(s.reads ?? {}).flat();
  if (reads.length) {
    out.push("## Sources read", "");
    for (const r of reads) {
      const where = r.url ? `${r.title} — <${r.url}>` : `${r.title} (search)`;
      out.push(`- ${r.source}: ${where}`);
    }
    out.push("");
  }

  // A speech carries its delegate, not its role, so the summarizer is found by
  // looking the seat up rather than by a field that does not exist.
  const summarizer = s.delegates.find((x) => x.role === "summarizer")?.id;
  const rounds = [...new Set(s.speeches.map((x) => x.round).filter((n) => n > 0))].sort(
    (a, b) => a - b,
  );

  out.push("## Transcript", "");
  for (const round of rounds) {
    out.push(`### Round ${round}`, "");
    for (const sp of s.speeches.filter((x) => x.round === round)) {
      const who = labelOf(s, sp.delegate);
      const phase = sp.delegate === summarizer ? " — the floor" : "";
      out.push(`#### ${who}${phase}`, "");
      if (sp.text.trim()) out.push(sp.text.trim(), "");
      for (const line of sp.actions) out.push(`- ${line}`);
      if (sp.actions.length) out.push("");
      for (const r of (s.reads[sp.id] ?? []) as { url: string; title: string; source: string }[]) {
        if (r.url) out.push(`- read ${r.source}: ${r.title} <${r.url}>`);
      }
    }
  }

  return `${out.join("\n").replace(/\n{3,}/g, "\n\n").trim()}\n`;
}

function pollMarkdown(poll: Poll, by: string): string[] {
  const lines = [`**${poll.question}**`, "", `_opened by ${by}, round ${poll.round}_`, ""];
  const tally = new Map<string, number>();
  for (const b of poll.ballots) tally.set(b.choice, (tally.get(b.choice) ?? 0) + 1);
  for (const opt of poll.options) {
    const n = tally.get(opt) ?? 0;
    lines.push(`- ${mdEscape(opt)} — ${n}`);
  }
  if (!poll.closed) lines.push("", `_no answer yet_`);
  lines.push("");
  return lines;
}

/**
 * Write a debate out to a file.
 *
 * The desktop app goes through the OS save dialog, and the browser keeps the
 * anchor. That order matters: `<a download>` with a blob URL is unreliable in
 * WebKitGTK, where the attribute is ignored and the refused navigation means the
 * button does nothing at all — the failure is silent, so it reads as a dead
 * button rather than as an error.
 *
 * Throws, with a message fit to show, if either route fails. A caller that
 * swallows this gets the original problem back.
 */
export async function download(
  filename: string,
  text: string,
  mime: string,
): Promise<void> {
  if (isDesktop()) {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const { writeTextFile } = await import("@tauri-apps/plugin-fs");
    const ext = extensionOf(filename);
    const path = await save({
      defaultPath: filename,
      filters: [{ name: ext, extensions: [ext] }],
    });
    // Cancelling the dialog is a choice, not a failure.
    if (!path) return;
    await writeTextFile(path, text);
    return;
  }

  const blob = new Blob([text], { type: `${mime};charset=utf-8` });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  // Revoked on the next turn: revoking synchronously can cancel the download in
  // some browsers before it has read the blob.
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function extensionOf(filename: string): string {
  const i = filename.lastIndexOf(".");
  return i < 0 ? "txt" : filename.slice(i + 1);
}

/** Whether this is the desktop shell rather than a browser tab. */
function isDesktop(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * Render one of the two export formats, or explain why it could not be rendered.
 *
 * The formatting used to run straight from the click handler, where a throw on
 * a partly-written record took the whole click with it and left the operator
 * pressing a button that did nothing. An export is the last artifact a debate
 * leaves by hand; it failing quietly is the one outcome worth engineering
 * against.
 */
export function renderExport(d: SavedDebate, format: "md" | "json"): string {
  try {
    return format === "json" ? toJson(d) : toMarkdown(d);
  } catch (e) {
    const why = e instanceof Error ? e.message : String(e);
    throw new Error(`could not render the ${format.toUpperCase()} export: ${why}`);
  }
}

export function slug(motion: string): string {
  const base = motion
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
  return base || "debate";
}

/**
 * One export button's whole job, including saying so when it fails.
 *
 * Every failure here used to be invisible: the formatting ran inside the click
 * handler, and a throw on a partly-written record took the click with it. The
 * operator's only symptom was a button that did nothing, on the one action that
 * gets a debate off the machine.
 */
export async function save(
  filename: string,
  d: SavedDebate,
  format: "md" | "json",
  mime: string,
): Promise<string | null> {
  try {
    await download(filename, renderExport(d, format), mime);
    return null;
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
}
