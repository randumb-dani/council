/**
 * The operator's seat: a turn, or a ballot their seat owes the room.
 *
 * The engine parks until this is answered, so the panel is a prompt rather than
 * a badge — the council is waiting on the person reading it. Two shapes share
 * the space because they share the pause: your turn to argue, or your vote on
 * something the room has just asked.
 *
 * Everything the operator does here goes through `submit_turn` /
 * `submit_ballot`, which validate server-side and hand back the reason a call
 * was refused. A refusal is shown where the mistake was made and the turn stays
 * open — the opposite of a model's, which is dropped.
 */

import { memo, useEffect, useRef, useState } from "react";
import { CaretDown } from "@phosphor-icons/react/dist/csr/CaretDown";
import { PaperPlaneTilt } from "@phosphor-icons/react/dist/csr/PaperPlaneTilt";
import { SkipForward } from "@phosphor-icons/react/dist/csr/SkipForward";
import type {
  BallotKind,
  HumanActionCall,
  HumanBallotSubmit,
  HumanTurnSubmit,
  Verdict,
} from "../types";
import type { PendingOperatorPrompt } from "../useCouncil";
import type { Point } from "../types";

const VERDICTS: { value: Verdict; label: string }[] = [
  { value: "win", label: "stands" },
  { value: "lose", label: "falls" },
  { value: "stalemate", label: "cannot be decided" },
];

const BALLOT_TITLE: Record<BallotKind, string> = {
  poll: "A delegate asked a question",
  review: "A point is deadlocked",
  adjourn: "Someone wants to end the debate",
  extension: "The Summarizer asks for more rounds",
};

interface Props {
  prompt: PendingOperatorPrompt;
  points: Point[];
  labelOf: (id: string) => string;
  /** In flight: the engine is applying the answer. */
  submitting: boolean;
  /**
   * The last answer's deadline passed with no reply.
   *
   * The panel has to own this rather than infer it: `submitting` going false is
   * ambiguous (it also happens on a refusal), and without a signal the operator
   * sees the button re-enable and assumes the send simply failed, when it may
   * have been taken and the reply lost. See the guard in `useCouncil.answer`.
   */
  stalled: boolean;
  /** Both resolve true only once the engine has taken the answer. */
  onSubmitTurn: (submit: HumanTurnSubmit) => Promise<boolean>;
  onSubmitBallot: (ballot: HumanBallotSubmit) => Promise<boolean>;
  /** Walk away from this turn without saying anything — as a skipped turn. */
  onSkip: () => void;
  /**
   * Reports whether the operator has unsent work on this turn.
   *
   * The header carries a second Skip button for the same action, and it sits
   * top-right, `btn-ghost`, next to the transcript — a stray click there used to
   * discard a typed-but-unsent speech with no prompt, while the identical action
   * in this panel asked first. The confirmation cannot live in the panel alone if
   * the button that needs it is outside the panel, so the panel publishes the
   * fact and the header decides.
   */
  onDraftChange?: (hasDraft: boolean) => void;
}

/** One action the operator has built, shown back to them as the row it becomes. */
interface Draft {
  key: string;
  tool: string;
  summary: string;
  call: Omit<HumanActionCall, "id">;
}

/**
 * Memoized: the chamber re-renders on every streamed token (~25/s), and this
 * panel sits in that tree. While parked-but-visible it draws nothing that
 * moves with the stream, so re-rendering it per token is pure waste. Props
 * include fresh inline callbacks from App today, so this mainly future-proofs
 * and helps the parked case; inner prompts own their own state regardless.
 */
export const OperatorPanel = memo(function OperatorPanel({
  prompt,
  points,
  labelOf,
  submitting,
  stalled,
  onSubmitTurn,
  onSubmitBallot,
  onSkip,
  onDraftChange,
}: Props) {
  // Keyed by the prompt itself: a new turn or ballot remounts with a clean
  // draft, so last turn's speech cannot leak into this one and a ballot that
  // happens to share an option string with the previous one does not arrive
  // pre-selected.
  if (prompt.kind === "ballot") {
    // A ballot owes no speech of the operator's own, so there is nothing for
    // the header's Skip to guard. Reset in an effect rather than during render:
    // writing to a ref while rendering is a side effect React may replay.
    useEffect(() => {
      onDraftChange?.(false);
    }, [onDraftChange, prompt.delegate, prompt.ballot]);
    return (
      <BallotPrompt
        key={`ballot:${prompt.delegate}:${prompt.ballot}:${prompt.pointId ?? "na"}:${prompt.question}`}
        prompt={prompt}
        labelOf={labelOf}
        submitting={submitting}
        stalled={stalled}
        onSubmit={onSubmitBallot}
      />
    );
  }
  return (
    <TurnPrompt
      key={`turn:${prompt.delegate}:${prompt.turn}`}
      prompt={prompt}
      points={points}
      submitting={submitting}
      stalled={stalled}
      onSubmit={onSubmitTurn}
      onSkip={onSkip}
      onDraftChange={onDraftChange}
    />
  );
});

// ---------------------------------------------------------------------------
// the turn
// ---------------------------------------------------------------------------

function TurnPrompt({
  prompt,
  points,
  submitting,
  stalled,
  onSubmit,
  onSkip,
  onDraftChange,
}: {
  prompt: Extract<PendingOperatorPrompt, { kind: "turn" }>;
  points: Point[];
  submitting: boolean;
  /** The last answer's deadline passed without a reply. */
  stalled: boolean;
  /** Resolves true only once the engine has taken the turn. */
  onSubmit: (submit: HumanTurnSubmit) => Promise<boolean>;
  onSkip: () => void;
  onDraftChange?: (hasDraft: boolean) => void;
}) {
  // A draft the operator typed is theirs until the engine takes it: it is keyed
  // to this exact turn, restored after a reload, and cleared only on acceptance.
  // Skipping or reloading used to delete it silently along with the panel.
  const draftKey = `council.draft.${prompt.delegate}.${prompt.turn}`;
  const loadDraft = (): { speech: string; thinking: string } => {
    try {
      const raw = window.sessionStorage.getItem(draftKey);
      if (!raw) return { speech: "", thinking: "" };
      const v = JSON.parse(raw) as { speech?: unknown; thinking?: unknown };
      return {
        speech: typeof v.speech === "string" ? v.speech : "",
        thinking: typeof v.thinking === "string" ? v.thinking : "",
      };
    } catch {
      return { speech: "", thinking: "" };
    }
  };
  const [speech, setSpeech] = useState(() => loadDraft().speech);
  const [thinking, setThinking] = useState(() => loadDraft().thinking);
  const [showThinking, setShowThinking] = useState(false);
  const [showBrief, setShowBrief] = useState(true);
  const [drafts, setDrafts] = useState<Draft[]>([]);

  // Debounced trailing write (~500ms): typing fires per keystroke, and a
  // synchronous sessionStorage write per keystroke is wasted work. The refs
  // carry the latest text so the unmount flush below never loses a draft,
  // even if the window closes inside the debounce window.
  const speechRef = useRef(speech);
  speechRef.current = speech;
  const thinkingRef = useRef(thinking);
  thinkingRef.current = thinking;
  const draftKeyRef = useRef(draftKey);
  draftKeyRef.current = draftKey;
  useEffect(() => {
    const t = window.setTimeout(() => {
      try {
        window.sessionStorage.setItem(
          draftKey,
          JSON.stringify({ speech, thinking }),
        );
      } catch {
        /* storage full or unavailable; the in-memory draft still stands */
      }
    }, 500);
    return () => window.clearTimeout(t);
  }, [draftKey, speech, thinking]);
  // Immediate write on unmount: never lose a draft typed just before the
  // panel goes away. Submit/skip paths clear via clearDraft instead.
  useEffect(() => {
    return () => {
      try {
        window.sessionStorage.setItem(
          draftKeyRef.current,
          JSON.stringify({ speech: speechRef.current, thinking: thinkingRef.current }),
        );
      } catch {
        /* storage full or unavailable; the in-memory draft still stands */
      }
    };
  }, []);

  // Mirrored up to the header, which carries a second Skip for the same action.
  // Staged moves count: a `make_point` the operator composed but has not sent is
  // just as lost by a stray Skip as an unsent sentence.
  const hasDraft = speech.trim() !== "" || thinking.trim() !== "" || drafts.length > 0;
  useEffect(() => {
    onDraftChange?.(hasDraft);
  }, [onDraftChange, hasDraft]);

  const clearDraft = () => {
    // Reset the flush refs too, so the unmount write below cannot resurrect
    // a draft that was deliberately cleared on accept/skip.
    speechRef.current = "";
    thinkingRef.current = "";
    try {
      window.sessionStorage.removeItem(draftKey);
    } catch {
      /* already gone */
    }
  };

  const send = async () => {
    if (submitting) return;
    const taken = await onSubmit({
      text: speech,
      thinking,
      // Ids are the engine's business; the operator never names one, and every
      // call goes through the same validation a model's.
      calls: drafts.map((d, i) => ({ ...d.call, id: `human-${i}` })),
    });
    // Cleared on acceptance only. This used to clear unconditionally, which is the
    // opposite of what this panel promises — a refusal is shown where the mistake
    // was made and the turn stays open, not a 300-word speech deleted because one
    // staged move named a field wrongly. The engine refuses a bad call by refusing
    // the whole turn, so this was the *normal* path, not an edge case.
    if (!taken) return;
    clearDraft();
    setSpeech("");
    setThinking("");
    setDrafts([]);
  };
  const add = (d: Draft) => setDrafts((prev) => [...prev, d]);
  const drop = (key: string) => setDrafts((prev) => prev.filter((d) => d.key !== key));
  const skip = () => {
    // Skipping throws the turn away by design, but a typed-but-unsent speech is
    // not "saying nothing" — it is work. Confirm rather than delete silently.
    if (hasDraft && !window.confirm("Skip this turn? What you typed will be discarded.")) return;
    clearDraft();
    onSkip();
  };

  /* Focus the heading when this turn appears.
   *
   * The engine parks until the operator answers, so the panel's arrival is the
   * one moment in the app where the user's attention is genuinely required —
   * and it was silent. Focus stayed wherever it was, usually in the note box at
   * the bottom of the chamber, where Enter sends a broadcast note to every
   * delegate rather than the turn the room is waiting on.
   *
   * The heading takes focus rather than the textarea: the brief above is long,
   * and focusing the textarea scrolls past it and past the heading, so the
   * operator arrives at a blinking cursor with no idea whose turn it is. The
   * heading is announced, and it is one Tab from the box. This mirrors what the
   * extension modal already does on open.
   */
  const headingRef = useRef<HTMLHeadingElement | null>(null);
  useEffect(() => {
    headingRef.current?.focus();
  }, [prompt.delegate, prompt.turn]);

  return (
    <section
      aria-label="your turn"
      /* `role="region"` so the label is exposed, and the heading inside is the
         accessible name for it. The bare `aria-label` on a plain section was
         decorative in practice.
         
         Focus moves here when the panel appears, because the engine *parks* until
         the operator answers — the debate cannot continue without them. Nothing
         announced that: with focus sitting in the note box at the bottom of the
         chamber, Enter there sent a broadcast note the parked seat did not want,
         and a screen reader user had no signal at all that the room was waiting
         on them. See the effect above. */
      role="region"
      className="shrink-0 border-b border-primary/40 bg-primary-soft/40"
    >
      <div className="safe-x mx-auto w-full max-w-[88ch] px-4 py-3 sm:px-5">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <span className="size-2 shrink-0 rounded-full bg-primary live-dot" aria-hidden />
          <h2
            ref={headingRef}
            tabIndex={-1}
            /* Focusable but not in the tab order, and outlined while focused: it
               has to be visibly focused or moving focus here looks like nothing
               happened. The stock `outline-none` would hide the one visible cue
               that the room is now waiting on the operator. */
            className="text-[13px] font-semibold text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
          >
            Your turn — you hold the {prompt.label} seat
          </h2>
          <span className="text-[11px] text-ink-3">round {prompt.round}</span>
          <div className="flex-1" />
          <button
            type="button"
            className="btn btn-ghost"
            onClick={skip}
            disabled={submitting}
            title="Say nothing and move on, as a skipped turn would"
          >
            <SkipForward size={13} /> skip my turn
          </button>
        </div>

        {/* The brief is what a model would have been shown, so the human argues
            from the same record. Long by design, hence collapsible. */}
        <div className="mt-2">
          <button
            type="button"
            className="flex items-center gap-1 text-[11px] text-ink-3 hover:text-ink-2"
            onClick={() => setShowBrief((s) => !s)}
            aria-expanded={showBrief}
          >
            <CaretDown
              size={11}
              style={{ transform: showBrief ? undefined : "rotate(-90deg)" }}
            />
            {showBrief ? "hide what the council can see" : "show what the council can see"}
          </button>
          {showBrief && (
            <pre className="mt-1 max-h-48 overflow-auto whitespace-pre-wrap rounded-lg border border-line bg-surface px-3 py-2 font-mono text-[11px] leading-relaxed text-ink-2 [overflow-wrap:anywhere]">
              {prompt.brief}
            </pre>
          )}
        </div>

        <div className="mt-2 flex flex-col gap-3">
          <ActionPicker
            points={points}
            drafts={drafts}
            onAdd={add}
            onDrop={drop}
          />

          <textarea
            /* Named, so a screen reader announces what the box is for rather
               than only its placeholder — and placeholders vanish on focus,
               which is most of the time it is being used. */
            aria-label="your speech on the record"
            className="field min-h-[70px] w-full resize-y text-[13px]"
            value={speech}
            placeholder="What you say on the record. The other delegates read this next turn."
            onChange={(e) => setSpeech(e.target.value)}
          />

          <div>
            <button
              type="button"
              className="text-[11px] text-ink-3 hover:text-ink-2"
              onClick={() => setShowThinking((s) => !s)}
              aria-expanded={showThinking}
            >
              {showThinking ? "hide your reasoning" : "add your reasoning (shown, never sent to a model)"}
            </button>
            {showThinking && (
              <textarea
                className="field mt-1 min-h-[50px] w-full resize-y font-mono !text-[12px]"
                value={thinking}
                placeholder="How you reached this. Kept on your speech, never fed to anyone's history."
                onChange={(e) => setThinking(e.target.value)}
              />
            )}
          </div>

          <div className="flex flex-wrap items-center gap-3">
            <button
              type="button"
              className="btn btn-primary"
              disabled={submitting || (!speech.trim() && !thinking.trim() && drafts.length === 0)}
              onClick={send}
            >
              <PaperPlaneTilt size={14} />{" "}
              {submitting ? "sending…" : stalled ? "send again" : "say it"}
            </button>
            {drafts.length > 0 && (
              <span className="text-[11px] text-ink-3">
                {drafts.length} move{drafts.length === 1 ? "" : "s"} will be applied
              </span>
            )}
            {stalled && (
              /* The request never came back, so whether the engine took it is
                 genuinely unknown — the button re-enabled itself, but "say it"
                 again reads as a first attempt. Say what happened and that a
                 duplicate is harmless. */
              <span className="text-[11px] leading-relaxed text-warn">
                no reply from the server. If the council has already moved on, this was
                taken; if it is still waiting on you, send again.
              </span>
            )}
          </div>
        </div>
      </div>
    </section>
  );
}

// ---------------------------------------------------------------------------
// the action picker
// ---------------------------------------------------------------------------

/** Which move is being built. One at a time: a vote needs a point, a rebuttal
 *  needs a point, and stacking three open forms would be a wall. */
type Move =
  | "vote"
  | "make_point"
  | "concede"
  | "steelman"
  | "rebut"
  | "cite"
  | "press"
  | "poll"
  | "adjourn";

const MOVES: { key: Move; label: string; hint: string }[] = [
  { key: "vote", label: "vote on a point", hint: "rate a point: it stands, it falls, or the record cannot decide it" },
  { key: "make_point", label: "raise a point", hint: "put a new falsifiable claim on the board" },
  { key: "concede", label: "concede", hint: "give a point up, or retract one of your own" },
  { key: "steelman", label: "steelman", hint: "restate another delegate's point in its strongest form" },
  { key: "rebut", label: "rebut", hint: "attack a specific point with a counter-argument" },
  { key: "cite", label: "cite", hint: "put named evidence on the record" },
  { key: "press", label: "press", hint: "demand a direct answer from a named delegate" },
  { key: "poll", label: "poll", hint: "put a fixed set of options to the room" },
  { key: "adjourn", label: "adjourn", hint: "argue the record is exhausted and ask to end" },
];

function ActionPicker({
  points,
  drafts,
  onAdd,
  onDrop,
}: {
  points: Point[];
  drafts: Draft[];
  onAdd: (d: Draft) => void;
  onDrop: (key: string) => void;
}) {
  const [open, setOpen] = useState<Move | null>(null);
  // Staged moves list above the picker rather than replacing it: `send` already
  // submits every staged call at once, but the old early return hid the move
  // buttons after the first draft, so a second move was unreachable.
  return (
    <div className="flex flex-col gap-1.5">
      {drafts.length > 0 && (
        <div className="flex flex-col gap-1 rounded-lg border border-line bg-surface px-2.5 py-2">
          {drafts.map((d) => (
            <div key={d.key} className="flex items-baseline gap-2 text-[12px]">
              <span className="shrink-0 font-semibold text-ink">{d.tool}</span>
              <span className="min-w-0 flex-1 text-ink-2">{d.summary}</span>
              <button
                type="button"
                className="shrink-0 text-[11px] text-ink-3 hover:text-bad"
                onClick={() => onDrop(d.key)}
              >
                remove
              </button>
            </div>
          ))}
        </div>
      )}
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="text-[11px] text-ink-3">act:</span>
        {MOVES.map((m) => (
          <button
            key={m.key}
            type="button"
            className={`btn !py-1 text-[11.5px] ${open === m.key ? "!bg-surface-3" : ""}`}
            onClick={() => setOpen(open === m.key ? null : m.key)}
            aria-expanded={open === m.key}
            title={m.hint}
            disabled={points.length === 0 && NEEDS_POINT.has(m.key)}
          >
            {m.label}
          </button>
        ))}
        {points.length === 0 && (
          <span className="text-[11px] text-ink-3">raise a point first — votes need one</span>
        )}
        {open && (
          <MoveForm
            key={open}
            move={open}
            points={points}
            onCancel={() => setOpen(null)}
            onAdd={(d) => {
              onAdd(d);
              setOpen(null);
            }}
          />
        )}
      </div>
    </div>
  );
}

const NEEDS_POINT = new Set<Move>(["vote", "concede", "steelman", "rebut"]);

function MoveForm({
  move,
  points,
  onCancel,
  onAdd,
}: {
  move: Move;
  points: Point[];
  onCancel: () => void;
  onAdd: (d: Draft) => void;
}) {
  const [point, setPoint] = useState(points[0]?.id ?? 1);
  const [text, setText] = useState("");
  const [verdict, setVerdict] = useState<Verdict>("win");
  const [reason, setReason] = useState("");
  const [question, setQuestion] = useState("");
  const [options, setOptions] = useState("");
  const [tag, setTag] = useState<"supports" | "undermines" | "neutral">("supports");
  const [who, setWho] = useState("");

  const commit = () => {
    const key = `${move}-${Math.random().toString(36).slice(2)}`;
    switch (move) {
      case "vote":
        onAdd({
          key,
          tool: "vote",
          summary: `P${point} — ${VERDICTS.find((v) => v.value === verdict)?.label}${reason ? `: ${reason}` : ""}`,
          call: {
            name: "vote",
            arguments: { point: Number(point), verdict, ...(reason.trim() ? { reason: reason.trim() } : {}) },
          },
        });
        return;
      case "make_point":
        onAdd({
          key,
          tool: "make_point",
          summary: text,
          call: { name: "make_point", arguments: { text } },
        });
        return;
      case "concede":
        onAdd({
          key,
          tool: "concede",
          summary: `P${point}${reason ? `: ${reason}` : ""}`,
          call: {
            name: "concede",
            arguments: { point: Number(point), ...(reason.trim() ? { reason: reason.trim() } : {}) },
          },
        });
        return;
      case "steelman":
        onAdd({
          key,
          tool: "steelman",
          summary: `P${point}: ${text}`,
          call: { name: "steelman", arguments: { point: Number(point), restatement: text } },
        });
        return;
      case "rebut":
        onAdd({
          key,
          tool: "rebut",
          summary: `P${point}: ${text}`,
          call: { name: "rebut", arguments: { point: Number(point), text } },
        });
        return;
      case "cite":
        onAdd({
          key,
          tool: "cite",
          summary: `${tag}: ${text}`,
          // `source`, not `evidence`: `parse_action` looks the field up under
          // `source | citation | reference | title` only, so `evidence` came back as
          // "cite needs `source`" — and because one bad call refuses the whole
          // submit, a perfectly good speech went with it.
          call: { name: "cite", arguments: { source: text, stance: tag } },
        });
        return;
      case "press":
        onAdd({
          key,
          tool: "press",
          summary: `${who}${reason ? ` — ${reason}` : ""}`,
          call: {
            name: "press",
            // `question` is required by `parse_action`, and the reason box was
            // labelled optional — so pressing a seat with no reason built a call the
            // engine rejects, which refuses the entire turn.
            arguments: {
              delegate: who,
              question: reason.trim() || `What is your evidence that ${who} is wrong?`,
            },
          },
        });
        return;
      case "poll":
        onAdd({
          key,
          tool: "poll",
          summary: `${question} (${options.split(",").filter((s) => s.trim()).join(" / ")})`,
          call: {
            name: "poll",
            arguments: {
              question,
              options: options.split(",").map((s) => s.trim()).filter(Boolean),
            },
          },
        });
        return;
      case "adjourn":
        onAdd({
          key,
          tool: "adjourn",
          summary: reason || "the record is exhausted",
          call: { name: "adjourn", arguments: { reason: reason.trim() } },
        });
        return;
    }
  };

  const needPoint = NEEDS_POINT.has(move);
  const needText = move !== "vote" && move !== "press" && move !== "poll" && move !== "adjourn";
  const blocked =
    (needPoint && !points.some((p) => p.id === Number(point))) ||
    (needText && !text.trim()) ||
    (move === "press" && !who.trim()) ||
    (move === "poll" && (!question.trim() || options.split(",").filter((s) => s.trim()).length < 2));
  // A disabled button with no reason reads as broken. Say what is missing.
  const blockReason = !blocked
    ? null
    : needPoint && !points.some((p) => p.id === Number(point))
      ? "pick a point above first"
      : needText && !text.trim()
        ? "write the text first"
        : move === "press" && !who.trim()
          ? "name the delegate to press"
          : move === "poll"
            ? "a poll needs a question and at least two options"
            : null;

  return (
    <div className="mt-1 flex w-full flex-col gap-2 rounded-lg border border-line bg-surface px-2.5 py-2">
      {needPoint && (
        <label className="flex items-center gap-2">
          <span className="shrink-0 text-[11px] text-ink-3">point</span>
          <select className="field !py-1 text-[12px]" value={point} onChange={(e) => setPoint(Number(e.target.value))}>
            {points.map((p) => (
              <option key={p.id} value={p.id}>
                P{p.id}: {p.text.slice(0, 80)}
              </option>
            ))}
          </select>
        </label>
      )}
      {move === "vote" && (
        <div className="flex flex-wrap items-center gap-2">
          {VERDICTS.map((v) => (
            <button
              key={v.value}
              type="button"
              className={`btn !py-1 text-[11.5px] ${verdict === v.value ? "!bg-surface-3" : ""}`}
              onClick={() => setVerdict(v.value)}
              aria-pressed={verdict === v.value}
            >
              {v.label}
            </button>
          ))}
        </div>
      )}
      {move === "cite" && (
        <div className="flex flex-wrap items-center gap-2">
          {(["supports", "undermines", "neutral"] as const).map((t) => (
            <button
              key={t}
              type="button"
              className={`btn !py-1 text-[11.5px] ${tag === t ? "!bg-surface-3" : ""}`}
              onClick={() => setTag(t)}
              aria-pressed={tag === t}
            >
              {t}
            </button>
          ))}
        </div>
      )}
      {move === "press" && (
        <label className="flex items-center gap-2">
          <span className="shrink-0 text-[11px] text-ink-3">ask</span>
          <input
            className="field !py-1 text-[12px]"
            value={who}
            placeholder="delegate label"
            onChange={(e) => setWho(e.target.value)}
          />
        </label>
      )}
      {move === "poll" && (
        <>
          <input
            className="field !py-1 text-[12px]"
            value={question}
            placeholder="the question"
            onChange={(e) => setQuestion(e.target.value)}
          />
          <input
            className="field !py-1 text-[12px]"
            value={options}
            placeholder="options, comma separated"
            onChange={(e) => setOptions(e.target.value)}
          />
        </>
      )}
      {needText && (
        <textarea
          className="field min-h-[52px] w-full resize-y text-[12px]"
          value={text}
          placeholder={
            move === "make_point"
              ? "the claim, as one falsifiable sentence"
              : move === "steelman"
                ? "the strongest form of their point"
                : move === "cite"
                  ? "the evidence, named"
                  : "your counter-argument"
          }
          onChange={(e) => setText(e.target.value)}
        />
      )}
      {(move === "vote" || move === "concede" || move === "adjourn" || move === "press") && (
        <input
          className="field !py-1 text-[12px]"
          value={reason}
          // `press` now always sends a question, so say so: the label said
          // "optional" while the field was in fact required to be non-empty for the
          // move to be accepted at all.
          placeholder={
            move === "adjourn"
              ? "why the record is exhausted"
              : move === "press"
                ? "what you want answered"
                : "why (optional)"
          }
          onChange={(e) => setReason(e.target.value)}
        />
      )}
      <div className="flex items-center gap-2">
        <button type="button" className="btn btn-primary !py-1 text-[12px]" disabled={blocked} onClick={commit}>
          add this move
        </button>
        <button type="button" className="btn btn-ghost !py-1 text-[12px]" onClick={onCancel}>
          cancel
        </button>
        {blocked && blockReason && (
          <span className="text-[11px] text-ink-3">{blockReason}</span>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// the ballot
// ---------------------------------------------------------------------------

function BallotPrompt({
  prompt,
  labelOf,
  submitting,
  stalled,
  onSubmit,
}: {
  prompt: Extract<PendingOperatorPrompt, { kind: "ballot" }>;
  labelOf: (id: string) => string;
  submitting: boolean;
  stalled: boolean;
  onSubmit: (ballot: HumanBallotSubmit) => Promise<boolean>;
}) {
  const [reason, setReason] = useState("");
  const [reviewVote, setReviewVote] = useState<Verdict | null>(null);
  const [picked, setPicked] = useState<string | null>(null);

  const answered = () => setReason("");

  /* Same reasoning as the turn panel: the engine is parked on this ballot, so
     its arrival needs the focus. The heading, not the first option button —
     focus on an option invites Enter, and Enter on a pre-surveyed button is a
     vote cast without the question being read. */
  const headingRef = useRef<HTMLHeadingElement | null>(null);
  useEffect(() => {
    headingRef.current?.focus();
  }, [prompt.delegate, prompt.ballot, prompt.question]);

  return (
    <section
      role="region"
      aria-label="your ballot"
      className="shrink-0 border-b border-primary/40 bg-primary-soft/40"
    >
      <div className="safe-x mx-auto w-full max-w-[88ch] px-4 py-3 sm:px-5">
        <div className="flex items-center gap-3">
          <span className="size-2 shrink-0 rounded-full bg-primary live-dot" aria-hidden />
          <h2
            ref={headingRef}
            tabIndex={-1}
            className="text-[13px] font-semibold text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
          >
            {BALLOT_TITLE[prompt.ballot]} — the {labelOf(prompt.delegate)} seat is yours to answer
          </h2>
        </div>
        <p className="mt-1.5 whitespace-pre-wrap text-[12.5px] leading-relaxed text-ink-2">
          {prompt.question}
        </p>

        <div className="mt-2 flex flex-wrap gap-2">
          {prompt.options.map((o) => (
            <button
              key={o}
              type="button"
              className={`btn !py-1 text-[12px] ${picked === o ? "!bg-surface-3" : ""}`}
              onClick={() => setPicked(o)}
              aria-pressed={picked === o}
            >
              {o}
            </button>
          ))}
        </div>

        {/* A review may change the vote on the point in the same breath — the
            sanctioned moment to change your mind — so it is offered here and
            nowhere else. */}
        {prompt.ballot === "review" && (
          <div className="mt-2 flex flex-wrap items-center gap-2">
            <span className="text-[11px] text-ink-3">change your vote on this point?</span>
            {VERDICTS.map((v) => (
              <button
                key={v.value}
                type="button"
                className={`btn !py-1 text-[11.5px] ${reviewVote === v.value ? "!bg-surface-3" : ""}`}
                onClick={() => setReviewVote(reviewVote === v.value ? null : v.value)}
                aria-pressed={reviewVote === v.value}
              >
                {v.label}
              </button>
            ))}
          </div>
        )}

        <input
          className="field mt-2 !py-1 text-[12px]"
          aria-label="why (optional)"
          value={reason}
          placeholder="why (optional)"
          onChange={(e) => setReason(e.target.value)}
        />

        <div className="mt-2 flex items-center gap-2">
          <button
            type="button"
            className="btn btn-primary !py-1 text-[12px]"
            disabled={submitting || !picked}
            onClick={async () => {
              const r = reason.trim();
              const taken = await onSubmit(
                prompt.ballot === "poll"
                  ? { kind: "poll", choice: picked!, reason: r || null, confidence: null }
                  : prompt.ballot === "review"
                    ? { kind: "review", reading: picked!, reason: r || null, vote: reviewVote }
                    : prompt.ballot === "adjourn"
                      ? { kind: "adjourn", confirm: picked === "yes", reason: r || null }
                      : { kind: "extension", grant: picked === "grant", reason: r || null },
              );
              // Same rule as the turn: a refused ballot keeps its reason so it can be
              // corrected, rather than clearing the box the operator just typed in.
              if (!taken) return;
              answered();
            }}
          >
            {submitting ? "answering…" : stalled ? "answer again" : "answer"}
          </button>
          {!picked && <span className="text-[11px] text-ink-3">choose an answer</span>}
          {stalled && (
            /* Same unknown as the turn: the reply never arrived, so this may or
               may not have been recorded. A duplicate ballot is answered by the
               engine with "that ballot is already in", which resolves it. */
            <span className="text-[11px] leading-relaxed text-warn">
              no reply from the server. If the room has moved on, this was recorded.
            </span>
          )}
        </div>
      </div>
    </section>
  );
}