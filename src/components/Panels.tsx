import { memo, useCallback, useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { CaretDown } from "@phosphor-icons/react/dist/csr/CaretDown";
import { CaretRight } from "@phosphor-icons/react/dist/csr/CaretRight";
import { ChatsCircle } from "@phosphor-icons/react/dist/csr/ChatsCircle";
import { Kanban } from "@phosphor-icons/react/dist/csr/Kanban";
import { seatInk, seatSlots } from "../seats";
import {
  READING_LABEL,
  STATUS_LABEL,
  VERDICT_GLYPH,
  VERDICT_LABEL,
  type Point,
  type Remark as RemarkRec,
  type Poll,
  type Speech,
  type Tally,
  type Verdict,
} from "../types";
import type { VoteChange } from "../useCouncil";
import type { CouncilState, LiveTurn, ReadPage } from "../useCouncil";
import { Md } from "./Markdown";

/** Ceiling on any rendered round count. The engine caps plans at 100. */
export const MAX_ROUND_TICKS = 100;

/**
 * A span of time, the way somebody reading it would say it. 95 -> "1m 35s", 47 -> "47s".
 *
 * Seconds only matter for the first minute; after that the hours are what
 * answers "how long has this been going", and `59m 59s` is not.
 *
 * One of these for the whole app. There were three — `ago()` here, `mmss()` in
 * the rail, and this — rendering the same duration three ways, so the rail's
 * per-seat time read `2m 00s` where the turn clock read `2m` and neither matched
 * the other.
 */
export function fmtElapsed(secs: number): string {
  if (secs < 60) return `${Math.max(0, Math.floor(secs))}s`;
  const m = Math.floor(secs / 60);
  const r = Math.floor(secs % 60);
  if (m < 60) return r === 0 ? `${m}m` : `${m}m ${r}s`;
  const h = Math.floor(m / 60);
  const rm = m % 60;
  return rm === 0 ? `${h}h` : `${h}h ${rm}m`;
}

/**
 * Tag an action-log line by the tool that produced it.
 *
 * A flat ordered match, most specific first. The nested-ternary version this
 * replaced put `includes("summary")` above the adjournment check, so an
 * adjournment whose reason mentioned the summary was filed as a summary, and
 * three real engine lines fell through to a generic "act".
 */
function splitAction(line: string, label: string): { tag: string; text: string } {
  const prefix = `${label} `;
  const bare = line.startsWith(prefix) ? line.slice(prefix.length) : line;
  const lower = bare.toLowerCase();

  const tag = ((): string => {
    if (lower.startsWith("called a deadlock review")) return "review";
    if (lower.startsWith("proposed ")) return "extension";
    if (lower.startsWith("closing address")) return "closing";
    if (lower.startsWith("ruled ")) return "ruling";
    if (lower.startsWith("would ")) return "extension";
    if (lower.startsWith("read ")) return "read";
    if (lower.startsWith("searched for")) return "search";
    if (lower.includes("ruled ") && lower.includes("re-voted")) return "re-vote";
    if (lower.startsWith("voted")) return "vote";
    if (lower.startsWith("raised p")) return "make_point";
    if (lower.startsWith("conceded")) return "concede";
    if (lower.startsWith("withdrew")) return "withdraw";
    if (lower.startsWith("opened poll")) return "poll";
    if (lower.startsWith("steelmanned")) return "steelman";
    if (lower.startsWith("rebutted")) return "rebut";
    if (lower.startsWith("cited")) return "cite";
    if (lower.startsWith("pressed")) return "press";
    if (lower.startsWith("moved to adjourn")) return "adjourn";
    if (lower.includes("adjournment") || lower.includes("adjourn")) return "adjourn";
    if (lower.startsWith("final result")) return "result";
    if (lower.includes("summary")) return "summary";
    if (lower.startsWith("(rejected")) return "rejected";
    if (lower.startsWith("(empty turn")) return "empty";
    if (lower.startsWith("(no actions")) return "idle";
    if (lower.startsWith("(turn failed")) return "failed";
    return "act";
  })();

  return { tag, text: bare.charAt(0).toUpperCase() + bare.slice(1) };
}

/**
 * What each kind of action is called on screen.
 *
 * The tag itself is the engine's own name for the thing — `make_point` — and it
 * is the wrong thing to show a reader: it is an identifier from the prompt, not
 * a word. `data-tag` keeps it for the colour; this is what it says.
 */
export const ACTION_LABEL: Record<string, string> = {
  make_point: "Point",
  vote: "Vote",
  "re-vote": "Re-vote",
  concede: "Concede",
  withdraw: "Withdraw",
  poll: "Poll",
  review: "Review",
  steelman: "Steelman",
  rebut: "Rebuttal",
  cite: "Cite",
  press: "Press",
  ruling: "Ruling",
  read: "Read",
  search: "Search",
  closing: "Closing",
  adjourn: "Adjourn",
  extension: "Extension",
  result: "Result",
  summary: "Summary",
  rejected: "Refused",
  empty: "Empty",
  idle: "Idle",
  failed: "Failed",
  act: "Action",
};

/**
 * An engine action tag as a word, pluralised for the count beside it.
 *
 * Falls back to a neutral word rather than the tag itself: the old `?? tag` meant
 * any action this app did not have a label for printed its engine name — and
 * `RecordPanel` does the same, so an unknown tag used to surface as raw text.
 */
function actionLabel(tag: string): string {
  return ACTION_LABEL[tag] ?? "Action";
}

/**
 * Same, but ready to sit after a number: "2 Points", "1 Rebuttal".
 *
 * Only the obvious plurals are pluralised. "Steelman" and "Poll" are already
 * fine either way, and the fallbacks are one-offs that read oddly inflected.
 */
const COUNTABLE = new Set(["Point", "Vote", "Rebuttal", "Cite", "Result", "Summary", "Action"]);

export function actionCountLabel(tag: string, count: number): string {
  const word = ACTION_LABEL[tag] ?? "Action";
  return count === 1 || !COUNTABLE.has(word) ? word : `${word}s`;
}

function ActionRow({
  tag,
  text,
  read,
}: {
  tag: string;
  text: string;
  /** The page this row names, when the seat read one. */
  read?: ReadPage;
}) {
  if (!read) {
    return (
      <li className="flex items-start gap-2 py-1 text-[12.5px] leading-relaxed">
        <span className="act-tag" data-tag={tag}>
          {actionLabel(tag)}
        </span>
        <span className="min-w-0 flex-1 text-ink-2 [overflow-wrap:anywhere]">{text}</span>
      </li>
    );
  }
  return (
    <li className="py-1 text-[12.5px] leading-relaxed">
      <details className="group">
        <summary className="flex cursor-pointer list-none items-start gap-2">
          <span className="act-tag" data-tag={tag}>
            {actionLabel(tag)}
          </span>
          <span className="min-w-0 flex-1 text-ink-2 [overflow-wrap:anywhere] group-open:text-ink">
            {text}
          </span>
        </summary>
        {/* The page itself, so the operator can check the seat's source rather
            than take its word for it. */}
        <div className="mt-1.5 ml-1 border-l-2 border-line-2 pl-2.5">
          {/* A search has results but no page behind them, so it shows the
              text and no link rather than an empty one. */}
          {read.url ? (
            <a
              href={read.url}
              target="_blank"
              rel="noreferrer noopener"
              className="text-[11.5px] text-accent underline underline-offset-2"
            >
              {read.url}
            </a>
          ) : (
            <span className="text-[11.5px] text-ink-3">search results</span>
          )}
          {read.truncated && (
            <p className="mt-0.5 text-[11px] text-ink-3">
              {/* Two different causes, one flag: the engine cut it at the turn's
                  reading budget, or this is a capped preview (see
                  READ_PREVIEW_CHARS). Either way the page above is the whole
                  thing, so say where to go rather than implying the text is
                  complete. */}
              {read.url
                ? "cut short — the full page is at the link above"
                : "cut at the turn's reading budget"}
            </p>
          )}
          <pre className="mt-1 max-h-72 overflow-auto whitespace-pre-wrap rounded bg-surface-2 p-2 text-[11.5px] leading-relaxed text-ink-2 [overflow-wrap:anywhere]">
            {read.text}
          </pre>
        </div>
      </details>
    </li>
  );
}

/**
 * Collapsible thinking, owned by React state. `defaultOpen` is only the
 * initial value: a viewer can close a streaming thought and it stays closed,
 * because no re-render ever forces it back open. (A native <details> lost
 * that fight against the `open` attribute on every token.)
 *
 * `closeOnContent` collapses the thought the moment speech starts, so a
 * streaming thought and a streaming reply never appear to run in parallel —
 * the model thinks, then speaks, and the UI says so.
 */
function Thought({
  text,
  defaultOpen,
  closeOnContent,
}: {
  text: string;
  defaultOpen?: boolean;
  /** Set when speech has started — collapses an open thought so thinking and reply never run in parallel. */
  closeOnContent?: boolean;
}) {
  const [open, setOpen] = useState(!!defaultOpen);
  const userToggled = useRef(false);
  useEffect(() => {
    if (closeOnContent && open && !userToggled.current) setOpen(false);
    if (!closeOnContent) userToggled.current = false;
  }, [closeOnContent, open]);
  if (!text) return null;
  const chars = text.length;
  return (
    <div className="thought">
      <button
        type="button"
        className="thought-toggle"
        onClick={() => {
          userToggled.current = true;
          setOpen((o) => !o);
        }}
        aria-expanded={open}
      >
        {open ? <CaretDown size={12} /> : <CaretRight size={12} />}
        <span className="tabular-nums">
          thought · {chars > 1000 ? `${(chars / 1000).toFixed(1)}k chars` : `${chars} chars`}
        </span>
      </button>
      {open && <ThoughtBody text={text} />}
    </div>
  );
}

/**
 * Text that streams, deferred before it is parsed.
 *
 * Markdown parsing is the expensive part of a token, and it is re-run over the
 * *whole* accumulated buffer every time a token arrives — quadratic across a
 * turn, and a turn can be 30k characters. Nothing needs to see every token: the
 * reader wants the newest text a few tens of milliseconds later, not a
 * re-parse per chunk.
 *
 * `useDeferredValue` is exactly that — the render runs at lower priority, so if
 * another token lands while React is still working, it restarts with the newer
 * string instead of finishing an intermediate one. Intermediates are skipped,
 * not queued, and the final value is always rendered. The caret and the turn
 * state sit outside this so they still move on every token: the text is what
 * can wait, the sign of life is what must not.
 */
function useStreamed(text: string): string {
  return useDeferredValue(text);
}

/**
 * The thought body is its own scroller past 16rem, and a nested scroller does
 * not follow new content on its own: the transcript container scrolls, the
 * inner box sits still, and fresh thinking lands below the fold. So the body
 * tracks its own pin and walks itself down while streaming — until the viewer
 * scrolls up inside it, which releases the pin like the main transcript does.
 *
 * The pin follows the *deferred* text, not the raw stream, so it scrolls to
 * where the reader can actually see rather than to a position that is about to
 * be replaced.
 */
function ThoughtBody({ text }: { text: string }) {
  const ref = useRef<HTMLDivElement | null>(null);
  const pin = useRef(true);
  const shown = useStreamed(text);
  useEffect(() => {
    const el = ref.current;
    if (el && pin.current) el.scrollTop = el.scrollHeight;
  }, [shown]);
  return (
    <div
      ref={ref}
      className="thought-body"
      onScroll={(e) => {
        const el = e.currentTarget;
        pin.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
      }}
    >
      <Md text={shown} compact />
    </div>
  );
}

const BLANK_TALLY: Tally = {
  wins: 0,
  losses: 0,
  stalemates: 0,
  abstentions: 0,
  score: 0,
  status: "open",
  contested: false,
  ratings: {},
};

const STATUS_EDGE: Record<string, string> = {
  carried: "border-l-ok",
  fell: "border-l-bad",
  stalemate: "border-l-warn",
  open: "border-l-line-2",
  withdrawn: "border-l-line-2 opacity-55",
};

const STATUS_TEXT: Record<string, string> = {
  carried: "text-ok",
  fell: "text-bad",
  stalemate: "text-warn",
  open: "text-ink-2",
  withdrawn: "text-ink-3",
};

const GLYPH_COLOR: Record<Verdict, string> = {
  win: "text-ok",
  lose: "text-bad",
  stalemate: "text-warn",
};

/** What the in-flight turn is actually doing, from heartbeats alone. */
/**
 * Three dots, each rising in turn.
 *
 * For the stretch between "the model stopped writing" and "the turn resolved",
 * which on a slow model with a tool call can be a minute: nothing on the
 * transcript moves, and without a sign of life that reads as a bug rather than
 * as waiting. Reduced to a static marker when motion is turned off.
 */
function BouncingDots() {
  return (
    <span className="bouncing-dots" aria-hidden="true">
      <i />
      <i />
      <i />
    </span>
  );
}

export function TurnState({ live, streaming = true }: { live: LiveTurn; streaming?: boolean }) {
  const thinking = live.tokens === 0 && live.reasoning.length > 0;
  // A seat that has said what it is doing is not stalled, however long it takes.
  //
  // The `streaming` guard is not a nicety. `stream replies` is a supported
  // setting — its own tooltip names Cloudflare tunnelling, which will not stream
  // — and with it off, `live.tokens` stays 0 for the whole turn by design. So
  // this read true for *every* healthy turn and labelled a working model
  // "stalled?" in amber. A warning that fires on success is worse than none: it
  // trains the operator to ignore the one place a genuinely stuck turn is
  // flagged.
  const quiet =
    streaming && live.elapsed >= 12 && live.tokens === 0 && !thinking && !live.activity;
  return (
    <span
      /* No `role="status"`. Its text is a clock and a chunk count, both of
         which change on every heartbeat, and a live region announces every
         change it observes — so this pill talked continuously for the length of
         every turn, over the top of the speech the listener was actually
         following. Nothing here needs pushing at anyone: the speech is already
         in the transcript, and the amber border and tooltip carry the stall
         case visually without interrupting. */
      className={`inline-flex shrink-0 items-center gap-1.5 rounded-pill border px-2 py-0.5 font-mono text-[10.5px] whitespace-nowrap tabular-nums ${
        quiet ? "border-warn/50 text-warn" : "border-line-2 text-ink-2"
      }`}
      title={
        live.activity
          ? live.activity
          : quiet
            ? "nothing has arrived yet — the model may be loading or the request may be queued"
            : thinking
              ? "the model is thinking out loud; speech has not started"
              : streaming
                ? "request in flight"
                : "replies arrive all at once, so nothing is expected until this turn finishes"
      }
    >
      {/* Streamed pieces, not a bill: the server only reports real token counts
          once a turn finishes, and those land in the rail totals. Always plural:
          the 1↔2 flip-flop redrew the header on the second chunk. */}
      {fmtElapsed(live.elapsed)} · {live.tokens} chunks
      {thinking && <span className="font-sans">thinking</span>}
      {quiet && <span className="font-sans">stalled?</span>}
      {live.activity && (
        <>
          <BouncingDots />
          <span className="font-sans text-ink-3">{live.activity}</span>
        </>
      )}
    </span>
  );
}

function ThinkingDots() {
  return (
    <span className="inline-flex gap-0.5" aria-hidden>
      {[0, 1, 2].map((i) => (
        <span
          key={i}
          className="size-1 animate-bounce rounded-full bg-primary"
          style={{ animationDelay: `${i * 0.18}s` }}
        />
      ))}
    </span>
  );
}

/**
 * `slot` is the seat's 1-based position, used only as a CSS fallback key: themes
 * with a ramp define `--seat-N`/`--seat-ink-N` and override the stored colour,
 * while the rest define neither and the seat's own colour applies. That keeps
 * every call site free of any theme knowledge.
 */
function Avatar({
  color,
  live,
  glyph,
  slot,
}: {
  color: string;
  live?: boolean;
  glyph: string;
  slot: number;
}) {
  return (
    <span
      className={`avatar ${live ? "live-dot" : ""}`}
      style={{
        background: `var(--seat-${slot}, ${color})`,
        color: seatInk(slot, color),
        boxShadow: live ? `0 0 10px var(--seat-${slot}, ${color})` : undefined,
      }}
      aria-hidden
    >
      {glyph}
    </span>
  );
}

/**
 * One finished speech. Memoized on its own props: the live turn streams at
 * ~25 events a second, and re-rendering (and re-parsing the markdown of) every
 * earlier speech on each one is what made the transcript feel stuck.
 */
const SpeechCard = memo(function SpeechCard({
  speech: s,
  label,
  color,
  glyph,
  slot,
  intermission,
  reads,
}: {
  speech: Speech;
  label: string;
  color: string;
  /** Seat mark, and its 1-based position for the monochrome ramp. */
  glyph: string;
  slot: number;
  intermission: boolean;
  /** Pages this turn read, matched to the rows that name them. */
  reads?: ReadPage[];
}) {
  const eyebrow = !intermission
    ? null
    : s.actions.some((a) => a === "final result")
      ? "the end result"
      : `after round ${s.round}`;
  return (
    <article
      className={`rise py-4 first:pt-1 ${intermission ? "border-l-2 border-dashed border-line-2 pl-3" : ""}`}
    >
      <header className="mb-1.5 flex flex-wrap items-center gap-x-2 gap-y-1">
        <Avatar color={color} glyph={glyph} slot={slot} />
        <strong className="text-[13.5px] font-semibold tracking-tight text-ink">
          {label}
        </strong>
        <span className="rounded-full bg-surface-2 px-2 py-px text-[10.5px] text-ink-2 tabular-nums">
          {eyebrow ?? `R${s.round}`}
        </span>
      </header>
      {s.text && <Md text={s.text} />}
      <Thought text={s.reasoning} />
      {s.actions.length > 0 && (
        <ul className="mt-1.5 border-t border-dashed border-line pt-1">
          {s.actions.map((a, i) => {
            const { tag, text } = splitAction(a, label);
            // A read row is expanded by the page it names. Matching on the title
            // rather than on position keeps the pairing right when a turn read
            // several pages, or when one of the reads failed and left no page.
            const read = reads?.find((r) => text.includes(r.title));
            return <ActionRow key={i} tag={tag} text={text} read={read} />;
          })}
        </ul>
      )}
    </article>
  );
});

/**
 * The settled rounds: everything already spoken, with the polls below.
 *
 * Split out of `Transcript` so a streamed token does not re-diff history. The
 * shell still renders per token — it has to, to hand the token to `LiveTurn` —
 * but every prop here holds its identity across tokens (the grouping memo bails
 * on unchanged `speeches`, the poll split bails on unchanged `polls`, the
 * callbacks are stable), so this whole subtree bails out and only the live
 * turn re-renders. `SpeechCard` memo was already bailing per card; this skips
 * the list itself: the per-speech `labelOf`/`colorOf` lookups, the section
 * elements, and the poll filters.
 */
const SettledRounds = memo(function SettledRounds({
  q,
  rounds,
  visible,
  byRound,
  collapsed,
  toggle,
  summarizerId,
  labelOf,
  colorOf,
  glyphOf,
  slotOf,
  reads,
}: {
  q: string;
  rounds: number[];
  visible: Set<number>;
  byRound: Map<number, Speech[]>;
  collapsed: Set<number>;
  toggle: (round: number) => void;
  summarizerId: string | null;
  labelOf: (id: string) => string;
  colorOf: (id: string) => string;
  glyphOf: (id: string) => string;
  slotOf: (id: string) => number;
  reads: Record<number, ReadPage[]> | undefined;
}) {
  return (
    <div className="divide-y divide-line">
      {rounds.map((round) => {
        const inRound = byRound.get(round) ?? [];
        // Without a query this is the collapse state; with one, every matching
        // round is forced open and not collapsible (see the caret below).
        const open = !q && !collapsed.has(round);
        // The last round is the one being argued right now, so it always gets
        // laid out. Everything older is handed to `content-visibility: auto`
        // below, which skips layout and paint until it is scrolled to — the
        // browser does the skipping, so a collapsed-then-expanded round is
        // still correct. A hundred-round plan holds over a thousand speeches,
        // and laying out every one to show the eight that matter is what made
        // long debates unusable.
        const near = round >= (rounds[rounds.length - 1] ?? 0);
        const skip = near
          ? undefined
          : ({ contentVisibility: "auto", containIntrinsicSize: "auto 480px" } as const);
        if (q && !visible.has(round)) return null;
        return (
          <section key={round} aria-label={`round ${round}`}>
            {rounds.length > 1 && (
              <h3 className="sticky top-0 z-1 -mx-1 flex items-center gap-2 bg-surface px-1 py-1.5 text-[10.5px] font-semibold tracking-[0.1em] text-ink-3 uppercase">
                <button
                  type="button"
                  className="inline-flex cursor-pointer items-center gap-1 rounded px-1 py-0.5 hover:text-ink"
                  onClick={() => toggle(round)}
                  /* Reports what is on screen, not what a click would do.
                    During a search every matching round is forced open — and
                    this said `false` while its speeches were right there, with
                    a closed caret beside it. Search mode is not "collapsed";
                    it is "not collapsible", so the expanded state is what
                    counts here. */
                  aria-expanded={open || !!q}
                  aria-controls={`round-${round}-speeches`}
                  title={
                    q
                      ? "showing matches — clear the search to fold rounds again"
                      : open
                        ? "collapse this round"
                        : "expand this round"
                  }
                >
                  {/* Also follows what's shown, for the same reason the caret
                      is a lie otherwise. */}
                  {open || q ? <CaretDown size={12} /> : <CaretRight size={12} />}
                  round {round}
                </button>
                <span className="tabular-nums">
                  {inRound.length} speech{inRound.length === 1 ? "" : "s"}
                </span>
                {!open && !q && (
                  <span className="font-normal normal-case">· collapsed</span>
                )}
                {!!q && (
                  <span className="font-normal normal-case">· showing matches</span>
                )}
              </h3>
            )}
            {(open || q) && (
              // The skip goes on this div, not the `<section>`: a sticky
              // heading inside a `content-visibility: auto` subtree stops
              // sticking, which would cost the round label that says where you
              // are in the debate.
              <div id={`round-${round}-speeches`} style={skip}>
                {inRound.map((sp) => (
                  <SpeechCard
                    key={sp.id}
                    speech={sp}
                    label={labelOf(sp.delegate)}
                    color={colorOf(sp.delegate)}
                    glyph={glyphOf(sp.delegate)}
                    slot={slotOf(sp.delegate)}
                    intermission={summarizerId !== null && sp.delegate === summarizerId}
                    reads={reads?.[sp.id]}
                  />
                ))}
              </div>
            )}
          </section>
        );
      })}
    </div>
  );
});

/**
 * The seat currently speaking, and nothing else.
 *
 * This is the one subtree that *should* re-render per token — the thought, the
 * streamed text, the turn state. Owning the deferred text here (rather than in
 * `Transcript`) keeps the markdown re-parse off the settled rounds: the shell
 * re-renders to pass the token down, `SettledRounds` bails, and only this
 * article re-parses. Memo is still load-bearing: when the token stream pauses
 * (heartbeats, polls, votes), the parent renders with the same `live` identity
 * and this skips.
 */
const LiveTurnArticle = memo(function LiveTurnArticle({
  live,
  streaming,
  summarizerId,
  glyphOf,
  slotOf,
}: {
  live: LiveTurn;
  streaming?: boolean;
  summarizerId: string | null;
  glyphOf: (id: string) => string;
  slotOf: (id: string) => number;
}) {
  // Streaming text is deferred before it is parsed: see `useStreamed`. Living
  // here, the re-parse touches only this article — the caret below it still
  // animates per token, so the turn reads as live.
  const shownText = useStreamed(live.text ?? "");
  return (
    <article
      className={`rise py-4 ${summarizerId !== null && live.delegate === summarizerId ? "border-l-2 border-dashed border-line-2 pl-3" : ""}`}
      aria-label={`${live.label} speaking`}
    >
      <header className="mb-1.5 flex flex-wrap items-center gap-x-2 gap-y-1">
        <Avatar color={live.color} glyph={glyphOf(live.delegate)} slot={slotOf(live.delegate)} live />
        <strong className="text-[13.5px] font-semibold tracking-tight text-ink">
          {live.label}
        </strong>
        {live.model && (
          <span
            className="max-w-56 truncate font-mono text-[10.5px] text-ink-3"
            title={live.model}
          >
            {live.model}
          </span>
        )}
        <span className="rounded-full bg-surface-2 px-2 py-px text-[10.5px] text-ink-2 tabular-nums">
          R{live.round}
        </span>
        <TurnState live={live} streaming={streaming} />
      </header>
      <Thought
        key={live.turn}
        text={live.reasoning}
        defaultOpen
        closeOnContent={live.text.length > 0}
      />
      {live.text ? (
        <>
          {/* Deferred: the speech is re-parsed as markdown on every token
              otherwise, over the whole buffer each time. The caret outside
              it still animates per token, so the turn reads as live. */}
          <Md text={shownText} />
          <span
            className="mt-1 inline-block h-3.5 w-0.5 animate-pulse bg-primary align-[-2px]"
            aria-hidden
          />
        </>
      ) : (
        !live.reasoning && !live.activity && (
          <p className="flex items-center gap-2 text-[12.5px] text-ink-3">
            <ThinkingDots />
            waiting on the model
          </p>
        )
      )}
    </article>
  );
});

/**
 * The polls under the transcript, open above closed.
 *
 * Memoized for the same reason as `SettledRounds`: a poll's tally is rewritten
 * per ballot, but a streamed token touches neither list, so both bail per
 * token while `PollCard` memo already bails per card.
 */
const PollsSection = memo(function PollsSection({
  openPolls,
  shutPolls,
  labelOf,
}: {
  openPolls: Poll[];
  shutPolls: Poll[];
  labelOf: (id: string) => string;
}) {
  return (
    <>
      {openPolls.length > 0 && (
        /* No `aria-live`. A poll's tally is rewritten on every ballot cast, so a
           ten-seat poll announced the entire card — question, options, running
           counts — ten times in the space of one round. The poll is a question
           the operator may need to answer; the panel that carries a live
           ballot announces its own arrival, which is the one event that needs
           saying out loud. */
        <section className="mt-1">
          <div className="flex flex-col gap-2">
            {openPolls.map((p) => (
              <PollCard key={p.id} poll={p} labelOf={labelOf} />
            ))}
          </div>
        </section>
      )}

      {shutPolls.length > 0 && (
        <section className="mt-2 border-t border-line pt-3">
          <h3 className="section-title mb-2">Polls</h3>
          <div className="flex flex-col gap-2">
            {shutPolls.map((p) => (
              <PollCard key={p.id} poll={p} labelOf={labelOf} />
            ))}
          </div>
        </section>
      )}
    </>
  );
});

export function Transcript({
  speeches,
  live,
  colorOf,
  labelOf,
  glyphOf,
  slotOf,
  polls,
  summarizerId,
  reads,
  query = "",
  initialCollapsed,
  streaming,
}: {
  speeches: Speech[];
  live: LiveTurn | null;
  colorOf: (id: string) => string;
  labelOf: (id: string) => string;
  /** Seat mark, and its 1-based position in the monochrome ramp. */
  glyphOf: (id: string) => string;
  slotOf: (id: string) => number;
  polls: Poll[];
  /** The seat that lays rounds out: its speeches render as intermissions. */
  summarizerId: string | null;
  /** Pages read, by turn. */
  reads: Record<number, ReadPage[]>;
  /** Case-insensitive search across speech text and action lines. */
  query?: string;
  /** Rounds to fold away on first paint, e.g. everything but the last. */
  initialCollapsed?: number[];
  /**
   * Whether the seat currently speaking has streaming switched on.
   *
   * Only used to decide whether "nothing yet" is worth flagging: with replies
   * arriving in one piece at the end of the turn, silence is the expected state
   * and calling it a stall is a false alarm. See `TurnState`.
   */
  streaming?: boolean;
}) {
  // Collapse state lives here so the live chamber and the reader get it for
  // free, and so it survives the transcript re-rendering on every token.
  // Reset when the debate turns over: a new run clears its speeches, and
  // without this the new debate inherited the old one's folded rounds.
  const [collapsed, setCollapsed] = useState<Set<number>>(
    () => new Set(initialCollapsed ?? []),
  );
  const hadSpeeches = useRef(speeches.length > 0);
  useEffect(() => {
    if (speeches.length === 0) {
      if (hadSpeeches.current) {
        hadSpeeches.current = false;
        setCollapsed(new Set(initialCollapsed ?? []));
      }
    } else {
      hadSpeeches.current = true;
    }
  }, [speeches, initialCollapsed]);
  // Stable across renders: the update is functional, so no render's state is
  // captured. `SettledRounds` memo depends on this identity holding per token.
  const toggle = useCallback(
    (r: number) =>
      setCollapsed((prev) => {
        const next = new Set(prev);
        if (!next.delete(r)) next.add(r);
        return next;
      }),
    [],
  );
  // Memoized: the transcript re-renders per streamed token, and the naive
  // O(rounds × speeches) scan on every token stalled long debates.
  // Must sit above the empty-state early return: a hook after it changes the
  // hook count the moment the first speech lands, and React blanks the tree
  // ("rendered more hooks than during the previous render").
  const { q, rounds, visible, byRound } = useMemo(() => {
    const qq = query.trim().toLowerCase();
    // Grouped once, here. The memo this comment credits used to cover only the round
    // list, so the render body still ran `speeches.filter(sp => sp.round === round)`
    // per round *and* a `speeches.some(...)` per round while a query was active —
    // on every streamed token, since `speeches` is a new array identity as each
    // speech lands. The memo bought nothing.
    const grouped = new Map<number, Speech[]>();
    for (const sp of speeches) {
      const bucket = grouped.get(sp.round);
      if (bucket) bucket.push(sp);
      else grouped.set(sp.round, [sp]);
    }
    const rr = [...grouped.keys()].sort((a, b) => a - b);
    const vv = new Set(
      qq
        ? rr.filter((r) =>
            (grouped.get(r) ?? []).some(
              (sp) =>
                sp.text.toLowerCase().includes(qq) ||
                sp.actions.some((a) => a.toLowerCase().includes(qq)),
            ),
          )
        : rr,
    );
    return { q: qq, rounds: rr, visible: vv, byRound: grouped };
  }, [query, speeches]);
  // The poll split in one pass, memoized: a token touches neither list, so
  // both hold their identity per token and `PollsSection` bails.
  const [openPolls, shutPolls] = useMemo(() => {
    const open: Poll[] = [];
    const shut: Poll[] = [];
    for (const p of polls) (p.closed ? shut : open).push(p);
    return [open, shut] as const;
  }, [polls]);
  if (!speeches.length && !live) {
    return (
      <div className="mx-auto mt-10 max-w-[52ch] text-center">
        <div className="mx-auto mb-3 flex size-10 items-center justify-center rounded-xl bg-surface-2 text-ink-3">
          <ChatsCircle size={20} />
        </div>
        {/* "a motion" and "Convene" were both undefined on first run. Now the
            empty state says what to type, what the button does, and what the
            board is for. */}
        <p className="text-[14.5px] font-semibold tracking-tight text-ink">
          Set the question in the setup panel, then press Convene.
        </p>
        <p className="mx-auto mt-1.5 max-w-[48ch] text-[12.5px] leading-relaxed text-ink-2">
          Each model argues its case, then acts on it: putting points to the others
          to vote on, and conceding the ones it cannot support. Every delegate votes
          on every point, and the count decides what stands — not the persuasive
          quality of the prose.
        </p>
      </div>
    );
  }

  // Rounds, with the speeches in each. A six-round debate is twenty-four
  // speeches in one undifferentiated column, separable only by a small "R3"
  // badge; nothing said where round two ended and three began.
  // Centered with room to breathe: uncapped the column ran to the panel
  // edge on wide screens, and left-aligned at 80ch it stranded a void on the
  // right. 88ch keeps long speeches readable while filling the middle panel.
  //
  // The shell re-renders per streamed token — it hands the token to
  // `LiveTurnArticle` — but the settled subtrees below bail: their props hold
  // identity across tokens, so only the live article re-parses.
  return (
    <div className="mx-auto w-full max-w-[88ch]">
      {q && (
        <p className="mb-2 text-[11.5px] text-ink-3" role="status">
          {visible.size === 0
            ? `nothing matches "${query.trim()}"`
            : `matching in round${visible.size === 1 ? "" : "s"} ${[...visible]
                .sort((a, b) => a - b)
                .join(", ")}`}
        </p>
      )}
      <SettledRounds
        q={q}
        rounds={rounds}
        visible={visible}
        byRound={byRound}
        collapsed={collapsed}
        toggle={toggle}
        summarizerId={summarizerId}
        labelOf={labelOf}
        colorOf={colorOf}
        glyphOf={glyphOf}
        slotOf={slotOf}
        reads={reads}
      />
      {live && (
        <LiveTurnArticle
          live={live}
          streaming={streaming}
          summarizerId={summarizerId}
          glyphOf={glyphOf}
          slotOf={slotOf}
        />
      )}
      <PollsSection openPolls={openPolls} shutPolls={shutPolls} labelOf={labelOf} />
    </div>
  );
}

/**
 * The run trace: LangGraph's "one timeline step per node", flattened for a
 * round-robin council. Every round is a group, every seat a tick — filled in
 * seat color when that seat has spoken, pulsing while it speaks, hollow
 * afterwards. The whole debate's state of play in one glanceable strip.
 */
export function TranscriptSearch({
  value,
  onChange,
}: {
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <div className="flex min-w-0 flex-1 items-center gap-2">
      <input
        className="field"
        type="search"
        value={value}
        placeholder="Search the transcript"
        onChange={(e) => onChange(e.target.value)}
        aria-label="search the transcript"
      />
      {value && (
        <button
          type="button"
          className="btn btn-ghost shrink-0 !px-2.5 text-[12px]"
          onClick={() => onChange("")}
          title="clear search"
        >
          clear
        </button>
      )}
    </div>
  );
}

/**
 * The narrow projection `RunTrace` reads, and the thing that makes its memo real.
 *
 * `memo` shallow-compares props. Passing the whole `state` — as this component
 * used to — cannot work, because the reducer returns a fresh object on every
 * event including every streamed token, so the identity always differs and the
 * grid's full `rounds × delegates` markup (up to 1100 nodes) is rebuilt 25×/second
 * for a picture that did not change. Passing `state` and hoping is not a memo, it
 * is a comment about one.
 *
 * Every field here is a primitive or an array rebuilt only when its own source
 * changes, so a token — which touches none of them — leaves the object's identity
 * alone. `CouncilRail` already projects exactly this way.
 */
export interface RunTraceView {
  maxRounds: number;
  round: number;
  running: boolean;
  ended: boolean;
  delegates: CouncilState["delegates"];
  /** `${round}:${delegateId}` per spoken speech. */
  spoke: string[];
  /** `${round}:${delegateId}` per skipped seat. */
  skipped: string[];
  /** The seat mid-turn, if any: `${delegateId}@${round}`. */
  liveKey: string | null;
}

/** Build the projection. Pure, and identity-stable when nothing relevant moved. */
export function runTraceView(state: CouncilState): RunTraceView {
  return {
    maxRounds: state.maxRounds,
    round: state.round,
    running: state.running,
    ended: state.verdict != null,
    delegates: state.delegates,
    spoke: state.speeches.map((sp) => `${sp.round}:${sp.delegate}`),
    skipped: state.skipped.flatMap((s) => s.ids.map((id) => `${s.round}:${id}`)),
    liveKey: state.live ? `${state.live.delegate}@${state.live.round}` : null,
  };
}

/**
 * The same projection, held at a stable identity across tokens.
 *
 * Recomputed only when something the trace draws has actually changed. Without
 * the dependency array the `useMemo` would rebuild all three arrays on every
 * render — a token — producing exactly the fresh props the memo is there to
 * avoid, so the projection would have to be rebuilt to defeat itself.
 */
export function useRunTraceView(state: CouncilState): RunTraceView {
  const { maxRounds, round, running, verdict, delegates, speeches, skipped, live } = state;
  return useMemo(
    () => runTraceView(state),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [maxRounds, round, running, verdict, delegates, speeches, skipped, live],
  );
}

export const RunTrace = memo(function RunTrace({ view }: { view: RunTraceView }) {
  // Clamped: `total` is multiplied by the seat count into one DOM node per
  // round-seat, so an unvalidated round count from storage was a way to hang
  // the tab before the first model answered.
  const total = Math.min(Math.max(view.maxRounds, view.round, 1), MAX_ROUND_TICKS);
  // Seat colours, resolved through the monochrome ramp fallback. See the note
  // in `index.css`: the coloured themes define none of `--seat-N`, so the tick
  // keeps each seat's own colour there and is overridden in the grey ones.
  const slots = useMemo(() => seatSlots(view.delegates), [view.delegates]);
  const spoke = useMemo(() => new Set(view.spoke), [view.spoke]);
  const skipped = useMemo(() => new Set(view.skipped), [view.skipped]);
  const rounds = Array.from({ length: total }, (_, i) => i + 1);
  return (
    <div
      className="flex items-center gap-4 overflow-x-auto"
      /* `role="status"` removed. The grid is up to 1100 nodes whose titles change
         as seats speak; as a live region it re-announced the whole progress
         grid on every heartbeat, which is the single noisiest thing this app
         could do to a screen reader. `aria-label` on a plain div is also not
         exposed, so it was carrying nothing — the role is what made it a
         target, and the target is what needed removing. */
      aria-label="debate progress by round and delegate"
    >
      {rounds.map((r) => {
        const isPast = r < view.round || (!view.running && view.ended);
        return (
          <div key={r} className="flex shrink-0 items-center gap-1.5">
            <span
              className={`font-mono text-[10px] tabular-nums ${
                r === view.round && view.running ? "font-bold text-ink" : "text-ink-3"
              }`}
            >
              R{r}
            </span>
            {view.delegates.map((d) => {
              const said = spoke.has(`${r}:${d.id}`);
              const isLive = view.liveKey === `${d.id}@${r}`;
              const wasSkipped = !said && !isLive && skipped.has(`${r}:${d.id}`);
              return (
                <span
                  key={d.id}
                  title={`${d.label} · round ${r} · ${
                    said ? "spoken" : isLive ? "speaking" : wasSkipped ? "skipped" : isPast ? "silent" : "upcoming"
                  }`}
                  className={`size-2 rounded-full ${
                    said ? "" : wasSkipped ? "border border-dashed border-warn" : "border border-line-2"
                  } ${isLive ? "live-dot" : ""}`}
                  style={
                    said || isLive
                      ? {
                          background: `var(--seat-${slots[d.id] ?? 1}, ${d.color})`,
                          borderColor: "transparent",
                        }
                      : undefined
                  }
                />
              );
            })}
          </div>
        );
      })}
    </div>
  );
});

/**
 * One poll's question, its options and the tally filling in.
 *
 * Memoized for the same reason as `RunTrace`: a poll stays on the transcript
 * after it closes, and every token re-rendered every open poll and every closed
 * one, re-counting the ballots each time.
 */
const PollCard = memo(function PollCard({
  poll,
  labelOf,
}: {
  poll: Poll;
  labelOf: (id: string) => string;
}) {
  if (poll.options.length === 0) return null;
  const confOf = (delegate: string): string => {
    const b = poll.ballots.find((x) => x.delegate === delegate);
    return b?.confidence != null ? ` · ${Math.round(b.confidence * 100)}%` : "";
  };
  const counts = poll.options.map((o) => ({
    option: o,
    n: poll.ballots.filter((b) => b.choice.toLowerCase() === o.toLowerCase()).length,
    voters: poll.ballots.filter((b) => b.choice.toLowerCase() === o.toLowerCase()),
  }));
  const cast = counts.reduce((a, c) => a + c.n, 0);
  const top = counts.reduce((best, c) => (c.n > (best?.n ?? -1) ? c : best), counts[0]);
  const outcome = cast > 0 && top && top.n * 2 > cast ? top.option : null;

  return (
    <div className={`card rise p-3 ${poll.closed ? "" : "!border-primary/60"}`}>
      <strong className="text-[13px] font-medium tracking-tight text-ink">
        {poll.question}
      </strong>
      <div className="mt-0.5 text-[11px] text-ink-3">
        opened by {labelOf(poll.openedBy)} · {cast} vote{cast === 1 ? "" : "s"} cast
        {!poll.closed && <span> · still open</span>}
      </div>
      <ul className="mt-2 flex flex-col gap-1.5">
        {counts.map(({ option, n, voters }, i) => (
          <li
            key={`${i}-${option.toLowerCase()}`}
            className={`bar ${outcome === option ? "leader" : ""}`}
            title={
              voters.map((v) => `${labelOf(v.delegate)}${confOf(v.delegate)}`).join(", ") ||
              "no votes"
            }
          >
            <i style={{ width: cast > 0 ? `${(n / cast) * 100}%` : "0%" }} />
            <span className="absolute inset-0 flex items-center gap-2 px-2.5 text-[12px]">
              <span className="truncate text-ink">{option}</span>
              <span className="ml-auto shrink-0 font-mono text-[11px] text-ink-2 tabular-nums">
                {n}
              </span>
            </span>
          </li>
        ))}
      </ul>
      <div className="mt-1.5 text-[11.5px]">
        {!poll.closed ? (
          <span className="text-ink-3">waiting for the other delegates to vote…</span>
        ) : outcome ? (
          <span className="font-medium text-ok">passed: {outcome}</span>
        ) : (
          <span className="text-warn">no decision — not enough votes either way</span>
        )}
      </div>
    </div>
  );
});

/**
 * The board: every point, its status, and how each seat voted.
 *
 * Memoized, and the largest single win in the per-token path. It re-counted
 * `points` five times over and mapped ten voter pills per point on every
 * streamed token, so a board of a few hundred points rebuilt thousands of
 * elements roughly 25 times a second to draw the same thing.
 */
export const BoardPanel = memo(function BoardPanel({
  points,
  delegates,
  tallyOf,
  labelOf,
  slotOf,
  voteLog,
  initialCollapsed,
}: {
  points: Point[];
  delegates: { id: string; label: string; color: string; role?: string }[];
  tallyOf: (id: number) => Tally | undefined;
  labelOf: (id: string) => string;
  /** Seat position in the monochrome ramp; see the note in `index.css`. */
  slotOf: (id: string) => number;
  /** Changes of mind per point, when they are known. */
  voteLog?: Record<number, VoteChange[]>;
  /**
   * Rounds to fold on first paint, so a long board does not open 200px tall per
   * point. Set by the reader, which knows the debate is over.
   */
  initialCollapsed?: number[];
}) {
  // The Summarizer casts no votes, ever — it abstains by construction, so a
  // pill for it reads as a voter that hasn't voted yet rather than as the
  // non-voter it is. Seats predating roles (no role) still show.
  const voters = delegates.filter((d) => d.role !== "summarizer");
  if (!points.length) {
    return (
      <div className="px-4 py-10 text-center">
        <div className="mx-auto mb-2 flex size-9 items-center justify-center rounded-xl bg-surface-2 text-ink-3">
          <Kanban size={18} />
        </div>
        <p className="text-[12.5px] font-medium text-ink-2">No points yet.</p>
        <p className="mx-auto mt-1 max-w-[30ch] text-[11.5px] leading-relaxed text-ink-3">
          A point is one thing a delegate claims, for or against the motion. They
          appear here as soon as one is raised, then every delegate votes on them.
        </p>
      </div>
    );
  }

  const tally = (p: Point): Tally => tallyOf(p.id) ?? BLANK_TALLY;
  const carried = points.filter((p) => tally(p).status === "carried").length;
  const fell = points.filter((p) => tally(p).status === "fell").length;
  const stuck = points.filter((p) => tally(p).status === "stalemate").length;
  // Counted by status, not by subtraction: a withdrawn point is not live
  // ground, and folding it into "open" reported a board of three retracted
  // claims as three undecided ones.
  const open = points.filter((p) => tally(p).status === "open").length;
  const pulled = points.filter((p) => tally(p).status === "withdrawn").length;

  /* Which points start folded.
   *
   * Every multi-remark block inside a point rendered `<details open>`, so a
   * 40-point board opened every rebuttal in full — several hundred pixels per
   * point, with the transcript's own careful collapsing not applying here at
   * all. The status line and the tally stay visible either way; only the
   * reasoning inside the card folds, so nothing that decides the board is ever
   * hidden by default.
   *
   * Settled points fold (their reasoning is rarely reopened); contested and open
   * ones do not, because those are the ones being argued about right now. In the
   * reader everything folds, since the debate is over and this is reference
   * material.
   */
  const folded = useMemo(() => {
    const seeded = new Set(initialCollapsed ?? []);
    const settled = new Set<number>();
    for (const p of points) {
      if (seeded.has(p.id)) settled.add(p.id);
      const status = tally(p).status;
      if (status === "carried" || status === "fell" || status === "withdrawn") {
        settled.add(p.id);
      }
    }
    return settled;
  }, [points, tallyOf, initialCollapsed]);

  return (
    <div className="flex flex-col gap-2 p-2.5">
      {folded.size > 0 && (
        /* Says so, because a board that looks sparse after a refresh gives no
           hint that the text is one click away — and the folded points are the
           *decided* ones, which is where an operator looks first. */
        <p className="px-1 text-[11px] leading-relaxed text-ink-3">
          {folded.size} settled point{folded.size === 1 ? "" : "s"} folded — open one to read
          the argument.
        </p>
      )}
      <div className="px-1 pt-1">
        {/* "carried / fell / pulled" are participles used as nouns, and nothing
            anywhere explained them. Each now carries a tooltip, since this
            strip is the only place the three states are counted. */}
        <div className="flex gap-3 text-[10.5px] tracking-[0.08em] uppercase">
          <span className="text-ok tabular-nums" title="Enough delegates supported it for it to stand">
            {carried} carried
          </span>
          <span className="text-bad tabular-nums" title="More delegates opposed it than supported it, so it did not stand">
            {fell} fell
          </span>
          <span className="text-warn tabular-nums" title="Split vote — neither side had enough">
            {stuck} tied
          </span>
          {pulled > 0 && (
            <span className="text-ink-3 tabular-nums" title="The delegate who raised it withdrew it, so it is out of the debate">
              {pulled} pulled
            </span>
          )}
          <span className="ml-auto text-ink-3 tabular-nums">{points.length} total</span>
        </div>
        <div
          className="spread mt-1.5"
          role="img"
          aria-label={`${carried} carried, ${fell} fell, ${stuck} tied, ${open} undecided, ${pulled} withdrawn`}
        >
          {carried > 0 && (
            <i style={{ width: `${(carried / points.length) * 100}%`, background: "var(--ok)" }} />
          )}
          {fell > 0 && (
            <i style={{ width: `${(fell / points.length) * 100}%`, background: "var(--bad)" }} />
          )}
          {stuck > 0 && (
            <i style={{ width: `${(stuck / points.length) * 100}%`, background: "var(--warn)" }} />
          )}
          {open > 0 && (
            <i style={{ width: `${(open / points.length) * 100}%`, background: "var(--line-2)" }} />
          )}
        </div>
      </div>

      {points.map((p) => {
        const t = tally(p);
        return (
          <article key={p.id} className={`card rise border-l-2 p-3 ${STATUS_EDGE[t.status]}`}>
            <header className="mb-1 flex items-center gap-2">
              <span className="font-mono text-[11px] text-ink-3">P{p.id}</span>
              <span className={`text-[10px] font-semibold tracking-[0.08em] uppercase ${STATUS_TEXT[t.status]}`}>
                {STATUS_LABEL[t.status]}
              </span>
              {/* The split, and then what it comes to.
                  A point one seat has hit and two have backed reads as "+1" —
                  carried — which is arithmetically right and looks like no
                  opposition at all, so the split has to be on the card.

                  Written as `1▲ 1▼ 1◆ = 0` rather than `1▲ 1▼ 1◆ 0`: the last
                  number is the sum of the ones before it, and set that way it
                  reads as a fourth count. The `=` says so, and the split is
                  only shown when there is something in it — a point three seats
                  back needs no arithmetic drawn out. */}
              <span
                className="ml-auto flex items-baseline gap-1.5 whitespace-nowrap"
                title={`${t.wins} support, ${t.losses} oppose, ${t.stalemates} neither — ${
                  t.abstentions
                } of ${t.wins + t.losses + t.stalemates + t.abstentions} delegates have not voted yet`}
              >
                {(t.losses > 0 || t.stalemates > 0) && (
                  <span className="flex font-mono text-[10.5px] tabular-nums sm:gap-1">
                    {t.wins > 0 && <span className="text-ok">{t.wins}▲</span>}
                    {t.losses > 0 && <span className="text-bad">{t.losses}▼</span>}
                    {t.stalemates > 0 && <span className="text-warn">{t.stalemates}◆</span>}
                    <span className="text-ink-3">=</span>
                  </span>
                )}
                <span
                  className={`font-mono text-[13px] font-bold tabular-nums ${
                    t.score > 0 ? "text-ok" : t.score < 0 ? "text-bad" : "text-ink-3"
                  }`}
                >
                  {t.score > 0 ? `+${t.score}` : t.score}
                </span>
              </span>
            </header>

            <div className="[&_div.md]:text-[13px]">
              <Md text={p.text} />
            </div>

            {p.ruling && (
              <div
                className={`mb-1.5 border-l-2 pl-2 ${
                  p.ruling.carries ? "border-ok" : "border-bad"
                }`}
              >
                <div
                  className="text-[9px] font-semibold tracking-[0.08em] text-ink-3 uppercase"
                  title="A ruling can overrule the votes. This is the one place in the app where that happens, so it explains itself."
                >
                  ruled by {labelOf(p.ruling.by)} · R{p.ruling.round}
                </div>
                <p className="text-[12px] leading-relaxed text-ink-2">
                  This point{" "}
                  <strong className={p.ruling.carries ? "text-ok" : "text-bad"}>
                    {p.ruling.carries ? "stands" : "does not stand"}
                  </strong>{" "}
                  regardless of how the delegates voted — {p.ruling.reason}
                </p>
              </div>
            )}

            {p.beats.length > 0 && (
              <div className="mt-1 mb-1 text-[11px] text-ink-3">
                <span
                  className="text-ink-2"
                  title="These points are ruled out by this one. Only one of a set of points that contradict each other can stand."
                >
                  rules out
                </span>{" "}
                {p.beats.map((b) => (
                  <span key={b} className="mr-1 font-mono">
                    P{b}
                  </span>
                ))}
              </div>
            )}

            <div className="mt-1 mb-2 text-[11px] text-ink-3">
              raised by {labelOf(p.raisedBy)}
              {p.withdrawnReason ? ` — withdrawn: ${p.withdrawnReason}` : ""}
              {p.review?.outcome && (
                <span className="text-ink-2"> — ruled {READING_LABEL[p.review.outcome]}</span>
              )}
              {/* Every seat's ruling, not just the outcome: these were dropped
                  on the floor, so a review looked like nothing happening for
                  the length of a round trip per seat. */}
              {p.review && p.review.ballots.length > 0 && (
                <ul className="mt-1.5 flex flex-wrap gap-1">
                  {p.review.ballots.map((b) => (
                    <li
                      key={b.delegate}
                      className="status-pill border border-line-2 text-ink-3"
                      title={b.reason ?? undefined}
                    >
                      {labelOf(b.delegate)}: {READING_LABEL[b.reading]}
                    </li>
                  ))}
                </ul>
              )}
              {p.review && !p.review.closed && (
                <span className="text-warn"> — under review</span>
              )}
            </div>

            <div className="flex flex-wrap gap-1">
              {voters.map((d) => {
                const shown = t.ratings[d.id] ?? null;
                const implicit = !p.votes[d.id] && shown !== null;
                // `shown` is the raw Verdict enum, so this used to read "Arbiter: lose" in the
                // tooltip. VERDICT_LABEL says what the glyph means. An implicit
                // vote — one a delegate has not cast but is counted on, because
                // it raised the point — is the kind of thing that needs spelling
                // out rather than jargon.
                const label = p.votes[d.id]?.reason
                  ? `${d.label} ${shown ? VERDICT_LABEL[shown] : "?"} this point — ${p.votes[d.id]!.reason}`
                  : implicit
                    ? `${d.label} ${shown ? VERDICT_LABEL[shown] : "?"} this point, without being asked — it raised the point`
                    : `${d.label} has not voted on this point yet`;
                return (
                  <span
                    key={d.id}
                    role="img"
                    aria-label={label}
                    className={`inline-flex items-center gap-1.5 rounded-pill border px-2 py-px text-[10px] ${
                      shown ? "text-ink-2" : "border-dashed text-ink-3"
                    }`}
                    style={{
                      borderColor: shown ? `var(--seat-${slotOf(d.id)}, ${d.color})` : undefined,
                    }}
                    title={label}
                  >
                    <span
                      className="size-1.5 rounded-full"
                      style={{ background: `var(--seat-${slotOf(d.id)}, ${d.color})` }}
                    />
                    {d.label}
                    <span className={`font-mono ${shown ? GLYPH_COLOR[shown] : ""}`}>
                      {shown ? VERDICT_GLYPH[shown] : "·"}
                    </span>
                  </span>
                );
              })}
            </div>

            {voteLog?.[p.id] && voteLog[p.id].length > 0 && (
              <div className="mt-1.5">
                <details className="group">
                  <summary
                    className="cursor-pointer list-none text-[11px] text-ink-3 hover:text-ink-2"
                    title="Delegates that changed their vote on this point"
                  >
                    {voteLog[p.id].length} change{voteLog[p.id].length === 1 ? "" : "s"} of mind
                    <CaretRight size={10} weight="bold" className="ml-1 inline group-open:hidden" />
                  </summary>
                  <ul className="mt-1 flex flex-col gap-0.5 border-l-2 border-line-2 pl-2">
                    {voteLog[p.id].map((c, i) => (
                      <li key={i} className="text-[11px] leading-relaxed text-ink-3">
                        <span className="font-mono">R{c.round}</span>{" "}
                        <span className="text-ink-2">{labelOf(c.delegate)}</span>{" "}
                        {/* Glyphs, not the words — these rows previously rendered
                            the enum itself, so the log read "win → lose". */}
                        {c.from ? (
                          <>
                            <span className={GLYPH_COLOR[c.from]}>{VERDICT_GLYPH[c.from]}</span> →{" "}
                          </>
                        ) : (
                          <>→ </>
                        )}
                        <span className={GLYPH_COLOR[c.to]}>{VERDICT_GLYPH[c.to]}</span>
                        {c.reason && <span className="ml-1">— {c.reason}</span>}
                      </li>
                    ))}
                  </ul>
                </details>
              </div>
            )}

            {!t.contested && t.status !== "withdrawn" && (
              <div className="mt-1.5 text-[11px] text-ink-3">
                no other delegate has voted on this yet
              </div>
            )}

            {p.rebuttals.length > 0 && (
              <Remarks label="Rebuttal" remarks={p.rebuttals} open={!folded.has(p.id)} />
            )}
            {p.steelmen.length > 0 && (
              // "Steelman" is a term of art, so it gets a gloss: stating the
              // other side's case as well as its own advocate would.
              <Remarks
                label="Steelman"
                title="The delegate states the other side's case as well as its own advocate would, then answers it."
                remarks={p.steelmen}
                open={!folded.has(p.id)}
              />
            )}
            {p.evidence.length > 0 && (
              <Remarks label="Evidence" remarks={p.evidence} open={!folded.has(p.id)} />
            )}
          </article>
        );
      })}

      <p className="px-1 pt-0.5 text-[11px] leading-relaxed text-ink-3">
        Every delegate votes on every point. Whoever raised a point is counted as
        supporting it until they vote against it, concede it, or withdraw it — so
        you never have to wait on a vote that is already assumed.
      </p>
    </div>
  );
});

/**
 * Every remark on a point, not only the last.
 *
 * Showing only the final beat meant an exchange that took three rounds to
 * resolve — steelman, rebut, re-rebut — read as a single remark, and the work
 * that changed the point's mind vanished. The last one stays open because it is
 * the one that counts now.
 */
function Remarks({
  label,
  title,
  remarks,
  open = true,
}: {
  label: string;
  title?: string;
  remarks: RemarkRec[];
  /** False folds the exchange; the count and label stay visible. */
  open?: boolean;
}) {
  if (remarks.length === 1)
    return (
      <Remark label={label} title={title}>
        {remarks[0]!.text}
      </Remark>
    );
  return (
    <div className="mt-1.5">
      <details open={open}>
        <summary
          className="cursor-pointer list-none text-[9px] font-semibold tracking-[0.08em] text-ink-3 uppercase hover:text-ink-2"
          title={title}
        >
          {remarks.length}× {label}
          <span className="ml-1 font-normal tracking-normal normal-case">
            (latest first)
          </span>
        </summary>
        <div className="flex flex-col gap-1.5">
          {[...remarks].reverse().map((r, i) => (
            <Remark key={i} label={label} title={title}>
              {r.text}
            </Remark>
          ))}
        </div>
      </details>
    </div>
  );
}

/**
 * One remark, under a plain-English heading.
 *
 * The heading used to be the engine's tag — "rebut", "steelman", "evidence" —
 * so a reader met an identifier before meeting a word. `label` is the display
 * name and `title` the one-line gloss; the tag has no remaining use.
 */
function Remark({
  label,
  title,
  children,
}: {
  label: string;
  title?: string;
  children: string;
}) {
  return (
    <div className="mt-1.5 border-l-2 border-line-2 pl-2">
      <div
        className="text-[9px] font-semibold tracking-[0.08em] text-ink-3 uppercase"
        title={title}
      >
        {label}
      </div>
      <Md text={children} compact />
    </div>
  );
}
