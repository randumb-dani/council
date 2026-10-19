import { lazy, memo, Suspense, useCallback, useDeferredValue, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { ArrowDown } from "@phosphor-icons/react/dist/csr/ArrowDown";
import { BookOpen } from "@phosphor-icons/react/dist/csr/BookOpen";
import { Info } from "@phosphor-icons/react/dist/csr/Info";
import { PaperPlaneTilt } from "@phosphor-icons/react/dist/csr/PaperPlaneTilt";
import { Sidebar } from "@phosphor-icons/react/dist/csr/Sidebar";
import { SkipForward } from "@phosphor-icons/react/dist/csr/SkipForward";
import { Warning } from "@phosphor-icons/react/dist/csr/Warning";
import { X } from "@phosphor-icons/react/dist/csr/X";
import { XCircle } from "@phosphor-icons/react/dist/csr/XCircle";
import { CouncilRail } from "./components/CouncilRail";
import {
  BoardPanel,
  fmtElapsed,
  MAX_ROUND_TICKS,
  RunTrace,
  Transcript,
  TranscriptSearch,
  useRunTraceView,
} from "./components/Panels";
import { SetupPanel } from "./components/SetupPanel";
import {
  imagesStorageKey,
  loadConfig,
  resetConfig,
  REQUIRED_ROLE_NAME,
  STORAGE_KEY,
  HUMAN_STORAGE_KEY,
} from "./configStore";
import { RecordPanel, councilLabel, endLabel, endTitle } from "./components/RecordPanel";
// The reader route and the operator's seat panel load on demand, not on first
// paint: the default landing is the library, which needs neither. `SetupPanel`
// and `HomeView` stay in the entry — the chamber shows the setup column by
// default and the library is the first paint, so splitting those would trade a
// smaller entry for a fallback flash on the very first screen.
const OperatorPanel = lazy(() =>
  import("./components/OperatorPanel").then((m) => ({ default: m.OperatorPanel })),
);
const ReaderView = lazy(() =>
  import("./components/ReaderView").then((m) => ({ default: m.ReaderView })),
);
/**
 * What a lazy chunk shows while it loads. A spinner with a status role, not a
 * skeleton of the thing coming — the wait is one local round trip, and a
 * skeleton that guesses the layout wrong is worse than an honest spinner.
 */
function ChunkFallback() {
  return (
    <div className="flex items-center justify-center gap-2 p-8 text-[12.5px] text-ink-3" role="status">
      <span className="spinner" aria-hidden /> Loading…
    </div>
  );
}
import { TabPanel, TabStrip } from "./components/Tabs";
import { Wordmark } from "./components/Wordmark";
import { HomeView } from "./components/HomeView";
import { newDebateId, saveDebate } from "./debates";
import { inTauri, useServedMode } from "./bridge";
import { configHasSecrets, stripSecrets } from "./serverBridge";
import {
  applyResumeDraft,
  clearResumeDraft,
  peekResumeDraft,
  recordConsumed,
  type ResumeDraft,
} from "./resume";
import { canBeTakenByOperator, type DebateConfig, type HumanBallotSubmit, type HumanTurnSubmit } from "./types";
import { useCouncil } from "./useCouncil";
import { ThemePicker } from "./components/ThemePicker";
import { DEFAULT_THEME, THEME_KEY, isThemeId, themeMeta, type ThemeId } from "./themes";
/**
 * What the ending actually was. The pill used to say "adjourned" for every
 * ending, which made an early stop indistinguishable from ten full rounds.
 */
/**
 * The summarizer's request for more rounds, as a modal over a blurred debate.
 *
 * It is a modal rather than a strip because it is a decision, not a status: the
 * thing is in the middle of the room asking, and the operator should not have
 * to find it. The backdrop blur says the same — what is behind cannot be acted
 * on until this is answered.
 *
 * The council votes on it too. The buttons are a veto, not the mechanism: leave
 * them alone and the room decides, and the tally below shows that happening
 * rather than a number appearing at the end.
 */
/**
 * The current time, refreshed only while something is running.
 *
 * A timer that ticks forever costs a render a second for a debate that ended
 * an hour ago, so it sleeps when there is nothing to count.
 */
function useNow(running: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!running) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [running]);
  return now;
}
/**
 * How long this debate has been going, in total.
 *
 * Per-turn timers already exist and they are the wrong answer: the interesting
 * number is the time spent, and a debate whose turns are each a few seconds can
 * still be an hour of waiting on one seat that stopped replying.
 */
function SessionClock({ startedAt, endedAt, running }: {
  startedAt: number | null;
  endedAt: number | null;
  running: boolean;
}) {
  const now = useNow(running);
  if (startedAt === null) return null;
  const end = endedAt ?? now;
  const secs = Math.max(0, Math.floor((end - startedAt) / 1000));
  return (
    <span
      className="shrink-0 font-mono text-[11.5px] tabular-nums text-ink-3"
      title={`${running ? "Time so far" : "Total time"}, measured from the first turn`}
    >
      {fmtElapsed(secs)}
    </span>
  );
}

/**
 * The extension modal only changes when the proposal does. Memoized so a
 * streamed token does not rebuild it: `ballots` grows per ballot (a
 * legitimate re-render) but holds identity across tokens.
 */
const ExtensionModal = memo(function ExtensionModal({
  rounds,
  reason,
  round,
  budget,
  ballots,
  labelOf,
  onResolve,
}: {
  rounds: number;
  reason: string;
  round: number;
  budget: number;
  /** Every seat asked so far, and what it said. */
  ballots: { delegate: string; grant: boolean; reason: string | null }[];
  labelOf: (id: string) => string;
  onResolve: (approved: boolean, rounds: number) => Promise<unknown> | void;
}) {
  const [count, setCount] = useState(rounds);
  const [busy, setBusy] = useState(false);
  const dialogRef = useRef<HTMLDivElement | null>(null);
  // One proposal, one dispatch at a time. `busy` alone was released by the
  // deadline below while the fetch was still in flight, so a second click
  // resolved the same proposal twice; this guards concurrent dispatches and is
  // released together with `busy`, so a hung request still becomes retryable.
  const answeredRef = useRef(false);
  // Clamped on sync, not only on change. The Summarizer can ask for more rounds
  // than the operator has left, and the old code copied `rounds` into the input
  // verbatim: the box showed the figure the model wanted while the label beside
  // it read "2 extra rounds left", and Extend dispatched the over-budget number.
  useEffect(() => {
    const cap = Math.max(1, budget);
    setCount(Math.min(Math.max(1, Math.floor(rounds) || 1), cap));
    setBusy(false);
    answeredRef.current = false;
  }, [rounds, round, budget]);
  // Focus the decision itself on open, and keep tab inside while it is open.
  // The dialog used to focus the first button ("No"), so a hasty Enter
  // declined a request the operator meant to grant — and tab reached Stop and
  // Convene behind the blur.
  useEffect(() => {
    dialogRef.current?.querySelector<HTMLButtonElement>("[data-extend]")?.focus();
  }, []);
  const trapTab = (e: React.KeyboardEvent) => {
    if (e.key !== "Tab") return;
    const root = dialogRef.current;
    if (!root) return;
    const items = Array.from(
      root.querySelectorAll<HTMLElement>("button:not([disabled]), input:not([disabled])"),
    );
    if (items.length === 0) return;
    const first = items[0]!;
    const last = items[items.length - 1]!;
    const active = document.activeElement as HTMLElement | null;
    if (e.shiftKey && (active === first || !root.contains(active))) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  };
  // A second click would hit an engine that has already taken the proposal and
  // raise a red error for something the operator did correctly.
  //
  // Released on whichever comes first, the request settling or a deadline. No
  // serverBridge fetch carries a timeout, and a phone walking out of Wi-Fi range
  // mid-request leaves the connection established with nothing coming back — so
  // `.finally` never ran, `busy` stayed true, both buttons and Escape were dead,
  // and because this modal covers the header, Stop was unreachable too. The debate
  // could not be stopped at all without a reload.
  const answer = (approved: boolean, n: number) => {
    if (busy || answeredRef.current) return;
    answeredRef.current = true;
    setBusy(true);
    let settled = false;
    const release = () => {
      if (settled) return;
      settled = true;
      window.clearTimeout(deadline);
      answeredRef.current = false;
      setBusy(false);
    };
    const deadline = window.setTimeout(release, 12_000);
    try {
      const r = onResolve(approved, n) as unknown;
      if (r && typeof (r as Promise<unknown>).finally === "function") {
        (r as Promise<unknown>).finally(release);
      } else {
        window.setTimeout(release, 1500);
      }
    } catch {
      release();
    }
  };
  const max = Math.max(1, budget);
  const clamp = (n: number) => (Number.isFinite(n) ? Math.min(max, Math.max(1, Math.floor(n))) : 1);
  const yes = ballots.filter((b) => b.grant).length;
  const no = ballots.length - yes;
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-canvas/60 p-4 backdrop-blur-sm"
      role="dialog"
      aria-modal="true"
      aria-labelledby="ext-title"
      aria-describedby="ext-reason"
      onKeyDown={(e) => {
        // Escape closes. It used to cast a declining ballot, because the
        // dialog only had two outcomes and dismissing was one of them — so the
        // standard "get me out of here" gesture silently voted against the
        // Summarizer, and the copy promised a "do nothing" path that did not
        // exist anywhere in the UI. Deferring is now a labelled button, and
        // Escape means only "stop showing me this".
        if (e.key === "Escape") {
          e.stopPropagation();
          if (!busy) answer(false, 0);
          return;
        }
        trapTab(e);
      }}
    >
      {/* `items-center` splits overflow off the top of a too-tall flex child, so
          an unbounded `reason` plus five ballots made the whole card — including
          the Extend button — unreachable on a half-height window. Capped and
          scrolled instead, with the scroll on the body of the dialog rather than
          the backdrop so a long proposal does not drag the buttons away. */}
      <div ref={dialogRef} className="card rise flex max-h-[min(85dvh,42rem)] w-full max-w-md flex-col p-5">
        <div className="flex shrink-0 items-center gap-2">
          <span className="size-2 shrink-0 rounded-full bg-warn live-dot" />
          <h2 id="ext-title" className="text-[14px] font-semibold tracking-tight text-ink">
            More rounds?
          </h2>
          <span className="ml-auto text-[11px] text-ink-3 tabular-nums">after R{round}</span>
        </div>
        {/* Only this scrolls. The heading and the buttons stay put, so a verbose
            Summarizer proposal can be read without losing the decision to it. */}
        <div className="mt-2.5 min-h-0 flex-1 overflow-y-auto overscroll-contain">
        <p className="text-[12.5px] leading-relaxed text-ink-2">
          <strong className="font-semibold text-ink">Summarizer</strong> says:
        </p>
        <p id="ext-reason" className="mt-1 border-l-2 border-line-2 pl-2.5 text-[12.5px] leading-relaxed text-ink-2">
          {reason}
        </p>
        {/* The count building, so the room is visibly deciding. */}
        <div className="mt-3.5 border-t border-line pt-3">
          <div className="flex items-baseline justify-between text-[11.5px]">
            <span className="text-ink-3">the delegates have voted</span>
            <span className="tabular-nums">
              <b className="text-ok">{yes}</b> for
              <span className="mx-1 text-ink-3">·</span>
              <b className="text-bad">{no}</b> against
            </span>
          </div>
          <ul className="mt-1.5 flex flex-col gap-0.5">
            {ballots.map((b) => (
              <li key={b.delegate} className="flex items-baseline gap-1.5 text-[11.5px]">
                <span className="text-ink-2">{labelOf(b.delegate)}</span>
                <span className={b.grant ? "text-ok" : "text-bad"}>
                  {b.grant ? "for" : "against"}
                </span>
                {b.reason && <span className="min-w-0 flex-1 text-ink-3">— {b.reason}</span>}
                {!b.reason && (
                  <span className="text-ink-3">— gave no reason</span>
                )}
              </li>
            ))}
            {ballots.length === 0 && (
              <li className="text-[11.5px] text-ink-3">waiting for their votes…</li>
            )}
          </ul>
        </div>
        <p className="mt-3 text-[11.5px] leading-relaxed text-ink-3">
          Do nothing and the delegates decide among themselves — most of them win.
          You can still answer it yourself below.
        </p>
        </div>
        <div className="mt-3 flex shrink-0 flex-wrap items-center gap-2 border-t border-line pt-3">
          <label className="flex items-center gap-1.5 text-[12px] text-ink-2">
            rounds
            <input
              type="number"
              min={1}
              max={max}
              value={count}
              onChange={(e) => setCount(clamp(Number(e.target.value)))}
              className="field !w-16 !py-1 text-center tabular-nums"
            />
          </label>
          <span className="text-[11px] text-ink-3 tabular-nums">
            {budget} extra round{budget === 1 ? "" : "s"} left{budget <= 0 ? " — the delegates decide" : ""}
          </span>
          <div className="ml-auto flex gap-2">
            {/* Named for what it does rather than for the answer. The copy above
                promises "do nothing and the delegates decide", and until now the
                only way to do that was a bare "No" next to the number you were
                about to change — which reads as "no rounds" and not "let them
                settle it". This is the action the paragraph describes, so it
                says so. */}
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy}
              aria-busy={busy}
              onClick={() => answer(false, 0)}
            >
              Let them decide
            </button>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || budget <= 0}
              data-extend
              aria-busy={busy}
              title={budget <= 0 ? "no extra rounds left — the council decides" : "grant extra rounds"}
              onClick={() => answer(true, count)}
            >
              Extend
            </button>
          </div>
        </div>
      </div>
    </div>
  );
});
/**
 * Round progress as segments: filled behind, pulsing ahead-of-the-speaker at
 * the current round, hollow for the road left. The top bar is the one place
 * the whole debate's pace is always visible, so it lives here and not only in
 * the trace strip.
 *
 * Props are primitives, not the state, so this bails per streamed token: it
 * only changes when the round, the running flag, or the verdict does.
 */
const RoundProgress = memo(function RoundProgress({
  round,
  running,
  verdictRounds,
  total,
}: {
  round: number;
  running: boolean;
  verdictRounds: number | null;
  total: number;
}) {
  const n = Math.max(1, Math.min(total, MAX_ROUND_TICKS));
  const rounds = Array.from({ length: n }, (_, i) => i + 1);
  /* Ticks are fixed 1px rather than `flex-1 min-w-1` inside a `max-w-36` box.
     That combination could not get below 4px, so 40 ticks needed ~160px in a
     144px box with nothing clipping it — the row painted over the pill's rounded
     background and onto the Skip and Stop buttons next to it. Reachable: the
     setup sliders allowed 20 rounds plus 20 extra, and an extension grant raises
     the total to 40. Fixed width degrades to a thin solid bar past ~30 rounds,
     which still shows how far along the debate is. */
  return (
    <span className="flex items-center gap-[3px] overflow-hidden" title={`${total} rounds`}>
      {rounds.map((r) => {
        const done = r < round || (verdictRounds !== null && r <= verdictRounds);
        const current = r === round && running;
        return (
          <i
            key={r}
            /* `shrink-0` rather than `flex-1` past the cap: `min-w-1` with
               `flex-1` still could not get below 4px, so 40 ticks needed ~160px
               in a 144px box. Fixed 1px plus a 3px gap degrades to a thin solid
               bar past ~30 rounds, which still shows where the debate is. */
            className={`h-1 w-px shrink-0 rounded-full ${
              done ? "bg-primary" : current ? "bg-primary live-dot" : "bg-line-2"
            }`}
          />
        );
      })}
    </span>
  );
});
/**
 * The motion line decodes itself out of binary when a debate opens, after the
 * fashion of Prism's headline. Pure ornament, so it bows out for long motions
 * and reduced-motion users, and it never changes the laid-out length.
 *
 * `animate` is true only for the convened motion: the header also mirrors the
 * setup draft as it is typed, and re-running a 48-frame animation on every
 * keystroke made the whole header flicker while writing.
 */
function Decode({ text, animate }: { text: string; animate: boolean }) {
  const [shown, setShown] = useState(text);
  const committed = useRef(text);
  useEffect(() => {
    if (committed.current === text) return;
    committed.current = text;
    if (
      !animate ||
      !text ||
      text.length > 160 ||
      window.matchMedia("(prefers-reduced-motion: reduce)").matches
    ) {
      setShown(text);
      return;
    }
    let frame = 0;
    const total = Math.max(14, Math.min(48, text.length));
    const id = window.setInterval(() => {
      frame += 1;
      const n = Math.floor((frame / total) * text.length);
      let out = text.slice(0, n);
      for (let i = n; i < text.length; i += 1) {
        const c = text[i]!;
        out += c === " " ? " " : Math.random() < 0.5 ? "0" : "1";
      }
      setShown(out);
      if (frame >= total) window.clearInterval(id);
    }, 24);
    return () => window.clearInterval(id);
  }, [text, animate]);
  return <>{shown}</>;
}
const PANELS_KEY = "council.panels.v1";
function loadPanels(): { left: boolean; right: boolean } {
  try {
    const raw = window.localStorage.getItem(PANELS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as { left?: unknown; right?: unknown };
      return {
        left: typeof parsed.left === "boolean" ? parsed.left : true,
        right: typeof parsed.right === "boolean" ? parsed.right : true,
      };
    }
  } catch {
    /* fall through to defaults */
  }
  return { left: true, right: true };
}
function Chamber({
  theme,
  onTheme,
  onLibrary,
  human = false,
}: {
  theme: ThemeId;
  onTheme: (id: ThemeId) => void;
  onLibrary: () => void;
  /**
   * The human-in-the-loop council: the operator takes one arguing seat and the
   * engine waits for them. A separate route from the automatic debate, which
   * is left exactly as it was — nothing here changes when this is false.
   */
  human?: boolean;
}) {
  const storageKey = human ? HUMAN_STORAGE_KEY : STORAGE_KEY;
  const [config, setConfig] = useState<DebateConfig>(() => loadConfig(storageKey));
  // A resume draft waiting from the reader page: the same motion and roster,
  // with the unfinished record handed over. Read once, on mount, and cleared —
  // Peeked, not consumed. `takeResumeDraft` deletes as it reads, and React
  // double-invokes `useState` initializers under `StrictMode`, keeping the *second*
  // result — so the first call ate the draft and threw it away. Resume therefore did
  // nothing in dev while working in a production build, which is the worst possible
  // way for it to break. The effect below offers it once, after the first commit.
  const [resumed] = useState(() => peekResumeDraft());
  const [panels, setPanels] = useState(loadPanels);
  const [note, setNote] = useState("");
  // Sending is async, so a second Enter mid-flight would inject the same note
  // twice — and the box is only cleared on success, so the text is still there
  // to be re-sent.
  const [sendingNote, setSendingNote] = useState(false);
  // Whether the operator has unsent work on their seat right now. Both Skip
  // buttons — this one in the panel, one in the header — must ask before
  // throwing it away, and the panel is the only thing that knows.
  const [operatorHasDraft, setOperatorHasDraft] = useState(false);
  const noteRef = useRef<HTMLTextAreaElement | null>(null);
  const [query, setQuery] = useState("");
  const [tab, setTab] = useState<"board" | "record">("board");
  const [hidden, setHidden] = useState<Set<number>>(new Set());
  const [showAllNotices, setShowAllNotices] = useState(false);
  const council = useCouncil();
  const sendNote = useCallback(async () => {
    const text = note;
    if (!text.trim() || sendingNote) return;
    setSendingNote(true);
    const landed = await council.say(text);
    setSendingNote(false);
    // Cleared on success only, so a failed or undeliverable note stays typed and
    // the operator can retry or copy it out before doing anything else.
    if (landed) {
      setNote("");
      if (noteRef.current) noteRef.current.style.height = "auto";
    }
  }, [council.say, note, sendingNote]);
  const resetSetup = useCallback(() => resetConfig(storageKey), [storageKey]);
  const skipTurn = useCallback(() => {
    if (
      operatorHasDraft &&
      !window.confirm("Skip this turn? Your typed speech and any staged moves will be discarded.")
    ) {
      return;
    }
    // The draft itself is cleared by the panel, which owns the sessionStorage key.
    setOperatorHasDraft(false);
    void council.skip();
  }, [council.skip, operatorHasDraft]);
  // Seat the resumed roster after the first commit, reading the live config through
  // a ref so this fires exactly once. It also sets up a render, so doing it in the
  // initializer would mean a `setConfig` during render — and StrictMode would run
  // the whole thing twice.
  const configRef = useRef(config);
  configRef.current = config;
  const [resumeNotice, setResumeNotice] = useState<string | null>(null);
  // Offered, never auto-applied: silently overwriting the operator's current
  // setup (motion, roster, saved instructions) with a stale draft was data loss
  // with no undo. The draft waits until they accept or discard it.
  const [resumeOffer, setResumeOffer] = useState<ResumeDraft | null>(null);
  const resumeDone = useRef(false);
  useEffect(() => {
    if (resumeDone.current) return;
    resumeDone.current = true;
    if (!resumed) return;
    setResumeOffer(resumed);
  }, [resumed]);
  const applyResume = (draft: ResumeDraft) => {
    setConfig(applyResumeDraft(configRef.current, draft));
    // Consumed here rather than on read, so a discarded StrictMode mount cannot
    // destroy it and the operator can come back to it.
    clearResumeDraft();
    setResumeOffer(null);
    const held = draft.delegates.filter((d) => d.human).map((d) => d.label).join(", ");
    setResumeNotice(
      "Resumed from an unfinished debate: the same motion, the same delegates, and the record handed to everyone. Pick the models — the board starts empty." +
        (held ? ` Seats you held stay yours: ${held}.` : ""),
    );
  };
  const discardResume = () => {
    clearResumeDraft();
    setResumeOffer(null);
  };
  // The seat that lays rounds out, resolved once per roster change — not per
  // token. `council.answer`/`resolve` are stable (built on a stable `notice`),
  // so these wrappers hold identity across tokens and `OperatorPanel`'s and
  // `ExtensionModal`'s memos can bail.
  const { state } = council;
  const summarizerId = useMemo(
    () => state.delegates.find((d) => d.role === "summarizer")?.id ?? null,
    [state.delegates],
  );
  const submitTurn = useCallback(
    (turn: HumanTurnSubmit) => council.answer({ turn }),
    // `council` itself is a fresh object per render; the actions on it are
    // stable callbacks, so depend on those, not the bag.
    [council.answer],
  );
  const submitBallot = useCallback(
    (ballot: HumanBallotSubmit) => council.answer({ ballot }),
    [council.answer],
  );
  const resolveExtension = useCallback(
    (approved: boolean, n: number) => council.resolve(approved, n),
    [council.resolve],
  );
  // The search scan runs over every speech per keystroke; the input stays
  // immediate while the scan runs deferred, so typing never janks.
  const deferredQuery = useDeferredValue(query);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  // pinnedRef drives the scroll math (no re-render per token); `pinned` state
  // only decides whether the jump-to-latest button shows.
  const pinnedRef = useRef(true);
  const [pinned, setPinned] = useState(true);
  // The pending scroll write, so several tokens inside one frame cost one
  // layout read rather than one each.
  const frame = useRef<number | null>(null);
  const setPin = (v: boolean) => {
    if (pinnedRef.current === v) return;
    pinnedRef.current = v;
    setPinned(v);
  };
  const served = useServedMode();
  // The setup autosaves behind typing (750ms), and light: image bytes live
  // under their own key (see imagesStorageKey) and are written only when they
  // change, so a motion keystroke no longer re-stringifies megabytes. A save
  // that fails warns once instead of every keystroke — but it warns, because
  // memory and disk have diverged and a reload would lose the difference.
  // `council` is a fresh object every render, so it stays out of the deps: the
  // ref carries the latest notice without resetting the debounce on tokens.
  const noticeRef = useRef(council.notice);
  noticeRef.current = council.notice;
  const saveWarnedRef = useRef(false);
  useEffect(() => {
    const t = window.setTimeout(() => {
      try {
        // In served mode keys live on the server, never in the browser: blank
        // them before persisting so an old stored key cannot leak back out.
        const toSave = served ? stripSecrets(config) : config;
        window.localStorage.setItem(storageKey, JSON.stringify({ ...toSave, images: [] }));
        saveWarnedRef.current = false;
      } catch {
        if (!saveWarnedRef.current) {
          saveWarnedRef.current = true;
          noticeRef.current(
            "Setup changes are no longer being saved — the browser's storage is full. Free some space, or they will be lost on reload.",
            "warn",
          );
        }
      }
    }, 750);
    return () => window.clearTimeout(t);
  }, [config, served, storageKey]);
  useEffect(() => {
    try {
      window.localStorage.setItem(imagesStorageKey(storageKey), JSON.stringify(config.images));
    } catch {
      /* the warning above covers a full store */
    }
  }, [config.images, storageKey]);
  // One-time nudge: a setup saved before served mode still holds keys in this
  // browser. They are blanked on send and on save, but the server needs its
  // own copies before a real debate will run.
  const nudgedRef = useRef(false);
  useEffect(() => {
    if (served && !nudgedRef.current && configHasSecrets(config)) {
      nudgedRef.current = true;
      council.notice(
        "This setup still holds endpoint keys in the browser. Served debates send profiles only — store each key on the server under Endpoints first.",
        "warn",
      );
    }
  }, [served, config, council]);
  useEffect(() => {
    try {
      window.localStorage.setItem(PANELS_KEY, JSON.stringify(panels));
    } catch {
      /* ignore */
    }
  }, [panels]);
  // A new debate starts pinned to the bottom of a fresh transcript.
  useEffect(() => {
    if (state.running) {
      setPin(true);
      // Prune dismissals for notices that have scrolled out of the buffer.
      setHidden((h) => {
        if (h.size < 200) return h;
        return new Set();
      });
    }
  }, [state.running]);
  // The record is written as it happens, not exported at the end. A debate is
  // four models and however many rounds you afforded it; until this existed,
  // closing the window threw all of it away with no way back.
  const debateIdRef = useRef<string | null>(null);
  const startedAtRef = useRef<number | null>(null);
  // A full store fails every save: warn once per debate, not once per speech.
  const quotaWarnedRef = useRef<string | null>(null);
  // The save is keyed to *finished speeches*, not to a timer any state change
  // can restart. The old dependency list carried `points`, `tallies`, `reads`
  // and `round` as well, and each of those changes on a single vote — so one
  // point being voted on postponed the write four times over, and a debate could
  // go minutes between saves. The gap that actually loses work is the last
  // speech before the window closes, which the verdict path covers immediately.
  const saveSpeechCount = state.speeches.length;
  const verdict = state.verdict;
  useEffect(() => {
    // A fresh run (new startedAt) gets a fresh library card. The old code
    // never cleared the ref, so convening twice overwrote the first debate.
    if (state.running && state.startedAt !== startedAtRef.current) {
      startedAtRef.current = state.startedAt;
      debateIdRef.current = newDebateId();
    }
    const id = debateIdRef.current;
    // Nothing worth keeping before the first turn, and a fresh chamber that has
    // never run should not leave an empty card in the library.
    if (!id || (!state.running && !verdict)) return;
    if (saveSpeechCount === 0) return;
    // Immediate on the verdict — that is the moment someone closes the window.
    const delay = verdict ? 0 : 1200;
    const t = window.setTimeout(() => {
      // The library event fired either way before, so a save that failed on a full
      // store re-read the library, showed the debate missing, and said nothing —
      // the operator closed the window believing it was kept.
      if (!saveDebate(state, id)) {
        if (quotaWarnedRef.current !== id) {
          quotaWarnedRef.current = id;
          council.notice(
            "This debate could not be saved — the browser's storage is full. Export it as JSON now, or free some space.",
            "error",
          );
        }
        return;
      }
      quotaWarnedRef.current = null;
      window.dispatchEvent(new Event("council:library"));
    }, delay);
    return () => window.clearTimeout(t);
    // `state` is read at fire time, not captured at schedule time: the timeout
    // runs 1200ms later and must save the state as it is then, which is the
    // whole reason it is a timer.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [saveSpeechCount, verdict, state.running]);
  // Direct scrollTop assignment: unlike scrollIntoView it cannot be captured
  // by an ancestor scroller, so a streaming transcript that is pinned stays
  // pinned, token after token.
  //
  // In a layout effect so the height is measured after this turn's DOM but
  // before paint — in a passive effect the pin visibly lagged behind the text.
  //
  // Coalesced to one frame: `scrollHeight` is a layout read, and a reasoning
  // model streams faster than the display refreshes, so this was forcing a
  // synchronous layout per token — several times a frame for the same result.
  // The last write of a frame wins, which is the only one visible anyway.
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el || !pinnedRef.current) return;
    if (frame.current !== null) return;
    frame.current = requestAnimationFrame(() => {
      frame.current = null;
      const node = scrollRef.current;
      if (node && pinnedRef.current) node.scrollTop = node.scrollHeight;
    });
    return () => {
      if (frame.current !== null) {
        cancelAnimationFrame(frame.current);
        frame.current = null;
      }
    };
  }, [
    state.speeches,
    state.live?.text,
    state.live?.reasoning,
    state.live?.turn,
    // The array identity, not its length: closing a poll swaps one entry for
    // another, so the length never changes and the tail relayout went
    // uncorrected.
    state.polls,
    state.reviews,
    state.adjourn,
  ]);
  const jumpToLatest = () => {
    setPin(true);
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  };
  // The engine scores; the UI only ever draws its answer.
  const tallyOf = useCallback((id: number) => state.tallies[id], [state.tallies]);
  // Held at a stable identity so `RunTrace`'s memo can bail. A token touches
  // none of the fields this projects, so the grid is not rebuilt mid-sentence.
  const traceView = useRunTraceView(state);
  // Open polls, for the fixed strip above the transcript. Cheap: a poll list is
  // a handful of entries, and this is on the hot path only in that it is
  // recomputed per render.
  const openPolls = useMemo(() => state.polls.filter((p) => !p.closed), [state.polls]);
  // Whether the seat currently holding the floor has streaming on, resolved
  // through that seat's own server rather than assumed. Only read to decide
  // whether silence is a stall worth flagging, so a mis-resolved `false` is
  // harmless (no warning) while a wrong `true` is the false alarm it replaces.
  // Dep is the holding seat's id, not the whole live turn: the object is new
  // per token, but the seat only changes when the floor does. The seat and
  // server lists are stable mid-debate, so this resolves once per turn.
  const liveDelegate = state.live?.delegate ?? null;
  const streamingSpeaker = useMemo(() => {
    if (!liveDelegate) return true;
    // The brief carries the seat's `endpoint_id` verbatim (see `briefs` in
    // engine.rs), so this is a direct lookup. An unknown server resolves to
    // `true` — assume streaming — because the warning is the thing not to get
    // wrong, and a taken seat has no server of its own.
    const seat = state.delegates.find((d) => d.id === liveDelegate);
    const ep = config.endpoints.find((e) => e.id === seat?.endpoint);
    return ep?.stream ?? true;
  }, [liveDelegate, state.delegates, config.endpoints]);
  const problems = useMemo(() => {
    const out: string[] = [];
    // Each message says what is missing and where to add it. The previous text
    // ("no motion", "a delegate points at a deleted endpoint") either named an
    // internal value or described the state rather than the fix.
    // "above" was wrong: the setup panel is a column to the left of the
    // transcript, not above it, so on a wide screen the operator was told to
    // look in the wrong place for the one field that has to be filled in first.
    if (!config.motion.trim()) out.push("no motion yet — set the question in the setup panel");
    // Names have to be unique and non-empty: the engine refuses both, and it refused
    // them *after* this list came back clean, so Convene's tooltip promised to start
    // and the only feedback was a red notice naming an internal rule.
    {
      const seen = new Set<string>();
      const clashes: string[] = [];
      for (const d of config.delegates) {
        const label = d.label.trim();
        if (!label) {
          clashes.push("one delegate has no name — give it one");
          continue;
        }
        const key = label.toLowerCase();
        if (seen.has(key) && !clashes.includes(`two seats are called ${label}`)) {
          clashes.push(`two seats are called ${label} — rename one`);
        }
        seen.add(key);
      }
      out.push(...clashes);
    }
    if (config.delegates.length < 2) {
      out.push("need at least two delegates to argue opposite sides");
    }
    for (const need of ["proponent", "opponent", "judge", "summarizer"] as const) {
      if (!config.delegates.some((d) => d.seatRole === need)) {
        out.push(`no ${REQUIRED_ROLE_NAME[need]} — add it under Delegates`);
      }
    }
    // A seat you argue yourself needs no model and no server — the operator is the
    // model — so it is exempt from both checks. Skipping the exemption is what
    // made the human council refuse to convene with a taken seat.
    const needsModel = (d: DebateConfig["delegates"][number]) => d.human !== true;
    if (config.delegates.some((d) => needsModel(d) && !d.model.trim())) {
      out.push("every delegate needs a model — pick one for each");
    }
    if (config.endpoints.some((e) => !e.baseUrl.trim())) {
      out.push("every server needs an address");
    }
    const known = new Set(config.endpoints.map((e) => e.id));
    if (config.delegates.some((d) => needsModel(d) && !known.has(d.endpointId))) {
      out.push("a delegate is set to a server that no longer exists — pick it another");
    }
    // A taken role the engine refuses: better named here than as a refusal
    // after the button press.
    const illegal = config.delegates.filter((d) => d.human === true && !canBeTakenByOperator(d.seatRole));
    if (illegal.length) {
      out.push(
        `${illegal.map((d) => d.label).join(", ")} hold${illegal.length === 1 ? "s" : ""} a role the models keep — only the debating roles can be taken`,
      );
    }
    // The decision model is optional, but a half-configured one passed this
    // list clean and failed on the first turn instead.
    if (config.decision.provider === "jev") {
      if (!config.decision.baseUrl.trim()) {
        out.push("the decision model has no address — add one under Advanced");
      }
      if (!config.decision.model.trim()) {
        out.push("the decision model names no model — pick one under Advanced");
      }
    }
    return out;
  }, [config]);
  const start = () => {
    if (problems.length) {
      council.notice(`Cannot convene: ${problems.join("; ")}.`, "warn");
      return;
    }
    void council.start(config).then((took) => {
      // The resume record is cleared here, and only here: the engine has it now
      // — it is in the opening brief every delegate reads, built once and kept
      // for the whole run — so the setup has no further use for it. Left in
      // place it came back on the next visit and rode into every unrelated
      // motion afterwards, telling each new delegate it was resuming a debate
      // it had never heard of. A refused start leaves it alone, because the
      // operator is about to fix the setup and try again.
      if (took) setConfig((c) => recordConsumed(c));
    });
  };
  const liveNotices = state.notices.filter((n) => !hidden.has(n.id));
  // Was capped at three with no history, and a poll or review round trip emits
  // one notice per voter, so the actionable ones scrolled off unrecoverably.
  const visibleNotices = showAllNotices ? liveNotices : liveNotices.slice(-3);
  const dismiss = (id: number) => setHidden((h) => new Set(h).add(id));
  return (
    <div className="flex h-full flex-col">
      {/* Wraps rather than clips.
            This row was `flex-nowrap` inside `overflow-hidden`, and its contents
            come to roughly 440px: wordmark, three panel toggles, the theme
            picker, Skip and Stop. At 360px the overflow was cut off at the right
            edge — so **Stop** went missing, and Stop is the one control needed
            when a model hangs. Not scrollable, not reachable by any gesture.
            Everything here is `shrink-0` by design, so the only honest fix is to
            let the row break: below `sm` the actions take a second line instead
            of pushing the important ones off the end. */}
        <header className="safe-top safe-x flex min-h-[52px] shrink-0 flex-wrap items-center gap-x-4 gap-y-1 border-b border-line bg-surface px-4 py-1">
        <Wordmark />
        <div
          className="hidden min-w-0 flex-1 truncate border-l border-line pl-4 font-mono text-[12px] text-ink-2 sm:block"
          title={state.motion || config.motion}
        >
          <Decode
            text={state.motion || config.motion || "no motion set"}
            animate={state.motion.length > 0}
          />
        </div>
        <div className="ml-auto flex flex-wrap items-center justify-end gap-2">
          <div className="mr-1 hidden items-center gap-2 rounded-pill border border-line bg-surface-2 px-2.5 py-1 text-[12px] text-ink-2 tabular-nums min-[480px]:flex">
            {state.running ? (
              <>
                <span className="size-1.5 shrink-0 rounded-full bg-ok live-dot" />
                <span className="shrink-0">
                  round {state.round || 1}/{state.maxRounds || config.maxRounds}
                  {state.extraBudget > 0 && (
                    <span
                      className="text-ink-3"
                      title={`${state.extraBudget} of ${state.extraTotal} extra rounds still available — the Summarizer can ask the council for these`}
                    >
                      {" "}+{state.extraBudget}
                    </span>
                  )}
                </span>
                <RoundProgress
                  round={state.round}
                  running={state.running}
                  verdictRounds={state.verdict?.rounds ?? null}
                  total={state.maxRounds || config.maxRounds}
                />
              </>
            ) : state.verdict ? (
              <>
                <span className="size-1.5 rounded-full bg-line-2" />
                <span className="shrink-0 text-ink-3" title={endTitle(state)}>
                  {endLabel(state)}
                </span>
                <span className="shrink-0 tabular-nums">
                  R{state.verdict.rounds}/{state.maxRounds || config.maxRounds}
                </span>
              </>
            ) : (
              <>
                <span className="size-1.5 rounded-full bg-line-2" />
                <span className="text-ink-3">not started</span>
              </>
            )}
          </div>
          <span className="hidden min-[480px]:inline">
            <SessionClock
              startedAt={state.startedAt}
              endedAt={state.endedAt}
              running={state.running}
            />
          </span>
          <button
            type="button"
            className={`btn btn-ghost !w-8 !px-0 ${panels.left ? "!bg-surface-3 !text-ink" : ""}`}
            onClick={() => setPanels((p) => ({ ...p, left: !p.left }))}
            title={panels.left ? "Hide the delegates and setup" : "Show the delegates and setup"}
            aria-label="toggle the delegates panel"
            aria-pressed={panels.left}
          >
            <Sidebar size={14} />
          </button>
          <button
            type="button"
            className={`btn btn-ghost !w-8 !px-0 ${panels.right ? "!bg-surface-3 !text-ink" : ""}`}
            onClick={() => setPanels((p) => ({ ...p, right: !p.right }))}
            title={panels.right ? "Hide the board and record" : "Show the board and record"}
            aria-label="toggle the board panel"
            aria-pressed={panels.right}
          >
            <span style={{ transform: "scaleX(-1)", display: "inline-flex" }}>
              <Sidebar size={14} />
            </span>
          </button>
          <button
            type="button"
            className="btn btn-ghost !w-8 !px-0"
            onClick={onLibrary}
            title="back to the library"
            aria-label="back to the library"
          >
            <BookOpen size={14} />
          </button>
          <ThemePicker value={theme} onChange={onTheme} />
          {state.running ? (
            <>
              <button
                type="button"
                className="btn btn-ghost"
                disabled={!!state.extension}
                onClick={skipTurn}
                title={
                  state.extension
                    ? "The Summarizer has asked for more rounds — answer the request above"
                    : operatorHasDraft
                      ? "Your typed turn will be discarded — skip the delegate instead"
                      : "Stop this delegate mid-turn, losing any vote it has not yet cast, and move to the next"
                }
              >
                <SkipForward size={14} />
                {/* Icon-only on a phone. Even wrapped, "Skip" beside "Stop" is two
                    more words competing for a 360px line with five other
                    controls; the icon and the tooltip carry it, and the button
                    keeps its 44px target. */}
                <span className="hidden sm:inline">Skip</span>
                <span className="sr-only sm:hidden">Skip this turn</span>
              </button>
              <button type="button" className="btn btn-danger" onClick={() => void council.stop()}>
                Stop
              </button>
            </>
          ) : (
            <button
              type="button"
              className="btn btn-primary"
              onClick={start}
              /* `aria-disabled` rather than `disabled`. A disabled button is not
                 focusable and its `title` is unreachable, so the explanation
                 only ever existed for a mouse hovering at the right moment — and
                 this app is explicitly served to a phone. The button stays
                 operable so it can be reached and pointed at the list below,
                 which is what pressing it does. */
              aria-disabled={problems.length > 0}
              aria-describedby={problems.length ? "convene-problems" : undefined}
              title={problems.length ? "see what is missing below" : "Start the debate with these settings"}
            >
              Convene
            </button>
          )}
        </div>
      </header>
      {/* The validation list, on screen.
          `problems` was a careful eleven-branch check whose entire output reached
          the operator as a tooltip on a disabled button: invisible to touch, to
          keyboard, and to a screen reader, and gone the moment the pointer moved.
          Every message already says what is missing and where to fix it, so
          showing them costs nothing but the space they occupy. */}
      {!state.running && problems.length > 0 && (
        <div
          id="convene-problems"
          className="shrink-0 border-b border-line bg-surface-2 px-4 py-2 text-[12px] leading-relaxed text-ink-2 sm:px-6"
        >
          <div className="font-medium text-ink">
            Before convening — {problems.length} thing{problems.length === 1 ? "" : "s"} to fix:
          </div>
          <ul className="mt-1 flex list-disc flex-col gap-0.5 pl-4">
            {problems.map((p) => (
              <li key={p}>{p}</li>
            ))}
          </ul>
        </div>
      )}
      {!inTauri() && served && (
        <div className="shrink-0 border-b border-line bg-primary-soft px-4 py-1.5 text-[12px] text-ink-2">
          Local server — the council below runs real models on this machine. Keys stay on the
          server; the browser sends setup profiles only.
        </div>
      )}
      {!inTauri() && !served && (
        <div className="shrink-0 border-b border-line bg-primary-soft px-4 py-1.5 text-[12px] text-ink-2">
          Browser preview — the council below is scripted, no models are contacted. Run{" "}
          <span className="rounded bg-surface-2 px-1.5 py-0.5 font-mono text-[11px]">
            npm run tauri dev
          </span>{" "}
          for the desktop app, or{" "}
          <span className="rounded bg-surface-2 px-1.5 py-0.5 font-mono text-[11px]">
            npm run serve
          </span>{" "}
          for real debates in this browser.
        </div>
      )}
      {resumeOffer && (
        <div className="shrink-0 border-b border-primary/40 bg-primary-soft px-4 py-2 text-[12px] leading-relaxed text-ink-2" role="status">
          Resume <strong className="font-semibold text-ink">“{resumeOffer.motion}”</strong>{" "}
          ({resumeOffer.roundsSpoken} round{resumeOffer.roundsSpoken === 1 ? "" : "s"} spoken)?
          Applying replaces the motion and roster above.
          <span className="ml-3 inline-flex gap-2">
            <button type="button" className="btn btn-primary !py-0.5 text-[11.5px]" onClick={() => applyResume(resumeOffer)}>
              resume this debate
            </button>
            <button type="button" className="btn btn-ghost !py-0.5 text-[11.5px]" onClick={discardResume}>
              keep my setup
            </button>
          </span>
        </div>
      )}
      {resumeNotice && (
        <div className="shrink-0 border-b border-line bg-primary-soft px-4 py-2 text-[12px] leading-relaxed text-ink-2">
          {resumeNotice}
        </div>
      )}
      {liveNotices.length > 3 && (
        <div className="shrink-0 border-b border-line bg-surface px-4 py-1">
          <button
            type="button"
            className="text-[11px] text-ink-3 hover:text-ink-2 hover:underline"
            onClick={() => setShowAllNotices((v) => !v)}
          >
            {showAllNotices ? "hide" : `show all ${liveNotices.length}`} notices
          </button>
        </div>
      )}
      {visibleNotices.length > 0 && (
        <div className="max-h-[30vh] shrink-0 overflow-y-auto stable-gutter" role="status" aria-live="polite">
          {visibleNotices.map((n) => (
            <div
              key={n.id}
              className={`rise flex items-center gap-2 border-b border-line bg-surface px-4 py-1.5 font-mono text-[11.5px] ${
                n.tone === "error"
                  ? "text-bad"
                  : n.tone === "warn"
                    ? "text-warn"
                    : "text-ink-2"
              }`}
            >
              {/* Tone is marked by shape as well as colour, so an error reads
                  as one without relying on hue — which is also how it stays
                  legible in the two monochrome themes. */}
              <span aria-hidden className="shrink-0">
                {n.tone === "error" ? (
                  <XCircle size={12} weight="fill" />
                ) : n.tone === "warn" ? (
                  <Warning size={12} weight="fill" />
                ) : (
                  <Info size={12} weight="fill" />
                )}
              </span>
              <span className="min-w-0 flex-1 break-words">{n.text}</span>
              <button
                type="button"
                className="shrink-0 rounded p-1 text-ink-3 hover:text-ink"
                onClick={() => dismiss(n.id)}
                title="dismiss"
                aria-label={`dismiss notice: ${n.text.slice(0, 80)}`}
              >
                <X size={12} />
              </button>
            </div>
          ))}
        </div>
      )}
      <main
        className="chamber min-h-0 flex-1"
        data-left={panels.left ? "1" : "0"}
        data-right={panels.right ? "1" : "0"}
      >
        {panels.left && (
          <aside className="chamber-left flex min-h-0 flex-col overflow-hidden border-r border-line bg-surface">
            <div className="max-h-[42%] shrink-0 overflow-y-auto border-b border-line stable-gutter">
              <CouncilRail state={state} />
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto stable-gutter">
              <SetupPanel
                config={config}
                onChange={setConfig}
                disabled={state.running}
                /* Stable, or `SetupPanel`'s memo never bails out. This closure was
                   a fresh function on every Chamber render, and Chamber re-renders on
                   every reducer dispatch — every streamed token. So ~60 controlled
                   inputs were diffed 25×/second for the whole debate, defeating the
                   memo whose own comment claimed it saved exactly that. */
                onReset={resetSetup}
              />
            </div>
          </aside>
        )}
        <section className="relative flex min-h-0 min-w-0 flex-col">
          {state.delegates.length > 0 &&
            (state.running || state.speeches.length > 0 || state.verdict) && (
              <div className="shrink-0 border-b border-line bg-surface px-5 py-1.5">
                <RunTrace view={traceView} />
              </div>
            )}
          {/* An open poll, outside the transcript scroller.
              The engine parks for votes, and the poll cards lived at the bottom
              of the scrolled transcript — so an operator reading an argument
              further up had no way to know the room was waiting on them. Nothing
              else in the app mentioned polls at all. This is the same treatment
              the adjourn request gets just below: it is a state the debate
              cannot continue without, so it gets a fixed place above the fold. */}
          {openPolls.length > 0 && (
            <div
              className="shrink-0 border-b border-line bg-primary-soft/50 px-5 py-1.5"
              role="status"
            >
              <div className="mx-auto flex max-w-[88ch] flex-wrap items-center gap-x-2 gap-y-1">
                <span className="text-[11px] font-semibold tracking-[0.1em] text-ink uppercase">
                  {openPolls.length === 1 ? "1 poll open" : `${openPolls.length} polls open`}
                </span>
                <button
                  type="button"
                  className="btn !py-0.5 text-[11px]"
                  onClick={() => {
                    setQuery("");
                    // Pin to the bottom, where the cards are, rather than
                    // jumping the scroll position — the operator may already be
                    // looking at it, and the alternative is a scroll fight.
                    requestAnimationFrame(() => {
                      const el = scrollRef.current;
                      if (el) el.scrollTop = el.scrollHeight;
                    });
                  }}
                  title="show the open polls at the end of the transcript"
                >
                  show
                </button>
                {openPolls.map((p) => (
                  <span key={p.id} className="text-[11px] text-ink-3">
                    {p.question.length > 60 ? `${p.question.slice(0, 60)}…` : p.question}
                  </span>
                ))}
              </div>
            </div>
          )}
          {state.adjourn && state.adjourn.passed === null && (
            <div className="rise shrink-0 border-b border-line bg-surface px-5 py-2.5">
                <div className="mx-auto flex max-w-[88ch] flex-wrap items-center gap-x-3 gap-y-1">
                <span className="size-2 shrink-0 rounded-full bg-warn live-dot" />
                <p className="min-w-0 flex-1 text-[12.5px] leading-relaxed text-ink-2">
                  <strong className="font-semibold text-ink">
                    {councilLabel(state, state.adjourn.by)} wants to end the debate early
                  </strong>{" "}
                  — “{state.adjourn.reason}”
                </p>
                <span
                  className="shrink-0 font-mono text-[11.5px] tabular-nums text-ink-2"
                  title={
                    state.adjourn.outstanding.length
                      ? `Still to vote: ${state.adjourn.outstanding.map((id) => councilLabel(state, id)).join(", ")}`
                      : "Every delegate has voted. Most votes win it."
                  }
                >
                  yes {state.adjourn.yes} · no {state.adjourn.no}
                  {state.adjourn.outstanding.length > 0 && (
                    <span className="text-ink-3">
                      {" "}· {state.adjourn.outstanding.length} still to vote (
                      {state.adjourn.outstanding.map((id) => councilLabel(state, id)).join(", ")})
                    </span>
                  )}
                </span>
              </div>
            </div>
          )}
          {/* The operator's own seat, or a ballot it owes. Only rendered where
              one can exist: the automatic council never marks a seat taken, and
              the scripted preview has nobody to answer. */}
          {state.pending && (
            <Suspense fallback={<ChunkFallback />}>
              <OperatorPanel
                prompt={state.pending}
                points={state.points}
                labelOf={council.labelOf}
                submitting={council.submitting}
                stalled={council.submitStalled}
                onSubmitTurn={submitTurn}
                onSubmitBallot={submitBallot}
                onSkip={skipTurn}
                onDraftChange={setOperatorHasDraft}
              />
            </Suspense>
          )}
          {state.extension && (
            <ExtensionModal
              rounds={state.extension.rounds}
              reason={state.extension.reason}
              round={state.extension.round}
              budget={state.extraBudget}
              ballots={state.extension.ballots}
              labelOf={council.labelOf}
              onResolve={resolveExtension}
            />
          )}
          <div className="flex shrink-0 items-center gap-2 border-b border-line bg-surface px-5 py-1.5">
            <TranscriptSearch value={query} onChange={setQuery} />
          </div>
          <div
            ref={scrollRef}
            className="min-h-0 flex-1 overflow-y-auto stable-gutter px-5 py-4"
            onScroll={(e) => {
              const el = e.currentTarget;
              setPin(el.scrollHeight - el.scrollTop - el.clientHeight < 80);
            }}
          >
            <Transcript
              speeches={state.speeches}
              live={state.live}
              colorOf={council.colorOf}
              labelOf={council.labelOf}
              glyphOf={council.glyphOf}
              slotOf={council.slotOf}
              polls={state.polls}
              summarizerId={summarizerId}
              reads={state.reads}
              query={deferredQuery}
              streaming={streamingSpeaker}
            />
          </div>
          {!pinned && (state.speeches.length > 0 || state.live) && (
            <button
              type="button"
              className="btn absolute bottom-20 left-1/2 -translate-x-1/2 !border-primary/50 !bg-surface"
              style={{ boxShadow: "var(--shadow-pop)" }}
              onClick={jumpToLatest}
              title="follow the live transcript again"
            >
              <ArrowDown size={13} /> latest
            </button>
          )}
          {/* The composer. `say` reports whether the note landed, and the box is
              cleared only then: the old code cleared it before the request
              resolved, so a note typed with no debate running vanished and the
              operator's next turn argued against a point no model had seen. */}
          <div className="safe-bottom safe-x flex shrink-0 flex-wrap items-start gap-2 border-t border-line bg-surface px-4 py-3 sm:px-6">
            {/* A textarea, not an input: a point injected mid-argument is
                routinely two or three sentences, and an input silently collapses
                the line breaks that carry the reasoning. Enter still sends;
                Shift+Enter is the newline. */}
            <textarea
              ref={noteRef}
              rows={1}
              className="field min-w-0 flex-1 resize-none leading-relaxed"
              value={note}
              placeholder={
                state.running
                  ? "Say something to the council — every delegate sees this on its next turn"
                  : "Press Convene to start a council, then anything you type here reaches every delegate"
              }
              aria-label="Say something to the council"
              onChange={(e) => {
                setNote(e.target.value);
                const el = e.target;
                el.style.height = "auto";
                el.style.height = `${el.scrollHeight}px`;
              }}
              onKeyDown={(e) => {
                // Never send mid-IME-composition: Enter is what commits a
                // candidate in Japanese, Chinese and Korean input, and sending
                // there submitted a half-composed string and cleared the box.
                if (e.nativeEvent.isComposing) return;
                if (e.key === "Enter" && !e.shiftKey && note.trim()) {
                  e.preventDefault();
                  void sendNote();
                }
              }}
            />
            <button
              type="button"
              className="btn mt-px h-[34px]"
              disabled={!note.trim() || !state.running || sendingNote}
              title={
                state.running
                  ? "Send this to every delegate (Enter)"
                  : "No debate is running — press Convene first"
              }
              onClick={() => void sendNote()}
            >
              {sendingNote ? <span className="spinner" /> : <PaperPlaneTilt size={14} />}
              Send
            </button>
          </div>
        </section>
        {panels.right && (
          <aside className="chamber-right flex min-h-0 flex-col overflow-hidden border-l border-line bg-surface">
<TabStrip
              label="board and record"
              value={tab}
              onChange={setTab}
              tabs={[
                { id: "board", label: "Board", badge: state.points.length },
                { id: "record", label: "Record" },
              ]}
            />
            <TabPanel id={tab}>
              {tab === "board" ? (
                <BoardPanel
                  points={state.points}
                  delegates={state.delegates}
                  tallyOf={tallyOf}
                  labelOf={council.labelOf}
                  slotOf={council.slotOf}
                  voteLog={state.voteLog}
                />
              ) : (
                <RecordPanel state={state} />
              )}
            </TabPanel>
          </aside>
        )}
      </main>
    </div>
  );
}
// ---------------------------------------------------------------------------
// routing
// ---------------------------------------------------------------------------
type Route =
  | { view: "home" | "new" | "human" | "read"; id: string | null };
function parseHash(): Route {
  const h = window.location.hash.replace(/^#\/?/, "");
  if (h.startsWith("d/") && h.length > 2) return { view: "read", id: h.slice(2) };
  if (h === "new") return { view: "new", id: null };
  if (h === "human") return { view: "human", id: null };
  return { view: "home", id: null };
}
/**
 * Four places to be: the library, an automatic council, a council you argue in
 * yourself, and a debate read.
 *
 * Hash routing rather than a router library, because the whole navigation is
 * four strings. Hash rather than history so a reload — the one thing a debate
 * app cannot avoid, since the engine keeps running while the window does not —
 * lands back where you were.
 *
 * The human council is a route rather than a setting because it is a different
 * kind of debate: one where the operator holds a seat and the engine waits for
 * them. Folding it into the automatic council would mean a toggle that changes
 * what every other control means.
 */
/** What each view is called, for the title bar and for the skip link. */
const VIEW_TITLE: Record<Route["view"], string> = {
  home: "Library",
  new: "Council",
  human: "Your council",
  read: "Reading a debate",
};
function useRoute(): Route {
  const [route, setRoute] = useState<Route>(parseHash);
  useEffect(() => {
    const on = () => setRoute(parseHash());
    window.addEventListener("hashchange", on);
    return () => window.removeEventListener("hashchange", on);
  }, []);
  return route;
}
/**
 * A heading for the new view, focused on arrival.
 *
 * Routing between views unmounts whatever was focused, and focus fell to
 * `<body>`: nothing was announced, and the next Tab started at the top of the
 * page — so leaving the library by pressing "read" put a keyboard user back at
 * the wordmark, having to Tab through the entire header to reach the debate.
 *
 * The target is a real heading with `tabIndex={-1}`, not a wrapper: screen
 * readers announce a focused heading as the new page's title, which is the
 * conventional way to say "you have arrived somewhere else".
 */
function RouteHeading({ route }: { route: Route }) {
  const ref = useRef<HTMLHeadingElement | null>(null);
  const first = useRef(true);
  useEffect(() => {
    // Only on a change of view, never on the initial mount: focusing something
    // on first paint moves focus away from wherever the browser put it, which
    // for a keyboard user means the page opens with focus stolen.
    if (first.current) {
      first.current = false;
      return;
    }
    ref.current?.focus();
  }, [route.view, route.id]);
  const label = VIEW_TITLE[route.view];
  return (
    <h1
      ref={ref}
      tabIndex={-1}
      className="sr-only focus-visible:not-sr-only focus-visible:absolute focus-visible:top-1 focus-visible:left-1 focus-visible:z-60 focus-visible:rounded focus-visible:bg-surface focus-visible:px-3 focus-visible:py-1.5 focus-visible:text-[13px] focus-visible:font-semibold focus-visible:text-ink"
    >
      {label}
    </h1>
  );
}
function go(hash: string) {
  if (window.location.hash === hash) return;
  window.location.hash = hash;
}
export default function App() {
  const [theme, setTheme] = useState<ThemeId>(() => {
    try {
      const saved = window.localStorage.getItem(THEME_KEY);
      // The old two-value toggle stored "dark"/"light"; map them onto the
      // themes that kept those exact colour sets, so nobody loses their choice
      // to the rename.
      if (saved === "dark") return "sage-dark";
      if (saved === "light") return "sage-light";
      return isThemeId(saved) ? saved : DEFAULT_THEME;
    } catch {
      return DEFAULT_THEME;
    }
  });
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    // Native scrollbars, the caret and select popups follow this, not our
    // tokens: without it a "dark" theme still renders a light scrollbar.
    document.documentElement.style.colorScheme = themeMeta(theme).tone;
    try {
      window.localStorage.setItem(THEME_KEY, theme);
    } catch {
      // A full or blocked store costs us the preference, not the app.
    }
  }, [theme]);
  const route = useRoute();
  /* The window title, so a tab is identifiable among several and the browser's
     history and bookmarks say what they are. It read "Council" everywhere,
     including in the library. */
  useEffect(() => {
    document.title =
      route.view === "home" ? "Council — library" : `Council — ${VIEW_TITLE[route.view]}`;
  }, [route.view, route.id]);
  if (route.view === "read" && route.id) {
    return (
      <>
        <RouteHeading route={route} />
        <Suspense fallback={<ChunkFallback />}>
          <ReaderView
            id={route.id}
            onBack={() => go("#/")}
            // The draft is already saved by the button; this only navigates.
            onResume={() => go("#/new")}
          />
        </Suspense>
      </>
    );
  }
  if (route.view === "new" || route.view === "human") {
    const human = route.view === "human";
    // Keyed by view: both routes rendered the same `Chamber` at the same
    // position, so React reconciled instead of remounting. The `useState`
    // initializers did not re-run, `useCouncil` was not recreated, and the
    // autosave then wrote the *automatic* council's roster into
    // `council.config.human.v1`. Reachable by editing the hash, or by back and
    // forward once both hashes are in history — and the comment above the route
    // promises the two setups are left entirely alone.
    return (
      <Chamber
        key={route.view}
        theme={theme}
        onTheme={setTheme}
        onLibrary={() => go("#/")}
        human={human}
      />
    );
  }
  return (
    <>
      <RouteHeading route={route} />
      <HomeView
        theme={theme}
        onTheme={setTheme}
        onConvene={() => go("#/new")}
        onOpen={(id) => go(`#/d/${id}`)}
        onReturn={() => go("#/new")}
        onTakeSeat={() => go("#/human")}
      />
    </>
  );
}
