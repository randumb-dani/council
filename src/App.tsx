import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  ArrowDown,
  BookOpen,
  Moon,
  PaperPlaneTilt,
  Sidebar,
  Scales,
  SkipForward,
  Sun,
  X,
} from "@phosphor-icons/react";
import { CouncilRail } from "./components/CouncilRail";
import {
  BoardPanel,
  fmtElapsed,
  MAX_ROUND_TICKS,
  RunTrace,
  Transcript,
  TranscriptSearch,
} from "./components/Panels";
import { SetupPanel, freshEndpoint } from "./components/SetupPanel";
import { RecordPanel, councilLabel, endLabel, endTitle } from "./components/RecordPanel";
import { HomeView } from "./components/HomeView";
import { ReaderView } from "./components/ReaderView";
import { newDebateId, saveDebate } from "./debates";
import { inTauri } from "./bridge";
import {
  type DebateConfig,
  type DecisionConfig,
  type DelegateConfig,
  type ImageAttachment,
  type SeatRole,
} from "./types";
import { useCouncil, type CouncilState } from "./useCouncil";

const STORAGE_KEY = "council.config.v1";
/** Kept in step with the engine's own bound on rounds. */
const MAX_ROUNDS = 100;
const SEAT_ROLES: readonly SeatRole[] = [
  "proponent",
  "opponent",
  "judge",
  "rationale",
  "summarizer",
  "custom",
];
const THEME_KEY = "council.theme";
const PANELS_KEY = "council.panels.v1";

function defaultConfig(): DebateConfig {
  const ep = freshEndpoint(0);
  ep.name = "Unsloth";
  return {
    motion: "",
    extraInstructions: null,
    endpoints: [ep],
    delegates: [
      {
        id: "seat-1",
        label: "Advocate",
        model: "",
        endpointId: ep.id,
        color: "#65ccc6",
        seatRole: "proponent",
        role: "makes the case",
        persona: null,
        temperature: 0.7,
      },
      {
        id: "seat-2",
        label: "Critic",
        model: "",
        endpointId: ep.id,
        color: "#4aa8a1",
        seatRole: "opponent",
        role: "steelmans the other side, then attacks it",
        persona: null,
        temperature: 0.7,
      },
      {
        id: "seat-3",
        label: "Arbiter",
        model: "",
        endpointId: ep.id,
        color: "#bffffa",
        seatRole: "judge",
        role: "votes only on the record",
        persona: null,
        temperature: 0.3,
      },
      {
        id: "seat-4",
        label: "Rationale",
        model: "",
        endpointId: ep.id,
        color: "#c9b8f2",
        seatRole: "rationale",
        role: "finds logical fallacies",
        persona: null,
        temperature: 0.5,
      },
      {
        id: "seat-5",
        label: "Summarizer",
        model: "",
        endpointId: ep.id,
        color: "#8ad9d3",
        seatRole: "summarizer",
        role: "lays out each round and the final result; never argues",
        persona: null,
        temperature: 0.6,
      },
    ],
    maxRounds: 4,
    autoAdjourn: false,
    closingStatements: true,
    decision: {
      provider: "seat",
      baseUrl: "",
      apiKey: null,
      model: "",
      timeoutSecs: 30,
    },
    maxExtraRounds: 4,
    allowWeb: false,
    maxFetchesPerTurn: 3,
    maxFetchChars: 6000,
    images: [],
  };
}

/**
 * The shape of a seat. Fresh seats carry it; stored ones predate it.
 * Grandfathered labels are kept — only the role is fixed, never renamed.
 */
const ESSENTIAL_ROLES: {
  role: SeatRole;
  label: string;
  color: string;
  temperature: number;
  /** Pre-filled "role in the council" line, so a repaired seat reads complete. */
  roleLine?: string;
}[] = [
  { role: "proponent", label: "Advocate", color: "#65ccc6", temperature: 0.7 },
  { role: "opponent", label: "Critic", color: "#4aa8a1", temperature: 0.7 },
  { role: "judge", label: "Arbiter", color: "#bffffa", temperature: 0.3 },
  {
    role: "rationale",
    label: "Rationale",
    color: "#c9b8f2",
    temperature: 0.5,
    roleLine: "finds logical fallacies",
  },
  { role: "summarizer", label: "Summarizer", color: "#8ad9d3", temperature: 0.6 },
];

function ensureEssentials(
  delegates: (Omit<DelegateConfig, "seatRole"> & { seatRole?: SeatRole })[],
  endpoints: DebateConfig["endpoints"],
): DelegateConfig[] {
  const fallbackEp = endpoints[0]?.id ?? "";
  const fallbackModel = delegates[0]?.model ?? "";
  const out: DelegateConfig[] = delegates.map((d) => ({
    ...d,
    seatRole: d.seatRole ?? ("custom" as SeatRole),
  }));
  // Legacy seats predate roles entirely: the first three keep their models
  // and endpoints and take the arguing roles in order.
  // Per-seat, not all-or-nothing: a half-migrated roster used to leave seats
  // with no role at all, which the UI then showed as essential (no delete
  // button) while the engine convened them as custom. Two panels, two truths.
  // Note: callers must leave unknown roles as undefined (not "custom") for
  // this migration to run.
  const order: SeatRole[] = ["proponent", "opponent", "judge"];
  // Re-open seats that were previously forced to "custom" by a coercion bug:
  // the first three customs in a 7-seat roster are the legacy essentials.
  const looksLegacy =
    out.filter((d) => d.seatRole === "custom").length >= 3 &&
    out.length > 4 &&
    (delegates as { seatRole?: unknown }[]).every(
      (d) => d.seatRole === undefined || d.seatRole === "custom",
    );
  if (looksLegacy) {
    let n = 0;
    for (const d of out) {
      if (d.seatRole === "custom" && n < 3) {
        d.seatRole = order[n++]!;
      }
    }
  }
  out.forEach((d, i) => {
    if ((delegates[i]?.seatRole as SeatRole | undefined) !== undefined) return;
    const want = order[i];
    d.seatRole = want && !out.some((o) => o.seatRole === want) ? want : "custom";
  });
  // A custom seat already doing the Rationale's job is promoted in place
  // rather than appended beside: the engine refuses duplicate labels, so an
  // append would break a setup that already has one under that name.
  if (!out.some((d) => d.seatRole === "rationale")) {
    const heir = out.find(
      (d) => d.seatRole === "custom" && d.label.trim().toLowerCase() === "rationale",
    );
    if (heir) {
      heir.seatRole = "rationale";
      if (!heir.role?.trim()) heir.role = "finds logical fallacies";
    }
  }
  // Whatever is still missing is appended with the first seat's model, so an
  // old setup convenes without the user repairing anything. That includes the
  // Rationale: existing councils gain it as a fifth speaker on next load.
  for (const need of ESSENTIAL_ROLES) {
    if (!out.some((d) => d.seatRole === need.role)) {
      out.push({
        id: `seat-${need.role}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`,
        label: need.label,
        model: fallbackModel,
        endpointId: fallbackEp,
        color: need.color,
        seatRole: need.role,
        role: need.roleLine ?? null,
        persona: null,
        temperature: need.temperature,
      });
    }
  }
  // A duplicated essential beyond the first argues as a custom seat instead
  // of refusing to convene.
  const seen = new Set<SeatRole>();
  for (const d of out) {
    if (d.seatRole === "custom" || !seen.has(d.seatRole)) {
      seen.add(d.seatRole);
    } else {
      d.seatRole = "custom";
    }
  }
  return out;
}

/**
 * Stored config can go stale in a way the user cannot see or fix: an endpoint
 * deleted in an earlier session leaves its delegates pointing at an id that no
 * longer exists, and the select then shows the first row while the value is
 * still the ghost. Repair it on load rather than nagging about it forever.
 */
/**
 * Coercion for everything read back out of localStorage.
 *
 * Storage is untrusted input: a hand-edited `"temperature": "0.7"` made the
 * setup panel throw during render and blanked the whole window, and an
 * out-of-range round count was rejected by the engine on every attempt to
 * start — with no way back to a working setup.
 */
function str(v: unknown, fallback = ""): string {
  return typeof v === "string" ? v : fallback;
}

function bool(v: unknown, fallback: boolean): boolean {
  return typeof v === "boolean" ? v : fallback;
}

function num(v: unknown, fallback: number, min: number, max: number): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) return fallback;
  return Math.min(max, Math.max(min, Math.round(n)));
}

function oneOf<T extends string>(v: unknown, allowed: readonly T[], fallback: T): T {
  return typeof v === "string" && (allowed as readonly string[]).includes(v)
    ? (v as T)
    : fallback;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/** At most this many images ride on the motion. */
const MAX_IMAGES = 6;
/** Per-image cap after downscaling (~1.9MB of bytes). */
const MAX_IMAGE_CHARS = 2_500_000;

/**
 * Image bytes dwarf everything else in storage, so only sane ones survive a
 * reload: real data URLs, each under the cap, at most MAX_IMAGES. Anything
 * else is dropped rather than allowed to brick the saved setup.
 */
function sanitizeImages(v: unknown): ImageAttachment[] {
  if (!Array.isArray(v)) return [];
  const out: ImageAttachment[] = [];
  let total = 0;
  for (const e of v) {
    if (!isRecord(e)) continue;
    const dataUrl = typeof e.dataUrl === "string" ? e.dataUrl : "";
    if (!/^data:image\/[a-zA-Z0-9.+-]+;base64,/.test(dataUrl)) continue;
    if (dataUrl.length > MAX_IMAGE_CHARS || total + dataUrl.length > MAX_IMAGE_CHARS * 4) {
      continue;
    }
    total += dataUrl.length;
    const media = /^data:(image\/[a-zA-Z0-9.+-]+);base64,/.exec(dataUrl)?.[1] ?? "image/jpeg";
    out.push({
      id: str(e.id, `img-${out.length}`),
      name: str(e.name, "image").slice(0, 80) || "image",
      mediaType: media,
      dataUrl,
    });
    if (out.length >= MAX_IMAGES) break;
  }
  return out;
}

function loadConfig(): DebateConfig {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return defaultConfig();
    const parsed = JSON.parse(raw) as unknown;
    if (!isRecord(parsed)) return defaultConfig();
    const base = defaultConfig();

    const endpoints = Array.isArray(parsed.endpoints)
      ? parsed.endpoints.filter(isRecord).map((e, i) => ({
          id: str(e.id, `ep-${i}`),
          name: str(e.name, `Endpoint ${i + 1}`),
          baseUrl: str(e.baseUrl),
          apiKey: typeof e.apiKey === "string" ? e.apiKey : null,
          headers: isRecord(e.headers)
            ? Object.fromEntries(
                Object.entries(e.headers).filter(
                  (pair): pair is [string, string] => typeof pair[1] === "string",
                ),
              )
            : {},
          timeoutSecs: num(e.timeoutSecs, 300, 10, 3600),
          // Reasoning effort is opt-in per endpoint: the shapes are not
          // interchangeable, and a model left to its own default may pick a level
          // that is slow, or that fails outright.
          reasoningWire: oneOf(
            e.reasoningWire,
            ["serverDefault", "flat", "chatTemplate"],
            "serverDefault",
          ),
          reasoningEffort: str(e.reasoningEffort).trim() || null,
          thinking: typeof e.thinking === "boolean" ? e.thinking : null,
          nativeTools: bool(e.nativeTools, true),
          stream: bool(e.stream, true),
          allowInvalidCerts: bool(e.allowInvalidCerts, false),
          includeUsage: bool(e.includeUsage, true),
        }))
      : base.endpoints;
    if (!endpoints.length) return base;

    const stored = Array.isArray(parsed.delegates)
      ? parsed.delegates.filter(isRecord).map((d, i) => ({
          id: str(d.id, `seat-${i}`),
          label: str(d.label, `Delegate ${i + 1}`),
          model: str(d.model),
          endpointId: str(d.endpointId),
          color: str(d.color, "#65ccc6"),
          // Leave unknown/missing roles empty so ensureEssentials can migrate
          // legacy rosters by position. Coercing straight to "custom" made the
          // `if (d.seatRole) return` early-out always true and turned every
          // old 3-seat setup into 3 customs + 4 appended essentials.
          seatRole: (
            typeof d.seatRole === "string" &&
            (SEAT_ROLES as readonly string[]).includes(d.seatRole)
              ? (d.seatRole as SeatRole)
              : undefined
          ) as SeatRole | undefined,
          role: typeof d.role === "string" ? d.role : null,
          persona: typeof d.persona === "string" ? d.persona : null,
          temperature:
            typeof d.temperature === "number" && Number.isFinite(d.temperature)
              ? Math.min(2, Math.max(0, d.temperature))
              : 0.7,
        }))
      : base.delegates;

    const known = new Set(endpoints.map((e) => e.id));
    const fallbackEp = endpoints[0]?.id ?? "";
    const delegates = ensureEssentials(
      stored.map((d) => (known.has(d.endpointId) ? d : { ...d, endpointId: fallbackEp })),
      endpoints,
    );

    const rawDecision = isRecord(parsed.decision) ? parsed.decision : {};
    const decision: DecisionConfig = {
      provider: oneOf(rawDecision.provider, ["seat", "jev"] as const, "seat"),
      baseUrl: str(rawDecision.baseUrl),
      apiKey: typeof rawDecision.apiKey === "string" ? rawDecision.apiKey : null,
      model: str(rawDecision.model),
      timeoutSecs: num(rawDecision.timeoutSecs, 30, 5, 300),
    };

    return {
      motion: str(parsed.motion),
      extraInstructions:
        typeof parsed.extraInstructions === "string" ? parsed.extraInstructions : null,
      endpoints,
      delegates,
      maxRounds: num(parsed.maxRounds, base.maxRounds, 1, MAX_ROUNDS),
      autoAdjourn: bool(parsed.autoAdjourn, false),
      closingStatements: bool(parsed.closingStatements, true),
      decision,
      maxExtraRounds: num(parsed.maxExtraRounds, base.maxExtraRounds, 0, MAX_ROUNDS),
      // Web access stays off for a config that predates it, exactly as it does
      // in the engine: reaching off the machine is opt-in, never inherited.
      allowWeb: parsed.allowWeb === true,
      maxFetchesPerTurn: num(parsed.maxFetchesPerTurn, 3, 1, 8),
      maxFetchChars: num(parsed.maxFetchChars, 6000, 500, 40000),
      // Image bytes dwarf everything else in storage: keep only sane ones.
      images: sanitizeImages(parsed.images),
    };
  } catch {
    return defaultConfig();
  }
}

/** Forget the saved setup and start over from defaults. */
function resetConfig() {
  try {
    window.localStorage.removeItem(STORAGE_KEY);
  } catch {
    /* storage unavailable; the in-memory reset below still applies */
  }
  window.location.reload();
}

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
      title={`${running ? "running" : "total"} time, from the first turn`}
    >
      {fmtElapsed(secs)}
    </span>
  );
}

function ExtensionModal({
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
  useEffect(() => {
    setCount(rounds);
    setBusy(false);
  }, [rounds, round]);
  // Focus the dialog on open so keyboard users land on the decision.
  // No focus trap library here: keep it simple, focus once, Esc declines.
  useEffect(() => {
    dialogRef.current?.querySelector<HTMLButtonElement>("button")?.focus();
  }, []);
  // A second click would hit an engine that has already taken the proposal and
  // raise a red error for something the operator did correctly.
  const answer = (approved: boolean, n: number) => {
    if (busy) return;
    setBusy(true);
    try {
      const r = onResolve(approved, n) as unknown;
      // resolve() swallows engine errors into a notice, so the modal would
      // stay busy forever on failure. Release on next tick unless unmounted.
      if (r && typeof (r as Promise<unknown>).finally === "function") {
        (r as Promise<unknown>).finally(() => setBusy(false));
      } else {
        window.setTimeout(() => setBusy(false), 1500);
      }
    } catch {
      setBusy(false);
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
        if (e.key === "Escape" && !busy) answer(false, 0);
      }}
    >
      <div ref={dialogRef} className="card rise w-full max-w-md p-5">
        <div className="flex items-center gap-2">
          <span className="size-2 shrink-0 rounded-full bg-warn live-dot" />
          <h2 id="ext-title" className="text-[14px] font-semibold tracking-tight text-ink">
            More rounds?
          </h2>
          <span className="ml-auto text-[11px] text-ink-3 tabular-nums">after R{round}</span>
        </div>

        <p className="mt-2.5 text-[12.5px] leading-relaxed text-ink-2">
          <strong className="font-semibold text-ink">Summarizer</strong> says:
        </p>
        <p id="ext-reason" className="mt-1 border-l-2 border-line-2 pl-2.5 text-[12.5px] leading-relaxed text-ink-2">
          {reason}
        </p>

        {/* The count building, so the room is visibly deciding. */}
        <div className="mt-3.5 border-t border-line pt-3">
          <div className="flex items-baseline justify-between text-[11.5px]">
            <span className="text-ink-3">the council says</span>
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
                  <span className="text-ink-3">— did not answer</span>
                )}
              </li>
            ))}
            {ballots.length === 0 && (
              <li className="text-[11.5px] text-ink-3">asking the room…</li>
            )}
          </ul>
        </div>

        <p className="mt-3 text-[11.5px] leading-relaxed text-ink-3">
          Leave this alone and the council decides — a majority carries it. You
          can still settle it yourself below.
        </p>

        <div className="mt-3 flex items-center gap-2 border-t border-line pt-3">
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
            {budget} left{budget <= 0 ? " — the council decides" : ""}
          </span>
          <div className="ml-auto flex gap-2">
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy}
              aria-busy={busy}
              onClick={() => answer(false, 0)}
            >
              No
            </button>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || budget <= 0}
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
}

/**
 * Round progress as segments: filled behind, pulsing ahead-of-the-speaker at
 * the current round, hollow for the road left. The top bar is the one place
 * the whole debate's pace is always visible, so it lives here and not only in
 * the trace strip.
 */
function RoundProgress({ state, total }: { state: CouncilState; total: number }) {
  const n = Math.max(1, Math.min(total, MAX_ROUND_TICKS));
  const rounds = Array.from({ length: n }, (_, i) => i + 1);
  return (
    <span className="flex max-w-36 items-center gap-[3px]" title={`${total} rounds`}>
      {rounds.map((r) => {
        const done =
          r < state.round ||
          (!!state.verdict && r <= state.verdict.rounds);
        const current = r === state.round && state.running;
        return (
          <i
            key={r}
            className={`h-1 min-w-1 flex-1 rounded-full ${
              done ? "bg-primary" : current ? "bg-primary live-dot" : "bg-line-2"
            }`}
          />
        );
      })}
    </span>
  );
}

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
  }, [text]);
  return <>{shown}</>;
}

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
  onToggleTheme,
  onLibrary,
}: {
  theme: "dark" | "light";
  onToggleTheme: () => void;
  onLibrary: () => void;
}) {
  const [config, setConfig] = useState<DebateConfig>(loadConfig);
  const [panels, setPanels] = useState(loadPanels);
  const [note, setNote] = useState("");
  const [query, setQuery] = useState("");
  const [tab, setTab] = useState<"board" | "record">("board");
  const [hidden, setHidden] = useState<Set<number>>(new Set());
  const [showAllNotices, setShowAllNotices] = useState(false);
  const council = useCouncil();
  const { state } = council;
  const scrollRef = useRef<HTMLDivElement | null>(null);
  // pinnedRef drives the scroll math (no re-render per token); `pinned` state
  // only decides whether the jump-to-latest button shows.
  const pinnedRef = useRef(true);
  const [pinned, setPinned] = useState(true);
  const setPin = (v: boolean) => {
    pinnedRef.current = v;
    setPinned(v);
  };

  useEffect(() => {
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(config));
    } catch {
      /* quota or blocked store: keep the in-memory config, skip persisting */
    }
  }, [config]);

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
    if (!id || (!state.running && !state.verdict)) return;
    if (state.speeches.length === 0) return;
    // Debounced, because a vote on a point fires this too; immediate on the
    // verdict, because that is the moment someone is most likely to close the
    // window.
    const delay = state.verdict ? 0 : 1200;
    const t = window.setTimeout(() => {
      saveDebate(state, id);
      window.dispatchEvent(new Event("council:library"));
    }, delay);
    return () => window.clearTimeout(t);
  }, [
    state.running,
    state.verdict,
    state.speeches,
    state.points,
    state.tallies,
    state.reads,
    state.round,
  ]);

  // Direct scrollTop assignment: unlike scrollIntoView it cannot be captured
  // by an ancestor scroller, so a streaming transcript that is pinned stays
  // pinned, token after token. In a layout effect so the height is measured
  // after this turn's DOM but before paint — in a passive effect the pin
  // visibly lagged behind the text.
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (el && pinnedRef.current) el.scrollTop = el.scrollHeight;
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

  const problems = useMemo(() => {
    const out: string[] = [];
    if (!config.motion.trim()) out.push("no motion");
    if (config.delegates.length < 2) out.push("need at least two delegates");
    for (const need of ["proponent", "opponent", "judge", "summarizer"] as const) {
      if (!config.delegates.some((d) => d.seatRole === need)) out.push(`no ${need} seat`);
    }
    if (config.delegates.some((d) => !d.model.trim())) out.push("every delegate needs a model");
    if (config.endpoints.some((e) => !e.baseUrl.trim())) out.push("every endpoint needs a URL");
    const known = new Set(config.endpoints.map((e) => e.id));
    if (config.delegates.some((d) => !known.has(d.endpointId))) {
      out.push("a delegate points at a deleted endpoint");
    }
    return out;
  }, [config]);

  const start = () => {
    if (problems.length) {
      council.notice(`Cannot convene: ${problems.join("; ")}.`, "warn");
      return;
    }
    void council.start(config);
  };

  const liveNotices = state.notices.filter((n) => !hidden.has(n.id));
  // Was capped at three with no history, and a poll or review round trip emits
  // one notice per voter, so the actionable ones scrolled off unrecoverably.
  const visibleNotices = showAllNotices ? liveNotices : liveNotices.slice(-3);
  const dismiss = (id: number) => setHidden((h) => new Set(h).add(id));

  return (
    <div className="flex h-full flex-col">
      <header className="flex min-h-[52px] shrink-0 flex-nowrap items-center gap-x-4 border-b border-line bg-surface px-4 py-1">
        <div className="flex items-center gap-2 text-[11px] font-bold tracking-[0.16em] text-ink">
          <span className="flex size-5 items-center justify-center rounded-md bg-primary text-on-primary">
            <Scales size={13} weight="bold" />
          </span>
          COUNCIL
        </div>
        <div
          className="hidden min-w-0 flex-1 truncate border-l border-line pl-4 font-mono text-[12px] text-ink-2 sm:block"
          title={state.motion || config.motion}
        >
          <Decode
            text={state.motion || config.motion || "no motion set"}
            animate={state.motion.length > 0}
          />
        </div>
        <div className="ml-auto flex shrink-0 flex-nowrap items-center gap-2">
          <div className="mr-1 hidden items-center gap-2 rounded-pill border border-line bg-surface-2 px-2.5 py-1 text-[12px] text-ink-2 tabular-nums min-[480px]:flex">
            {state.running ? (
              <>
                <span className="size-1.5 shrink-0 rounded-full bg-ok live-dot" />
                <span className="shrink-0">
                  round {state.round || 1}/{state.maxRounds || config.maxRounds}
                  {state.extraBudget > 0 && (
                    <span
                      className="text-ink-3"
                      title={`${state.extraBudget} of ${state.extraTotal} extra rounds still available to the summarizer`}
                    >
                      {" "}+{state.extraBudget}
                    </span>
                  )}
                </span>
                <RoundProgress state={state} total={state.maxRounds || config.maxRounds} />
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
                <span className="text-ink-3">idle</span>
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
            title={panels.left ? "hide the council panel" : "show the council panel"}
            aria-label="toggle council panel"
            aria-pressed={panels.left}
          >
            <Sidebar size={14} />
          </button>
          <button
            type="button"
            className={`btn btn-ghost !w-8 !px-0 ${panels.right ? "!bg-surface-3 !text-ink" : ""}`}
            onClick={() => setPanels((p) => ({ ...p, right: !p.right }))}
            title={panels.right ? "hide the board panel" : "show the board panel"}
            aria-label="toggle board panel"
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
          <button
            type="button"
            className="btn btn-ghost !w-8 !px-0"
            onClick={onToggleTheme}
            title={theme === "dark" ? "switch to light" : "switch to dark"}
            aria-label="toggle theme"
          >
            {theme === "dark" ? <Moon size={14} /> : <Sun size={14} />}
          </button>
          {state.running ? (
            <>
              <button
                type="button"
                className="btn btn-ghost"
                disabled={!!state.extension}
                onClick={() => void council.skip()}
                title={
                  state.extension
                    ? "the council is waiting on your answer to the extension request"
                    : "abandon the current speaker's turn — ballots included — and move on"
                }
              >
                <SkipForward size={14} /> Skip
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
              title={problems.length ? `blocked: ${problems.join("; ")}` : "open the debate"}
            >
              Convene
            </button>
          )}
        </div>
      </header>

      {!inTauri() && (
        <div className="shrink-0 border-b border-line bg-primary-soft px-4 py-1.5 text-[12px] text-ink-2">
          Browser preview — the council below is scripted, no models are contacted. Run{" "}
          <span className="rounded bg-surface-2 px-1.5 py-0.5 font-mono text-[11px]">
            npm run tauri dev
          </span>{" "}
          for the real thing.
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
              <span aria-hidden className="shrink-0">
                {n.tone === "error" ? "✕" : n.tone === "warn" ? "!" : "·"}
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
                onReset={resetConfig}
              />
            </div>
          </aside>
        )}

        <section className="relative flex min-h-0 min-w-0 flex-col">
          {state.delegates.length > 0 &&
            (state.running || state.speeches.length > 0 || state.verdict) && (
              <div className="shrink-0 border-b border-line bg-surface px-5 py-1.5">
                <RunTrace state={state} />
              </div>
            )}
          {state.adjourn && state.adjourn.passed === null && (
            <div className="rise shrink-0 border-b border-line bg-surface px-5 py-2.5">
              <div className="mx-auto flex max-w-[80ch] flex-wrap items-center gap-x-3 gap-y-1">
                <span className="size-2 shrink-0 rounded-full bg-warn live-dot" />
                <p className="min-w-0 flex-1 text-[12.5px] leading-relaxed text-ink-2">
                  <strong className="font-semibold text-ink">
                    {councilLabel(state, state.adjourn.by)} moved to adjourn
                  </strong>{" "}
                  — “{state.adjourn.reason}”
                </p>
                <span
                  className="shrink-0 font-mono text-[11.5px] tabular-nums text-ink-2"
                  title={
                    state.adjourn.outstanding.length
                      ? `waiting on ${state.adjourn.outstanding.map((id) => councilLabel(state, id)).join(", ")}`
                      : "every seat has answered"
                  }
                >
                  yes {state.adjourn.yes} · no {state.adjourn.no}
                  {state.adjourn.outstanding.length > 0 && (
                    <span className="text-ink-3">
                      {" "}· {state.adjourn.outstanding.length} to answer (
                      {state.adjourn.outstanding.map((id) => councilLabel(state, id)).join(", ")})
                    </span>
                  )}
                </span>
              </div>
            </div>
          )}
          {state.extension && (
            <ExtensionModal
              rounds={state.extension.rounds}
              reason={state.extension.reason}
              round={state.extension.round}
              budget={state.extraBudget}
              ballots={state.extension.ballots}
              labelOf={council.labelOf}
              onResolve={(approved, n) => council.resolve(approved, n)}
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
              polls={state.polls}
              summarizerId={
                state.delegates.find((d) => d.role === "summarizer")?.id ?? null
              }
              reads={state.reads}
              query={query}
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
          <div className="flex shrink-0 flex-wrap gap-2 border-t border-line bg-surface px-4 py-3 sm:px-6">
            <input
              className="field min-w-0 flex-1"
              value={note}
              placeholder="Interject — every delegate sees this on their next turn"
              aria-label="Interject — every delegate sees this on their next turn"
              onChange={(e) => setNote(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && note.trim()) {
                  void council.say(note);
                  setNote("");
                }
              }}
            />
            <button
              type="button"
              className="btn"
              disabled={!note.trim()}
              onClick={() => {
                void council.say(note);
                setNote("");
              }}
            >
              <PaperPlaneTilt size={14} /> Send
            </button>
          </div>
        </section>

        {panels.right && (
          <aside className="chamber-right flex min-h-0 flex-col overflow-hidden border-l border-line bg-surface">
            <div
              className="sticky top-0 z-2 flex shrink-0 border-b border-line bg-surface"
              role="tablist"
              aria-label="debate side panel"
            >
            <button
              type="button"
              role="tab"
              aria-selected={tab === "board"}
              onClick={() => setTab("board")}
              className={`flex-1 border-b-2 px-3 py-3 text-[11px] font-semibold tracking-[0.1em] uppercase transition-colors ${
                tab === "board"
                  ? "border-primary text-ink"
                  : "border-transparent text-ink-3 hover:text-ink-2"
              }`}
            >
              Board
              {state.points.length > 0 && (
                <span className="ml-1.5 rounded-pill bg-surface-3 px-1.5 text-[10px] tracking-normal text-ink-2 tabular-nums">
                  {state.points.length}
                </span>
              )}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={tab === "record"}
              onClick={() => setTab("record")}
              className={`flex-1 border-b-2 px-3 py-3 text-[11px] font-semibold tracking-[0.1em] uppercase transition-colors ${
                tab === "record"
                  ? "border-primary text-ink"
                  : "border-transparent text-ink-3 hover:text-ink-2"
              }`}
            >
              Record
            </button>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto stable-gutter">
            {tab === "board" ? (
              <BoardPanel
                points={state.points}
                delegates={state.delegates}
                tallyOf={tallyOf}
                labelOf={council.labelOf}
                voteLog={state.voteLog}
              />
            ) : (
              <RecordPanel state={state} />
            )}
          </div>
          </aside>
        )}
      </main>
    </div>
  );
}

// ---------------------------------------------------------------------------
// routing
// ---------------------------------------------------------------------------

type Route = { view: "home" | "new" | "read"; id: string | null };

function parseHash(): Route {
  const h = window.location.hash.replace(/^#\/?/, "");
  if (h.startsWith("d/") && h.length > 2) return { view: "read", id: h.slice(2) };
  if (h === "new") return { view: "new", id: null };
  return { view: "home", id: null };
}

/**
 * Three places to be: the library, a council in session, and a debate read.
 *
 * Hash routing rather than a router library, because the whole navigation is
 * three strings. Hash rather than history so a reload — the one thing a debate
 * app cannot avoid, since the engine keeps running while the window does not —
 * lands back where you were.
 */
function useRoute(): Route {
  const [route, setRoute] = useState<Route>(parseHash);
  useEffect(() => {
    const on = () => setRoute(parseHash());
    window.addEventListener("hashchange", on);
    return () => window.removeEventListener("hashchange", on);
  }, []);
  return route;
}

function go(hash: string) {
  if (window.location.hash === hash) return;
  window.location.hash = hash;
}

export default function App() {
  const [theme, setTheme] = useState<"dark" | "light">(() => {
    try {
      const saved = window.localStorage.getItem(THEME_KEY);
      return saved === "light" || saved === "dark" ? saved : "dark";
    } catch {
      return "dark";
    }
  });
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    try {
      window.localStorage.setItem(THEME_KEY, theme);
    } catch {
      // A full or blocked store costs us the preference, not the app.
    }
  }, [theme]);

  const route = useRoute();

  if (route.view === "read" && route.id) {
    return <ReaderView id={route.id} onBack={() => go("#/")} />;
  }
  if (route.view === "new") {
    return (
      <Chamber
        theme={theme}
        onToggleTheme={() => setTheme((t) => (t === "dark" ? "light" : "dark"))}
        onLibrary={() => go("#/")}
      />
    );
  }
  return (
    <HomeView
      theme={theme}
      onToggleTheme={() => setTheme((t) => (t === "dark" ? "light" : "dark"))}
      onConvene={() => go("#/new")}
      onOpen={(id) => go(`#/d/${id}`)}
    />
  );
}
