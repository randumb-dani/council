/**
 * Resuming a debate that did not finish.
 *
 * What a resume can and cannot be is worth being blunt about, because the
 * library decides it.
 *
 * The library keeps a debate's *state* — the board, the speeches, the tallies —
 * and the model **names** of the delegates. It deliberately keeps no
 * `DebateConfig` and no API keys. So a resume cannot put the old board back:
 * that board was built by Rust, validated turn by turn, and exists only in the
 * engine's memory while a debate runs. Restoring a JSON copy of it would be
 * inventing a record and calling it the same one.
 *
 * So a resume is honest about what it is: **the same motion, the same seats,
 * argued again — with the unfinished record handed to everyone up front.**
 * The new debate starts with an empty board and says so; what it inherits is
 * the argument that was had, which is the part a returning operator actually
 * wants back.
 *
 * The operator picks the models. A model name from a debate that ran days ago
 * may not be loaded on the server now, so nothing is pre-selected beyond the
 * name — the setup panel opens with the roster in place and the models blank,
 * which is exactly one decision to make rather than a whole council to rebuild.
 */

import { SEAT_ROLES, type DebateConfig, type DelegateConfig, type SeatRole } from "./types.ts";
import type { CouncilState } from "./useCouncil";
import type { DebateSummary } from "./debates";

/** Where the draft waits between the reader page and the setup panel. */
const KEY = "council.resume.v1";

/** How much of the prior record to carry over.
 *
 * The instruction field is a textarea a person reads and edits, not a
 * transcript to scroll, so this is capped: a six-round debate is far more than
 * fits, and past a point the summaries are worth more than the speeches. The
 * cap is on characters, because that is what the prompt pays for.
 */
const RECORD_CHARS = 6000;

/**
 * Kept only as an alias, for the resume tests and for the one thing resume
 * actually needs: a sensible cap on the plan it proposes.
 *
 * It used to be "must match the setup panel's own rounds slider", which is how
 * two numbers in two files came to disagree — the slider said 20, the loader
 * accepted 100, and a stored 40 pinned to the right-hand end of a 20-wide track.
 * The slider now reads its own bound from `STORED_BOUNDS`, so there is one
 * number. Resume still wants a modest cap, because proposing 100 rounds to
 * someone resuming a debate that reached four is not helpful advice.
 */
export const SETUP_MAX_ROUNDS = 20;

export interface ResumeDraft {
  motion: string;
  /** Labels, roles, model *names* and taken seats — no endpoints, no keys. */
  delegates: { label: string; role: SeatRole; model: string; human: boolean }[];
  /** The record so far, truncated and shaped for a prompt. */
  record: string;
  /** Rounds the old debate actually reached, so the panel can suggest more. */
  roundsSpoken: number;
  /** Saved when the draft was written, for the operator's benefit. */
  fromDebateId: string;
}

/** Turn the unfinished record into prose a model can be handed. */
export function recordForResume(state: CouncilState, summary: DebateSummary): string {
  const lines: string[] = [];
  if (state.motion.trim()) lines.push(`Motion: ${state.motion.trim()}`);
  if (summary.answer) lines.push(`What the previous run concluded: ${summary.answer}`);

  const roster = state.delegates.map((d) => d.label).join(", ");
  if (roster) lines.push(`Who sat: ${roster}`);

  // Oldest first, because the record reads chronologically and — more to the
  // point — because the tail slice below then keeps the *end*. Truncating a
  // newest-first string would keep the beginning and throw away exactly where
  // the argument was cut off.
  const speeches = [...state.speeches].sort((a, b) => a.round - b.round || a.id - b.id);
  const linesOut: string[] = [];
  for (const sp of speeches) {
    const text = sp.text.trim();
    if (!text) continue;
    linesOut.push(`R${sp.round} ${state.delegates.find((d) => d.id === sp.delegate)?.label ?? sp.delegate}: ${text}`);
  }
  let record = linesOut.join("\n\n");
  if (record.length > RECORD_CHARS) {
    record = `…(earlier speeches omitted — ${record.length - RECORD_CHARS} characters)\n\n${record.slice(-RECORD_CHARS)}`;
  }
  return [lines.join("\n"), record && `The record so far:\n\n${record}`].filter(Boolean).join("\n\n");
}

/** Build the draft for an unfinished debate, or `null` if there is nothing to resume. */
export function draftFrom(debateId: string, state: CouncilState, summary: DebateSummary): ResumeDraft | null {
  // Nothing spoken is nothing to continue: the operator can just convene again,
  // and a resume button on an empty card is a button that pretends.
  if (state.speeches.length === 0) return null;
  const delegates = state.delegates.map((d) => ({
    label: d.label,
    role: d.role,
    // The name is a hint, not a selection. The setup panel shows it as the
    // placeholder so the operator can accept it with one click if the server
    // still serves that model.
    model: d.model,
    // A seat the operator held stays held: silently converting it to an
    // automatic seat would hand their arguments to a model with no notice.
    human: d.human === true,
  }));
  if (!delegates.length) return null;
  return {
    motion: state.motion || summary.motion,
    delegates,
    record: recordForResume(state, summary),
    roundsSpoken: summary.rounds,
    fromDebateId: debateId,
  };
}

export function saveResumeDraft(draft: ResumeDraft): boolean {
  try {
    window.localStorage.setItem(KEY, JSON.stringify(draft));
    return true;
  } catch {
    // A full store loses the draft; the debate is still in the library and the
    // operator can re-convene from it. Not worth an error.
    return false;
  }
}

/** Is this actually a draft, field by field?
 *
 * Storage is untrusted input — this module's own header says so, and it names the
 * exact failure: a hand-edited `"temperature": "0.7"` once blanked the whole window
 * because the setup panel threw during render and there is no error boundary in
 * this app. The old check looked at `motion` and nothing else, so `{"motion":"x"}`
 * reached `draft.delegates.map(...)` and threw. `applyResumeDraft` is called from a
 * state initializer, i.e. during render, so this is the one place a bad draft can
 * take the window down.
 */
function isDraft(v: unknown): v is ResumeDraft {
  if (typeof v !== "object" || v === null) return false;
  const d = v as Partial<ResumeDraft>;
  if (typeof d.motion !== "string") return false;
  if (typeof d.record !== "string") return false;
  if (typeof d.roundsSpoken !== "number" || !Number.isFinite(d.roundsSpoken)) return false;
  if (typeof d.fromDebateId !== "string") return false;
  if (!Array.isArray(d.delegates) || d.delegates.length === 0) return false;
  return d.delegates.every(
    (x) =>
      typeof x === "object" &&
      x !== null &&
      typeof (x as { label?: unknown }).label === "string" &&
      typeof (x as { model?: unknown }).model === "string" &&
      typeof (x as { role?: unknown }).role === "string" &&
      SEAT_ROLES.includes((x as { role: string }).role as SeatRole) &&
      (typeof (x as { human?: unknown }).human === "boolean" ||
        typeof (x as { human?: unknown }).human === "undefined"),
  );
}

function readDraft(): ResumeDraft | null {
  try {
    const raw = window.localStorage.getItem(KEY);
    if (!raw) return null;
    const draft: unknown = JSON.parse(raw);
    return isDraft(draft) ? draft : null;
  } catch {
    return null;
  }
}

/** Read the draft *without* consuming it. */
export function peekResumeDraft(): ResumeDraft | null {
  return readDraft();
}

/** Read the draft and clear it.
 *
 * Deliberately not called from a `useState` initializer: React double-invokes those
 * under `StrictMode` and keeps the second result, so the first call consumed the
 * draft and threw it away — which is why resume silently did nothing in dev while
 * working in a production build. Call this after the first commit. */
export function takeResumeDraft(): ResumeDraft | null {
  const draft = readDraft();
  clearResumeDraft();
  return draft;
}

export function clearResumeDraft(): void {
  try {
    window.localStorage.removeItem(KEY);
  } catch {
    /* nothing to clear */
  }
}

/**
 * Fold a draft into a config, leaving the decisions to the operator.
 *
 * The roster is rebuilt rather than patched: delegate ids, colours and endpoint
 * bindings belong to the *old* setup and may not match the one on the machine
 * now. Only the names survive, which is the whole of what is safe to carry.
 */
export function applyResumeDraft(base: DebateConfig, draft: ResumeDraft): DebateConfig {
  const endpointId = base.endpoints[0]?.id ?? "";
  const delegates: DelegateConfig[] = draft.delegates.map((d, i) => ({
    id: `seat-${i + 1}`,
    label: d.label,
    // Blank, deliberately: the operator confirms the model on a server that is
    // running it *now*. A stale name pre-selected is a debate that fails on the
    // first turn with an endpoint error.
    model: "",
    endpointId,
    color: base.delegates[i]?.color ?? base.delegates[0]?.color ?? "#65ccc6",
    seatRole: d.role,
    role: null,
    persona: null,
    temperature: null,
    // A taken seat stays taken (see draftFrom): the operator argued it before
    // and the resume hands them the same seat back. Old drafts written without
    // the field read as automatic, as they always did.
    human: d.human === true,
  }));
  return {
    ...base,
    motion: draft.motion,
    delegates,
    // Its own field, deliberately. This used to overwrite `extraInstructions` — the
    // operator's own standing instructions, which the setup panel saves — so a
    // resume silently replaced them with the transcript and persisted the
    // replacement, meaning the next fresh motion was still being handed a dead
    // debate's argument until someone noticed and cleared a two-row textarea.
    resumeRecord: draft.record,
    // The old run was cut short, so afford one more round than it reached. Clamped
    // to the setup panel's own slider ceiling: 21 read as "rounds, max 21" with the
    // thumb pinned at 20, and the first touch of the slider quietly dropped the plan
    // to 20.
    maxRounds: Math.min(Math.max(draft.roundsSpoken + 1, 2), SETUP_MAX_ROUNDS),
  };
}

/**
 * The record has done its job once the engine has the config.
 *
 * It is not decoration in the setup panel: `prompt.rs` puts it in the opening
 * brief of *every* delegate ("YOU ARE RESUMING A DEBATE THAT DID NOT FINISH…
 * RECORD SO FAR"), and the engine builds that brief once and keeps it for the
 * whole run. So the moment the engine has taken the config, the record is
 * already in the delegates' hands.
 *
 * What it must not do is outlive that run. The setup is saved, so a record left
 * in it came back on the next visit to the setup panel and — the part that
 * actually mattered — rode along into every *unrelated* motion afterwards,
 * telling each new delegate it was resuming a debate it had never heard of.
 * That is the same failure as overwriting `extraInstructions` with the
 * transcript, one level up: `applyResumeDraft` stopped the record leaking into
 * the instructions and it went on leaking into the whole prompt.
 *
 * Pure, and identity-preserving when there is nothing to clear, so the common
 * path allocates nothing and the autosave dependency does not churn. Call it
 * only once the engine has accepted the config: clearing earlier would convene
 * the resumed debate without the record it exists to carry.
 */
export function recordConsumed(config: DebateConfig): DebateConfig {
  return config.resumeRecord ? { ...config, resumeRecord: null } : config;
}