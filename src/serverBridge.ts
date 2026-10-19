// Talking to council-serve, the localhost companion to this UI.
//
// The desktop app reaches the engine through Tauri commands; a plain browser
// cannot. When this page itself was served by `council-serve` (same origin,
// `/api/health` answers), every operation below goes over REST plus one
// server-sent-events stream instead. Otherwise the caller falls back to the
// scripted stand-in in `bridge.ts`.
//
// One rule governs the whole module: **no secrets leave the browser**.
// Endpoint profiles travel with `apiKey: null`; the server merges keys from
// its own store by endpoint id and rejects any request carrying one.

import type {
  DebateConfig,
  EndpointConfig,
  EngineEvent,
  HumanBallotSubmit,
  HumanTurnSubmit,
} from "./types";

/** Same shape as `bridge.DebateSnapshot`, defined here so this module never imports `bridge`. */
export interface ServerSnapshot {
  motion: string;
  round: number;
  maxRounds?: number;
  delegates?: {
    id: string;
    label: string;
    model: string;
    color: string;
    endpoint: string;
    role: string;
    human?: boolean;
  }[];
  tallies?: Record<number, unknown>;
  points: unknown[];
  polls: unknown[];
  speeches: unknown[];
  finished: boolean;
  /** What the operator is being asked, when this debate is parked on them. */
  pending?: { kind: "turn"; delegate: string; turn: number; round: number } | {
    kind: "ballot";
    delegate: string;
    ballot: "poll" | "review" | "adjourn" | "extension";
    question: string;
    options: string[];
    pointId: number | null;
  };
}

/** Fired on `window` when the event stream reports a gap. The UI answers by
 * refetching the snapshot, which is authoritative — the stream is a live view,
 * the snapshot is the record. */
export const RESYNC_EVENT = "council:resync";

let served: boolean | null = null;
let activeId: string | null = null;

export function isServed(): boolean {
  return served === true;
}

/** True only for a genuine health answer. Any dev server's SPA fallback
 * returns 200 with HTML for unknown paths — under `tauri dev` that fallback
 * made the desktop app believe a local server was present, swapping in the
 * server key UI and blanking keys before send. Status alone is not proof. */
export function isHealthBody(body: unknown): boolean {
  if (typeof body !== "object" || body === null) return false;
  const v = body as Record<string, unknown>;
  return v.ok === true && v.service === "council-serve";
}

/** Probe the same-origin API. Cached after the first success; failures are
 * re-probed because the server may simply not be started yet. */
export async function detectServed(timeoutMs = 1500): Promise<boolean> {
  if (served === true) return true;
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeoutMs);
  try {
    const res = await fetch("/api/health", { signal: ctrl.signal });
    if (!res.ok) {
      served = false;
    } else {
      try {
        served = isHealthBody(await res.json());
      } catch {
        // HTML fallback, not the API.
        served = false;
      }
    }
  } catch {
    served = false;
  } finally {
    clearTimeout(timer);
  }
  return served;
}

async function readError(res: Response, fallback: string): Promise<string> {
  try {
    const body = (await res.json()) as { error?: unknown };
    if (typeof body.error === "string" && body.error) return body.error;
  } catch {
    /* not JSON; fall through */
  }
  return `${fallback} (HTTP ${res.status})`;
}

// ---------------------------------------------------------------------------
// secrets stay home
// ---------------------------------------------------------------------------

/** Copy of the config with every key blanked. What the server receives. */
export function stripSecrets(config: DebateConfig): DebateConfig {
  return {
    ...config,
    endpoints: config.endpoints.map((e) => ({ ...e, apiKey: null })),
    decision: { ...config.decision, apiKey: null },
  };
}

/** True when the config still holds a key the served API would refuse. Used
 * for the one-time nudge to move keys onto the server. */
export function configHasSecrets(config: DebateConfig): boolean {
  return (
    config.endpoints.some((e) => !!e.apiKey) || !!config.decision.apiKey
  );
}

function stripEndpoint(ep: EndpointConfig): EndpointConfig {
  return { ...ep, apiKey: null };
}

// ---------------------------------------------------------------------------
// debates
// ---------------------------------------------------------------------------

/** Ask the server what is running right now, bypassing the cache. */
async function refreshActive(): Promise<string | null> {
  try {
    const res = await fetch("/api/debates/active");
    if (!res.ok) return null;
    const body = (await res.json()) as { id?: string | null };
    activeId = typeof body.id === "string" ? body.id : null;
    return activeId;
  } catch {
    return null;
  }
}

async function debateId(): Promise<string | null> {
  if (activeId) return activeId;
  return refreshActive();
}

/** The debate went away between calls (a new one superseded it, or the
 * server restarted). Drop the cached id so the next call re-asks. */
function forgot(id: string | null): null {
  if (id && id === activeId) activeId = null;
  return null;
}

export async function start(config: DebateConfig): Promise<void> {
  const res = await fetch("/api/debates", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(stripSecrets(config)),
  });
  if (!res.ok) throw new Error(await readError(res, "the local server refused the debate"));
  const body = (await res.json()) as { id?: unknown };
  if (typeof body.id !== "string") throw new Error("the local server answered without a debate id");
  activeId = body.id;
  // The engine emits DebateStarted/RoundStarted/TurnStarted within microseconds of
  // the POST returning, and the server has no backlog for a subscriber that has not
  // connected yet — so waiting for the next poll (up to two seconds) lost them and
  // left the chamber reading "not started" with a Convene button that 409s.
  wakeSubscription();
}

export async function stop(): Promise<void> {
  const id = await debateId();
  // Not a silent success. This used to return quietly, so pressing Stop while the
  // server was briefly unreachable did nothing at all: the red dot kept pulsing
  // and the debate it was supposed to be spending money on kept running.
  if (!id) throw new Error("the local server would not say which debate is running, so it could not be stopped");
  const res = await fetch(`/api/debates/${encodeURIComponent(id)}/stop`, { method: "POST" });
  if (res.status === 404) {
    forgot(id);
    return;
  }
  if (!res.ok) throw new Error(await readError(res, "the local server would not stop the debate"));
}

export async function skip(): Promise<boolean> {
  const id = await debateId();
  if (!id) return false;
  const res = await fetch(`/api/debates/${encodeURIComponent(id)}/skip`, { method: "POST" });
  if (res.status === 404) {
    forgot(id);
    return false;
  }
  if (!res.ok) throw new Error(await readError(res, "the local server would not skip the turn"));
  const body = (await res.json()) as { skipped?: unknown };
  return body.skipped === true;
}

/**
 * Inject a note into the running debate.
 *
 * Returns whether the note actually reached anyone. This used to return
 * `Promise<void>` and return quietly when there was no debate, so a note typed
 * before Convene — or after the debate finished — was dropped with nothing to
 * show for it: the input cleared and the operator reasonably believed the room
 * had heard them. A silent no-op here is worse than a visible failure, because
 * the operator's next turn then argues against a point the models never saw.
 */
export async function injectNote(text: string): Promise<boolean> {
  const id = await debateId();
  if (!id) return false;
  const res = await fetch(`/api/debates/${encodeURIComponent(id)}/inject`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ text }),
  });
  if (res.status === 404) {
    forgot(id);
    return false;
  }
  if (!res.ok) throw new Error(await readError(res, "the local server dropped the note"));
  return true;
}

export async function resolveExtension(approved: boolean, rounds: number): Promise<string> {
  const id = await debateId();
  if (!id) return "no debate is running on the local server";
  const res = await fetch(`/api/debates/${encodeURIComponent(id)}/resolve-extension`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ approved, rounds }),
  });
  if (res.status === 404) {
    forgot(id);
    return "no debate is running on the local server";
  }
  if (!res.ok) throw new Error(await readError(res, "the local server refused the answer"));
  const body = (await res.json()) as { message?: unknown };
  return typeof body.message === "string" ? body.message : "answered";
}

export async function debateState(): Promise<ServerSnapshot | null> {
  const id = await debateId();
  if (!id) return null;
  const res = await fetch(`/api/debates/${encodeURIComponent(id)}/state`);
  if (!res.ok) {
    if (res.status === 404) forgot(id);
    return null;
  }
  return (await res.json()) as ServerSnapshot;
}

export async function listModels(endpoint: EndpointConfig): Promise<string[]> {
  const res = await fetch("/api/endpoints/probe", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ endpoint: stripEndpoint(endpoint) }),
  });
  if (!res.ok) throw new Error(await readError(res, "the local server could not reach the endpoint"));
  const body = (await res.json()) as { models?: unknown };
  return Array.isArray(body.models) ? body.models.filter((m): m is string => typeof m === "string") : [];
}

// ---------------------------------------------------------------------------
// event stream
// ---------------------------------------------------------------------------

/**
 * Remembers the highest sequence number seen, so a reconnect that replays the gap
 * does not double-speak turns already on the transcript.
 *
 * A forward hole is reported as well as a repeat. `accept` returning `false` for a
 * gap used to be silent: the server's only "you missed something" signal was a
 * resync marker, which a *reconnecting* client never received because a fresh
 * broadcast subscriber starts at the tail. A phone that backgrounded mid-debate
 * therefore absorbed the hole — points raised in those seconds never reached the
 * board and nothing said so.
 */
export function createSeqTracker(): {
  last: number;
  accept(seq: unknown): boolean;
  /** True if the last `accept` call was passed a sequence it had already seen. */
  repeated(): boolean;
} {
  let repeatedLast = false;
  const t = {
    last: -1,
    repeated: () => repeatedLast,
    accept(seq: unknown): boolean {
      if (typeof seq !== "number" || !Number.isFinite(seq)) {
        // Not a sequence number at all: refusing it is right, and it is also
        // exactly the situation where the stream has gone quiet without telling
        // anyone, so the caller refetches.
        repeatedLast = true;
        return false;
      }
      if (seq <= t.last) {
        repeatedLast = true;
        return false;
      }
      repeatedLast = false;
      t.last = seq;
      return true;
    },
  };
  return t;
}

function frameOf(raw: string): { seq: unknown; event?: EngineEvent; resync?: boolean } | null {
  try {
    const v = JSON.parse(raw) as { seq?: unknown; event?: EngineEvent; resync?: unknown };
    if (typeof v !== "object" || v === null) return null;
    return { seq: v.seq, event: v.event, resync: v.resync === true };
  } catch {
    return null;
  }
}

/** Re-arm the live subscription now. See the note where `wake` is assigned. */
let wake: (() => void) | null = null;

export function wakeSubscription(): void {
  wake?.();
}

export async function subscribe(onEvent: (e: EngineEvent) => void): Promise<() => void> {
  let stopped = false;
  let source: EventSource | null = null;
  let current: string | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  // Fresh debate, fresh numbering: the server counts from zero per process,
  // so a carried-over watermark would swallow the new debate whole.
  let seen = createSeqTracker();

  const close = () => {
    source?.close();
    source = null;
    current = null;
  };
  const onFrame = (raw: string) => {
    const frame = frameOf(raw);
    if (!frame) return;
    if (frame.resync) {
      // The server dropped frames for this subscriber. Ask the UI to
      // refetch the snapshot rather than render a transcript with a hole.
      window.dispatchEvent(new CustomEvent(RESYNC_EVENT));
      return;
    }
    if (!frame.event) return;
    if (seen.accept(frame.seq)) {
      onEvent(frame.event);
      return;
    }
    // A repeat is the server replaying our own tail: harmless, drop it. But a
    // frame we cannot number at all means the stream is not what we think it is,
    // so refetch rather than guess.
    if (typeof frame.seq !== "number") window.dispatchEvent(new CustomEvent(RESYNC_EVENT));
  };
  // Rebind, remembering where we got to so the server can replay the gap. Only
  // `EventSource` cannot send a `Last-Event-ID` by hand, and does not need to for
  // the case that matters: the browser replays the last `id:` it saw as
  // `Last-Event-ID` on its own reconnect, and the server now fills the gap from
  // that. A manual rebind after an idle close cannot say where it got to, so the
  // server sends a resync marker instead and the UI refetches the snapshot —
  // correct, if less incremental than it could be.
  const attach = (id: string) => {
    close();
    current = id;
    source = new EventSource(`/api/debates/${encodeURIComponent(id)}/events`);
    source.onmessage = (m) => onFrame(m.data as string);
    source.onerror = () => {
      // Deliberately left alone. Tearing the source down here would cancel the
      // browser's own reconnect, which is the very thing that carries
      // `Last-Event-ID` and closes the gap.
    };
  };
  const ensure = async () => {
    if (stopped) return;
    // Always re-ask: a finished debate is superseded by the next start, and
    // the stream must follow the new id rather than poll a grave.
    const id = await refreshActive();
    if (stopped) return;
    if (id && id !== current) {
      // A different debate starts its numbering again, so the watermark goes with
      // it — carrying one over would swallow the new debate whole. Reset on
      // *every* new id, not just the first attach: the second convene in a
      // session otherwise inherits the first debate's high-water mark and drops
      // everything until the new numbering catches up.
      seen = createSeqTracker();
      attach(id);
    } else if (id && id === current && (!source || source.readyState === EventSource.CLOSED)) {
      // The socket died without changing debates (sleep, killed connection):
      // the browser's own reconnect only fires while the source is alive, so a
      // closed source with the same id would poll forever and never re-attach,
      // freezing the transcript with no error.
      attach(id);
    }
    if (!id) close();
    timer = setTimeout(ensure, id ? 10000 : 2000);
  };
  // Called after a debate starts, so the stream attaches immediately instead of
  // waiting up to two seconds for the next poll. The engine emits
  // `DebateStarted`, `RoundStarted` and `TurnStarted` within microseconds of the
  // POST returning and the server keeps no backlog for a subscriber that has not
  // arrived yet, so that window was a debate whose header read "not started" with
  // no way back except a reload.
  // Exposed so `startDebate` can attach the stream the instant the POST returns,
  // rather than at the mercy of the poll below.
  wake = () => {
    if (timer) clearTimeout(timer);
    void ensure();
  };
  return () => {
    stopped = true;
    if (timer) clearTimeout(timer);
    close();
    wake = null;
  };
}

// ---------------------------------------------------------------------------
// the taken seat: a turn and a ballot, submitted
// ---------------------------------------------------------------------------

/**
 * Answer a taken seat's turn.
 *
 * A refusal is thrown rather than swallowed: it is the operator's cue to fix
 * the action, and a silent no-op would leave them believing they had spoken.
 */
export async function submitTurn(turn: HumanTurnSubmit): Promise<void> {
  const id = await debateId();
  if (!id) throw new Error("no debate is running on the local server");
  const res = await fetch(`/api/debates/${encodeURIComponent(id)}/submit-turn`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ turn }),
  });
  if (!res.ok) throw new Error(await readError(res, "the server refused that turn"));
}

/** Answer a ballot on the operator's own seat. Refusals throw, as above. */
export async function submitBallot(ballot: HumanBallotSubmit): Promise<void> {
  const id = await debateId();
  if (!id) throw new Error("no debate is running on the local server");
  const res = await fetch(`/api/debates/${encodeURIComponent(id)}/submit-ballot`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ballot }),
  });
  if (!res.ok) throw new Error(await readError(res, "the server refused that ballot"));
}

// ---------------------------------------------------------------------------
// server-side keys: presence in, never values out
// ---------------------------------------------------------------------------

export async function keyStatus(ids: string[]): Promise<Record<string, boolean>> {
  const res = await fetch("/api/keys/status", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ids }),
  });
  if (!res.ok) throw new Error(await readError(res, "the local server would not report key status"));
  const body = (await res.json()) as Record<string, unknown>;
  const out: Record<string, boolean> = {};
  for (const id of ids) out[id] = body[id] === true;
  return out;
}

/**
 * Store a key on the server (`apiKey`) or forget it (`null`). The value
 * never comes back out through any API. */
export async function setServerKey(
  endpointId: string,
  apiKey: string | null,
  baseUrl?: string,
): Promise<boolean> {
  const res = await fetch("/api/keys", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    // `baseUrl` is not decoration: the server records it and refuses to send that
    // key to any other address, which is what stops this API being usable as a
    // credential relay. Forgetting a key has to ask for it explicitly too, so a
    // request that merely forgets the field cannot delete a stored secret.
    body: JSON.stringify(
      apiKey === null ? { endpointId, clear: true } : { endpointId, apiKey, baseUrl },
    ),
  });
  if (!res.ok) throw new Error(await readError(res, "the local server would not store the key"));
  const body = (await res.json()) as { stored?: unknown };
  return body.stored === true;
}
