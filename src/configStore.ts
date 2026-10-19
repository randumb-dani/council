/**
 * The saved setup: what a council is, who sits in it, and where their models live.
 *
 * Extracted from `App.tsx` so the desktop chamber and the phone's own UI read the
 * same store through the same validation. Two copies of a hundred and thirty lines
 * of careful coercion is two copies that drift, and a setup that validates on a
 * laptop and not on a phone is a bug nobody finds.
 *
 * Storage is untrusted input, so nothing here trusts it: every field is read
 * through a validator with a fallback, because a hand-edited `"temperature": "0.7"`
 * once made the setup panel throw during render and blanked the window, and there
 * is no error boundary to catch that.
 */

import type {
  DebateConfig,
  DelegateConfig,
  DecisionConfig,
  EndpointConfig,
  ImageAttachment,
  SeatRole,
} from "./types.ts";
import { canBeTakenByOperator, PROVIDER_BASE_URL } from "./types.ts";
import { MAX_IMAGES } from "./images.ts";

export const STORAGE_KEY = "council.config.v1";
/**
 * The human-in-the-loop council keeps its own setup, for one concrete reason:
 * the operator's seat has no model, so the same saved config cannot serve both
 * councils. Sharing it would mean the automatic debate inherited a seat with
 * no endpoint and refused to start.
 */
export const HUMAN_STORAGE_KEY = "council.config.human.v1";
/** Kept in step with the engine's own bound on rounds. */
export const MAX_ROUNDS = 100;
export const SEAT_ROLES: readonly SeatRole[] = [
  "proponent",
  "opponent",
  "judge",
  "rationale",
  "summarizer",
  "custom",
];

/**
 * The four delegates a debate cannot start without, in the order they speak.
 *
 * The setup check reported a missing one as `no ${need} seat`, which leaked the
 * `SeatRole` value straight into a blocking error — "no proponent seat" means
 * nothing to someone who has only seen the delegate called Advocate. These are
 * the names the UI uses, and the check says which one is missing.
 */
export const REQUIRED_ROLE_NAME: Record<string, string> = {
  proponent: "Advocate",
  opponent: "Critic",
  judge: "Arbiter",
  summarizer: "Summarizer",
};

/**
 * A new endpoint, defaulted.
 *
 * Lives here rather than in the setup panel because both apps build one: the
 * panel's "add a server" button, and `defaultConfig`. It is config construction,
 * not UI, and keeping it in a component file meant the pure config layer had to
 * import a React module to reach it.
 */
export function freshEndpoint(index: number): EndpointConfig {
  return {
    id: `ep-${Date.now().toString(36)}-${index}`,
    name: `Endpoint ${index + 1}`,
    provider: "openaiCompatible",
    baseUrl: PROVIDER_BASE_URL.openaiCompatible,
    apiKey: null,
    headers: {},
    timeoutSecs: 300,
    nativeTools: true,
    stream: true,
    allowInvalidCerts: false,
    includeUsage: true,
    // Nothing is sent until asked for: the shapes are not interchangeable and a
    // model left to its own default may pick a level that fails.
    reasoningWire: "serverDefault",
    reasoningEffort: null,
    thinking: null,
  };
}

export function defaultConfig(): DebateConfig {
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

/** Per-image cap after downscaling (~1.9MB of bytes). */
const MAX_IMAGE_CHARS = 2_500_000;

/** Images live beside the setup, not inside it.
 *
 * Image bytes dwarf everything else in storage (up to ~10MB), so saving them
 * inside the setup meant every motion keystroke re-stringified megabytes. The
 * setup saves light and debounced; images save under this key, only when they
 * change. Old setups that still carry images inline keep working — the side
 * key simply wins when it exists.
 */
export function imagesStorageKey(key: string): string {
  return `${key}.images`;
}

function loadSideImages(key: string): ImageAttachment[] | null {
  try {
    const raw = window.localStorage.getItem(imagesStorageKey(key));
    if (raw === null) return null;
    return sanitizeImages(JSON.parse(raw) as unknown);
  } catch {
    return null;
  }
}

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


export function loadConfig(key: string = STORAGE_KEY): DebateConfig {
  try {
    const raw = window.localStorage.getItem(key);
    if (!raw) return defaultConfig();
    const parsed = JSON.parse(raw) as unknown;
    if (!isRecord(parsed)) return defaultConfig();
    const base = defaultConfig();

    // An empty endpoint list used to throw the whole stored setup away
    // (motion, roster and all) in favour of defaults. Fall back to a fresh
    // endpoint and keep everything else the operator saved.
    let endpoints = Array.isArray(parsed.endpoints)
      ? parsed.endpoints.filter(isRecord).map((e, i) => {
          // A saved config from before providers existed has no `provider`, and
          // these endpoints were all OpenAI-compatible by construction — it was
          // the only wire the engine spoke.
          const provider = oneOf(
            e.provider,
            ["openaiCompatible", "gemini", "deepseek", "openrouter", "anthropic"] as const,
            "openaiCompatible",
          );
          // Anthropic thinking controls vary by model generation and are not
          // sent in this release. Clear stale values so an older setting cannot
          // leak into a request format that rejects it.
          const anthropic = provider === "anthropic";
          return {
            id: str(e.id, `ep-${i}`),
            name: str(e.name, `Endpoint ${i + 1}`),
            provider,
            baseUrl: str(e.baseUrl),
            apiKey: typeof e.apiKey === "string" ? e.apiKey : null,
            headers: isRecord(e.headers)
              ? Object.fromEntries(
                  Object.entries(e.headers).filter(
                    (pair): pair is [string, string] => typeof pair[1] === "string",
                  ),
                )
              : {},
            timeoutSecs: num(e.timeoutSecs, 300, STORED_BOUNDS.endpointTimeoutSecs.min, STORED_BOUNDS.endpointTimeoutSecs.max),
            // Reasoning effort is opt-in per endpoint: the shapes are not
            // interchangeable, and a model left to its own default may pick a level
            // that is slow, or that fails outright.
            reasoningWire: anthropic
              ? "serverDefault"
              : oneOf(
                  e.reasoningWire,
                  ["serverDefault", "flat", "chatTemplate"],
                  "serverDefault",
                ),
            reasoningEffort: anthropic ? null : str(e.reasoningEffort).trim() || null,
            thinking: anthropic ? null : typeof e.thinking === "boolean" ? e.thinking : null,
            nativeTools: bool(e.nativeTools, true),
            stream: bool(e.stream, true),
            allowInvalidCerts: bool(e.allowInvalidCerts, false),
            includeUsage: bool(e.includeUsage, true),
          };
        })
      : base.endpoints;
    if (!endpoints.length) endpoints = base.endpoints;

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
          // Taken seats carry no model or endpoint, so neither is required of
          // them — and the engine refuses a taken seat that is not an arguing
          // one, which is repaired here rather than at convene time.
          human: d.human === true && canBeTakenByOperator(d.seatRole),
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
      timeoutSecs: num(
        rawDecision.timeoutSecs,
        30,
        STORED_BOUNDS.decisionTimeoutSecs.min,
        STORED_BOUNDS.decisionTimeoutSecs.max,
      ),
    };

    return {
      motion: str(parsed.motion),
      extraInstructions:
        typeof parsed.extraInstructions === "string" ? parsed.extraInstructions : null,
      resumeRecord:
        typeof parsed.resumeRecord === "string" ? parsed.resumeRecord : null,
      endpoints,
      delegates,
      maxRounds: num(parsed.maxRounds, base.maxRounds, STORED_BOUNDS.maxRounds.min, STORED_BOUNDS.maxRounds.max),
      autoAdjourn: bool(parsed.autoAdjourn, false),
      closingStatements: bool(parsed.closingStatements, true),
      decision,
      maxExtraRounds: num(
        parsed.maxExtraRounds,
        base.maxExtraRounds,
        STORED_BOUNDS.maxExtraRounds.min,
        STORED_BOUNDS.maxExtraRounds.max,
      ),
      // Web access stays off for a config that predates it, exactly as it does
      // in the engine: reaching off the machine is opt-in, never inherited.
      allowWeb: parsed.allowWeb === true,
      maxFetchesPerTurn: num(parsed.maxFetchesPerTurn, 3, STORED_BOUNDS.maxFetchesPerTurn.min, STORED_BOUNDS.maxFetchesPerTurn.max),
      maxFetchChars: num(parsed.maxFetchChars, 6000, STORED_BOUNDS.maxFetchChars.min, STORED_BOUNDS.maxFetchChars.max),
      // Image bytes dwarf everything else in storage: keep only sane ones.
      // Prefer the side key (see imagesStorageKey); fall back to images still
      // stored inline by older saves.
      images: loadSideImages(key) ?? sanitizeImages(parsed.images),
    };
  } catch {
    return defaultConfig();
  }
}

/** Extra delegates a council may have. Must match the engine's own limit. */
export const MAX_CUSTOM_DELEGATES = 6;

/**
 * The ranges the loader will accept, exported so the setup panel's sliders use
 * the same numbers.
 *
 * These are not the engine's limits — they are what `loadConfig` will hand back
 * from storage. When the two drifted apart, a stored value above the slider's
 * maximum rendered pinned at the right-hand end while the number beside it
 * printed the real figure, and nudging the slider silently snapped the value
 * down by up to 3300 seconds. `SETUP_MAX_ROUNDS` used to say, in a comment, that
 * it "must match the setup panel's own rounds slider" — which is the kind of
 * agreement that only exists until someone edits one side.
 */
export const STORED_BOUNDS = {
  endpointTimeoutSecs: { min: 10, max: 3600 },
  decisionTimeoutSecs: { min: 5, max: 300 },
  maxRounds: { min: 1, max: MAX_ROUNDS },
  maxExtraRounds: { min: 0, max: MAX_ROUNDS },
  maxFetchesPerTurn: { min: 1, max: 8 },
  maxFetchChars: { min: 500, max: 40000 },
} as const;

/**
 * What deleting a server would do, before it does it.
 *
 * Returns the confirmation sentence, or null when the deletion is unambiguous
 * and needs no prompt. The re-pointing itself — the part with the real logic —
 * is the same either way; only the warning is conditional.
 *
 * The danger is that every delegate on the deleted server moves to whatever is
 * left, which may not serve their model at all. The debate then convenes cleanly
 * and fails model-not-found on those seats, one turn each, a long way from the
 * click that caused it.
 *
 * Lives here rather than in the setup panel because it is a pure function of the
 * config, and this module is where config shaping and repair already live — and
 * where its tests are.
 */
export function planEndpointRemoval(
  config: DebateConfig,
  id: string,
): { sentence: string | null; fallback: string } {
  const endpoints = config.endpoints.filter((e) => e.id !== id);
  const fallback = endpoints[0]?.id ?? "";
  const moving = config.delegates.filter((d) => d.endpointId === id);
  if (moving.length === 0) return { sentence: null, fallback };
  const target = config.endpoints.find((e) => e.id === fallback);
  const where = target ? target.name || target.baseUrl : "no other server";
  const names = moving.map((d) => d.label);
  const listed =
    names.length <= 3
      ? names.join(", ")
      : `${names.slice(0, 3).join(", ")} and ${names.length - 3} more`;
  return {
    sentence: `Remove this server? ${listed} ${names.length === 1 ? "is" : "are"} set to use it and would move to ${where}, which may not serve ${names.length === 1 ? "its" : "their"} model.`,
    fallback,
  };
}

/**
 * Forget the saved setup and start over from defaults.
 *
 * Which store it clears is the caller's business: the two councils keep their
 * setups apart, so resetting one must not silently reset the other.
 */
export function resetConfig(key = STORAGE_KEY) {
  try {
    window.localStorage.removeItem(key);
  } catch {
    /* storage unavailable; the in-memory reset below still applies */
  }
  window.location.reload();
}

