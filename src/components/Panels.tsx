import { memo, useEffect, useMemo, useRef, useState } from "react";
import { CaretDown, CaretRight, ChatsCircle, Kanban } from "@phosphor-icons/react";
import {
  READING_LABEL,
  STATUS_LABEL,
  VERDICT_GLYPH,
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

/** 95 -> "1m 35s", 47 -> "47s" */
/**
 * A span of time, the way somebody reading it would say it.
 *
 * Seconds only matter for the first minute; after that the hours are what
 * answers "how long has this been going", and `59m 59s` is not.
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
const ACTION_LABEL: Record<string, string> = {
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

function actionLabel(tag: string): string {
  return ACTION_LABEL[tag] ?? tag;
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
            <p className="mt-0.5 text-[11px] text-ink-3">cut at the turn&apos;s reading budget</p>
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
 * The thought body is its own scroller past 16rem, and a nested scroller does
 * not follow new content on its own: the transcript container scrolls, the
 * inner box sits still, and fresh thinking lands below the fold. So the body
 * tracks its own pin and walks itself down while streaming — until the viewer
 * scrolls up inside it, which releases the pin like the main transcript does.
 */
function ThoughtBody({ text }: { text: string }) {
  const ref = useRef<HTMLDivElement | null>(null);
  const pin = useRef(true);
  useEffect(() => {
    const el = ref.current;
    if (el && pin.current) el.scrollTop = el.scrollHeight;
  }, [text]);
  return (
    <div
      ref={ref}
      className="thought-body"
      onScroll={(e) => {
        const el = e.currentTarget;
        pin.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
      }}
    >
      <Md text={text} compact />
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

export function TurnState({ live }: { live: LiveTurn }) {
  const thinking = live.tokens === 0 && live.reasoning.length > 0;
  // A seat that has said what it is doing is not stalled, however long it takes.
  const quiet = live.elapsed >= 12 && live.tokens === 0 && !thinking && !live.activity;
  return (
    <span
      role="status"
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
              : "request in flight"
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

/** Dark ink on pale seats, pale ink on deep ones. */
function inkOn(hex: string): string {
  const m = hex.trim().replace("#", "");
  if (m.length < 6) return "#06110f";
  const r = parseInt(m.slice(0, 2), 16) / 255;
  const g = parseInt(m.slice(2, 4), 16) / 255;
  const b = parseInt(m.slice(4, 6), 16) / 255;
  return 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.45 ? "#06110f" : "#f2faf9";
}

function Avatar({ label, color, live }: { label: string; color: string; live?: boolean }) {
  return (
    <span
      className={`avatar ${live ? "live-dot" : ""}`}
      style={{
        background: color,
        color: inkOn(color),
        boxShadow: live ? `0 0 10px ${color}` : undefined,
      }}
      aria-hidden
    >
      {(label.trim().charAt(0) || "?").toUpperCase()}
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
  intermission,
  reads,
}: {
  speech: Speech;
  label: string;
  color: string;
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
        <Avatar label={label} color={color} />
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

export function Transcript({
  speeches,
  live,
  colorOf,
  labelOf,
  polls,
  summarizerId,
  reads,
  query = "",
  initialCollapsed,
}: {
  speeches: Speech[];
  live: LiveTurn | null;
  colorOf: (id: string) => string;
  labelOf: (id: string) => string;
  polls: Poll[];
  /** The seat that lays rounds out: its speeches render as intermissions. */
  summarizerId: string | null;
  /** Pages read, by turn. */
  reads: Record<number, ReadPage[]>;
  /** Case-insensitive search across speech text and action lines. */
  query?: string;
  /** Rounds to fold away on first paint, e.g. everything but the last. */
  initialCollapsed?: number[];
}) {
  // Collapse state lives here so the live chamber and the reader get it for
  // free, and so it survives the transcript re-rendering on every token.
  const [collapsed, setCollapsed] = useState<Set<number>>(
    () => new Set(initialCollapsed ?? []),
  );
  const toggle = (r: number) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (!next.delete(r)) next.add(r);
      return next;
    });
  // Memoized: the transcript re-renders per streamed token, and the naive
  // O(rounds × speeches) scan on every token stalled long debates.
  // Must sit above the empty-state early return: a hook after it changes the
  // hook count the moment the first speech lands, and React blanks the tree
  // ("rendered more hooks than during the previous render").
  const { q, rounds, visible } = useMemo(() => {
    const qq = query.trim().toLowerCase();
    const rr = [...new Set(speeches.map((sp) => sp.round))].sort((a, b) => a - b);
    const vv = new Set(
      qq
        ? rr.filter((r) =>
            speeches.some(
              (sp) =>
                sp.round === r &&
                (sp.text.toLowerCase().includes(qq) ||
                  sp.actions.some((a) => a.toLowerCase().includes(qq))),
            ),
          )
        : rr,
    );
    return { q: qq, rounds: rr, visible: vv };
  }, [query, speeches]);
  if (!speeches.length && !live) {
    return (
      <div className="mx-auto mt-10 max-w-[52ch] text-center">
        <div className="mx-auto mb-3 flex size-10 items-center justify-center rounded-xl bg-surface-2 text-ink-3">
          <ChatsCircle size={20} />
        </div>
        <p className="text-[14.5px] font-semibold tracking-tight text-ink">
          Put a motion to the council and press Convene.
        </p>
        <p className="mx-auto mt-1.5 max-w-[44ch] text-[12.5px] leading-relaxed text-ink-2">
          Each model argues, then acts: raises points, rates every point win / lose /
          stalemate, concedes, and polls the room. The board decides, not the prose.
        </p>
      </div>
    );
  }

  const openPolls = polls.filter((p) => !p.closed);
  const shutPolls = polls.filter((p) => p.closed);

  // Rounds, with the speeches in each. A six-round debate is twenty-four
  // speeches in one undifferentiated column, separable only by a small "R3"
  // badge; nothing said where round two ended and three began.
  return (
    <div className="max-w-[80ch]">
      {q && (
        <p className="mb-2 text-[11.5px] text-ink-3" role="status">
          {visible.size === 0
            ? `nothing matches "${query.trim()}"`
            : `matching in round${visible.size === 1 ? "" : "s"} ${[...visible]
                .sort((a, b) => a - b)
                .join(", ")}`}
        </p>
      )}
      <div className="divide-y divide-line">
        {rounds.map((round) => {
          if (q && !visible.has(round)) return null;
          const inRound = speeches.filter((sp) => sp.round === round);
          const open = !q && !collapsed.has(round);
          return (
            <section key={round} aria-label={`round ${round}`}>
              {rounds.length > 1 && (
                <h3 className="sticky top-0 z-1 -mx-1 flex items-center gap-2 bg-surface px-1 py-1.5 text-[10.5px] font-semibold tracking-[0.1em] text-ink-3 uppercase">
                  <button
                    type="button"
                    className="inline-flex cursor-pointer items-center gap-1 rounded px-1 py-0.5 hover:text-ink"
                    onClick={() => toggle(round)}
                    aria-expanded={open}
                    aria-controls={`round-${round}-speeches`}
                    title={open ? "collapse this round" : "expand this round"}
                  >
                    {open ? <CaretDown size={12} /> : <CaretRight size={12} />}
                    round {round}
                  </button>
                  <span className="tabular-nums">
                    {inRound.length} speech{inRound.length === 1 ? "" : "s"}
                  </span>
                  {!open && !q && (
                    <span className="font-normal normal-case">· collapsed</span>
                  )}
                </h3>
              )}
              {(open || q) && (
                <div id={`round-${round}-speeches`}>
                  {inRound.map((sp) => (
                    <SpeechCard
                      key={sp.id}
                      speech={sp}
                      label={labelOf(sp.delegate)}
                      color={colorOf(sp.delegate)}
                      intermission={summarizerId !== null && sp.delegate === summarizerId}
                      reads={reads?.[sp.id]}
                    />
                  ))}
                </div>
              )}
            </section>
          );
        })}

        {live && (
          <article
            className={`rise py-4 ${summarizerId !== null && live.delegate === summarizerId ? "border-l-2 border-dashed border-line-2 pl-3" : ""}`}
            aria-label={`${live.label} speaking`}
          >
            <header className="mb-1.5 flex flex-wrap items-center gap-x-2 gap-y-1">
              <Avatar label={live.label} color={live.color} live />
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
              <TurnState live={live} />
            </header>
            <Thought
              text={live.reasoning}
              defaultOpen
              closeOnContent={live.text.length > 0}
            />
            {live.text ? (
              <>
                <Md text={live.text} />
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
        )}
      </div>

      {openPolls.length > 0 && (
        <section className="mt-1" aria-live="polite">
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

export function RunTrace({ state }: { state: CouncilState }) {
  // Clamped: `total` is multiplied by the seat count into one DOM node per
  // round-seat, so an unvalidated round count from storage was a way to hang
  // the tab before the first model answered.
  const total = Math.min(Math.max(state.maxRounds, state.round, 1), MAX_ROUND_TICKS);
  const done = new Set(state.speeches.map((sp) => `${sp.round}:${sp.delegate}`));
  const rounds = Array.from({ length: total }, (_, i) => i + 1);
  return (
    <div
      className="flex items-center gap-4 overflow-x-auto"
      role="status"
      aria-label="debate progress by round and seat"
    >
      {rounds.map((r) => {
        const isPast = r < state.round || (!state.running && state.verdict != null);
        return (
          <div key={r} className="flex shrink-0 items-center gap-1.5">
            <span
              className={`font-mono text-[10px] tabular-nums ${
                r === state.round && state.running ? "font-bold text-ink" : "text-ink-3"
              }`}
            >
              R{r}
            </span>
            {state.delegates.map((d) => {
              const spoke = done.has(`${r}:${d.id}`);
              const isLive =
                state.live?.delegate === d.id && state.live.round === r;
              const wasSkipped =
                !spoke && !isLive && state.skipped.some((s) => s.round === r && s.ids.includes(d.id));
              return (
                <span
                  key={d.id}
                  title={`${d.label} · round ${r} · ${
                    spoke ? "spoken" : isLive ? "speaking" : wasSkipped ? "skipped" : isPast ? "silent" : "upcoming"
                  }`}
                  className={`size-2 rounded-full ${
                    spoke ? "" : wasSkipped ? "border border-dashed border-warn" : "border border-line-2"
                  } ${isLive ? "live-dot" : ""}`}
                  style={
                    spoke || isLive
                      ? { background: d.color, borderColor: "transparent" }
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
}

function PollCard({ poll, labelOf }: { poll: Poll; labelOf: (id: string) => string }) {
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
        opened by {labelOf(poll.openedBy)} · {cast} ballot{cast === 1 ? "" : "s"}
        {!poll.closed && <span> · open</span>}
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
          <span className="text-ink-3">awaiting ballots…</span>
        ) : outcome ? (
          <span className="font-medium text-ok">carried: {outcome}</span>
        ) : (
          <span className="text-warn">inconclusive — no majority</span>
        )}
      </div>
    </div>
  );
}

export function BoardPanel({
  points,
  delegates,
  tallyOf,
  labelOf,
  voteLog,
}: {
  points: Point[];
  delegates: { id: string; label: string; color: string; role?: string }[];
  tallyOf: (id: number) => Tally | undefined;
  labelOf: (id: string) => string;
  /** Changes of mind per point, when they are known. */
  voteLog?: Record<number, VoteChange[]>;
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
        <p className="text-[12.5px] font-medium text-ink-2">No points on the board yet.</p>
        <p className="mx-auto mt-1 max-w-[26ch] text-[11.5px] leading-relaxed text-ink-3">
          Claims appear here the moment a delegate raises them.
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

  return (
    <div className="flex flex-col gap-2 p-2.5">
      <div className="px-1 pt-1">
        <div className="flex gap-3 text-[10.5px] tracking-[0.08em] uppercase">
          <span className="text-ok tabular-nums">{carried} carried</span>
          <span className="text-bad tabular-nums">{fell} fell</span>
          <span className="text-warn tabular-nums">{stuck} tied</span>
          {pulled > 0 && (
            <span className="text-ink-3 tabular-nums">{pulled} pulled</span>
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
                title={`${t.wins} for, ${t.losses} against, ${t.stalemates} tied — ${
                  t.abstentions
                } of ${t.wins + t.losses + t.stalemates + t.abstentions} seats have not voted`}
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
                <div className="text-[9px] font-semibold tracking-[0.08em] text-ink-3 uppercase">
                  ruled by {labelOf(p.ruling.by)} · R{p.ruling.round}
                </div>
                <p className="text-[12px] leading-relaxed text-ink-2">
                  This point{" "}
                  <strong className={p.ruling.carries ? "text-ok" : "text-bad"}>
                    {p.ruling.carries ? "carries" : "falls"}
                  </strong>{" "}
                  whatever the votes said — {p.ruling.reason}
                </p>
              </div>
            )}

            {p.beats.length > 0 && (
              <div className="mt-1 mb-1 text-[11px] text-ink-3">
                <span className="text-ink-2">defeats</span>{" "}
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
                const label = p.votes[d.id]?.reason
                  ? `${d.label}: ${shown} — ${p.votes[d.id]!.reason}`
                  : implicit
                    ? `${d.label}: ${shown} (implicit — the point's own proponent)`
                    : `${d.label}: no vote yet`;
                return (
                  <span
                    key={d.id}
                    tabIndex={0}
                    aria-label={label}
                    className={`inline-flex items-center gap-1.5 rounded-pill border px-2 py-px text-[10px] ${
                      shown ? "text-ink-2" : "border-dashed text-ink-3"
                    }`}
                    style={{ borderColor: shown ? d.color : undefined }}
                    title={label}
                  >
                    <span className="size-1.5 rounded-full" style={{ background: d.color }} />
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
                    title="seats that changed their mind on this point"
                  >
                    {voteLog[p.id].length} change{voteLog[p.id].length === 1 ? "" : "s"} of mind
                    <span className="ml-1 text-ink-3 group-open:hidden">▸</span>
                  </summary>
                  <ul className="mt-1 flex flex-col gap-0.5 border-l-2 border-line-2 pl-2">
                    {voteLog[p.id].map((c, i) => (
                      <li key={i} className="text-[11px] leading-relaxed text-ink-3">
                        <span className="font-mono">R{c.round}</span>{" "}
                        <span className="text-ink-2">{labelOf(c.delegate)}</span>{" "}
                        {c.from ? (
                          <>
                            <span className={GLYPH_COLOR[c.from]}>{c.from}</span> →{" "}
                          </>
                        ) : (
                          <>→ </>
                        )}
                        <span className={GLYPH_COLOR[c.to]}>{c.to}</span>
                        {c.reason && <span className="ml-1">— {c.reason}</span>}
                      </li>
                    ))}
                  </ul>
                </details>
              </div>
            )}

            {!t.contested && t.status !== "withdrawn" && (
              <div className="mt-1.5 text-[11px] text-ink-3">
                nobody else has engaged with this yet
              </div>
            )}

            {p.rebuttals.length > 0 && <Remarks tag="rebut" remarks={p.rebuttals} />}
            {p.steelmen.length > 0 && <Remarks tag="steelman" remarks={p.steelmen} />}
            {p.evidence.length > 0 && <Remarks tag="evidence" remarks={p.evidence} />}
          </article>
        );
      })}

      <p className="px-1 pt-0.5 text-[11px] leading-relaxed text-ink-3">
        Every delegate rates every point. A point's own proponent counts as a win until they vote
        otherwise, concede it, or withdraw it.
      </p>
    </div>
  );
}

/**
 * Every remark on a point, not only the last.
 *
 * Showing only the final beat meant an exchange that took three rounds to
 * resolve — steelman, rebut, re-rebut — read as a single remark, and the work
 * that changed the point's mind vanished. The last one stays open because it is
 * the one that counts now.
 */
function Remarks({ tag, remarks }: { tag: string; remarks: RemarkRec[] }) {
  if (remarks.length === 1) return <Remark tag={tag}>{remarks[0]!.text}</Remark>;
  return (
    <div className="mt-1.5">
      <details open>
        <summary className="cursor-pointer list-none text-[9px] font-semibold tracking-[0.08em] text-ink-3 uppercase hover:text-ink-2">
          {remarks.length}× {tag}
          <span className="ml-1 font-normal tracking-normal normal-case">
            (latest first)
          </span>
        </summary>
        <div className="flex flex-col gap-1.5">
          {[...remarks].reverse().map((r, i) => (
            <Remark key={i} tag={tag}>
              {r.text}
            </Remark>
          ))}
        </div>
      </details>
    </div>
  );
}

function Remark({ tag, children }: { tag: string; children: string }) {
  return (
    <div className="mt-1.5 border-l-2 border-line-2 pl-2">
      <div className="text-[9px] font-semibold tracking-[0.08em] text-ink-3 uppercase">
        {tag}
      </div>
      <Md text={children} compact />
    </div>
  );
}
