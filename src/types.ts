// Mirrors `council-core`'s serde representation exactly. Keep the two in step:
// the Rust side is the source of truth.

export type Verdict = "win" | "lose" | "stalemate";
export type PointStatus = "open" | "carried" | "fell" | "stalemate" | "withdrawn";
export type NoticeTone = "info" | "warn" | "error";

export interface VoteRecord {
  delegate: string;
  verdict: Verdict;
  reason: string | null;
  round: number;
}

export interface Remark {
  by: string;
  round: number;
  text: string;
}

export type StalemateReading = "unsolvable" | "too_dense" | "decidable";

export interface StalemateBallot {
  delegate: string;
  reading: StalemateReading;
  reason: string | null;
}

export interface StalemateReview {
  point: number;
  openedBy: string;
  round: number;
  ballots: StalemateBallot[];
  closed: boolean;
  outcome: StalemateReading | null;
}

export interface Point {
  id: number;
  text: string;
  raisedBy: string;
  round: number;
  withdrawn: boolean;
  withdrawnReason: string | null;
  beats: number[];
  votes: Record<string, VoteRecord>;
  rebuttals: Remark[];
  steelmen: Remark[];
  evidence: Remark[];
  concededBy: Record<string, string>;
  /** The latest deadlock review, if the point was ever deadlocked enough to need one. */
  review: StalemateReview | null;
  /**
   * The Arbiter's ruling, if it settled the point.
   *
   * The tally is left alone when this is present, so the board shows that the
   * Arbiter overrode the count rather than quietly rewriting it.
   */
  ruling: PointRuling | null;
}

export interface PointRuling {
  by: string;
  round: number;
  carries: boolean;
  reason: string;
}

export interface Tally {
  wins: number;
  losses: number;
  stalemates: number;
  abstentions: number;
  score: number;
  status: PointStatus;
  contested: boolean;
  /** Effective rating per delegate, implicit proponent votes included. */
  ratings: Record<string, Verdict>;
}

export interface PollBallot {
  delegate: string;
  choice: string;
  reason: string | null;
  /** Calibrated confidence, when a decision model cast this ballot. */
  confidence: number | null;
}

export interface Poll {
  id: number;
  question: string;
  options: string[];
  openedBy: string;
  round: number;
  ballots: PollBallot[];
  closed: boolean;
}

export interface Speech {
  id: number;
  delegate: string;
  round: number;
  text: string;
  /** The thinking behind the speech, when the server exposed it. */
  reasoning: string;
  actions: string[];
  phase: "queued" | "speaking" | "waiting" | "done";
}

export interface CarriedPoint {
  id: number;
  text: string;
  raisedBy: string;
  score: number;
  wins: number;
  losses: number;
  stalemates: number;
  /** How the council ruled the deadlock, when it did. */
  ruling: StalemateReading | null;
}

export type EndedHow = "adjourned" | "settled" | "calledSettled" | "complete" | "stopped";

export interface DebateVerdict {
  motion: string;
  rounds: number;
  /** Set when the Summarizer read the record as converged and ended it there. */
  settledReason?: string | null;
  endedHow: EndedHow;
  carried: CarriedPoint[];
  fell: CarriedPoint[];
  stalemate: CarriedPoint[];
  withdrawn: CarriedPoint[];
  crux: CarriedPoint | null;
  summary: string;
  adjournedBy: string | null;
  /** The summarizer's end result, written after the last round. */
  finalSummary: string | null;
  /** The direct answer to the motion, when the council reached one. */
  conclusion: Conclusion | null;
}

export type Provider =
  | "openaiCompatible"
  | "gemini"
  | "deepseek"
  | "openrouter"
  | "anthropic";

/**
 * The documented base URL for each provider.
 *
 * A default, not an override: picking a provider fills these in and every field
 * stays editable, because running your own proxy in front of any of them is a
 * normal thing to do.
 */
export const PROVIDER_BASE_URL: Record<Provider, string> = {
  openaiCompatible: "http://127.0.0.1:8888",
  gemini: "https://generativelanguage.googleapis.com/v1beta/openai",
  deepseek: "https://api.deepseek.com",
  openrouter: "https://openrouter.ai/api/v1",
  anthropic: "https://api.anthropic.com",
};

export const PROVIDER_LABEL: Record<Provider, string> = {
  openaiCompatible: "OpenAI-compatible",
  gemini: "Google Gemini",
  deepseek: "DeepSeek",
  openrouter: "OpenRouter",
  anthropic: "Anthropic Claude",
};

/**
 * How much thinking a model does before answering, per provider.
 *
 * Gemini names its levels differently from everything else and will reject an
 * OpenAI-shaped `reasoning_effort` rather than ignore it, so the values offered
 * are the provider's own.
 */
export const PROVIDER_EFFORTS: Record<Provider, readonly string[]> = {
  openaiCompatible: ["low", "medium", "high", "xhigh"],
  gemini: ["minimal", "low", "medium", "high"],
  deepseek: ["low", "medium", "high", "max"],
  openrouter: ["low", "medium", "high"],
  // Anthropic's future `output_config.effort` vocabulary. Controls are disabled
  // for now because thinking modes differ by model generation; these names only
  // document where a later mapping should start.
  anthropic: ["low", "medium", "high", "max"],
};

export interface EndpointConfig {
  id: string;
  name: string;
  /** Which API this server speaks. Sets defaults; every field stays editable. */
  provider: Provider;
  baseUrl: string;
  apiKey: string | null;
  headers: Record<string, string>;
  timeoutSecs: number;
  /** Where reasoning effort goes on the wire. No standard; see REASONING_WIRE. */
  reasoningWire: ReasoningWire;
  /** Spelled exactly as this model spells it. Blank means send nothing. */
  reasoningEffort: string | null;
  /** Turn the thinking block off, for templates that gate it. */
  thinking: boolean | null;
  nativeTools: boolean;
  stream: boolean;
  allowInvalidCerts: boolean;
  /**
   * Ask the server to report token usage. On by default: the rail totals are
   * the server's own counts, never an estimate. A server that does not know
   * `stream_options` rejects the whole request, so the engine retries without
   * it and remembers the refusal for the session.
   */
  includeUsage: boolean;
}

export interface DelegateConfig {
  id: string;
  label: string;
  model: string;
  endpointId: string;
  color: string;
  /** The seat's fixed role — the four convening essentials sit exactly once each; the Rationale at most once. */
  seatRole: SeatRole;
  role: string | null;
  persona: string | null;
  temperature: number | null;
  /**
   * Taken by the operator rather than a model.
   *
   * The engine waits for this seat's turn instead of calling an endpoint, so
   * `model` and `endpointId` are not required — only on arguing seats. Judging
   * and summarizing stay with the models and are refused if marked here.
   */
  human?: boolean;
}

/**
 * How an endpoint wants reasoning effort expressed.
 *
 * `serverDefault` sends nothing. `flat` puts `reasoning_effort` beside
 * `temperature` (OpenAI, vLLM, NIM). `chatTemplate` nests it under
 * `chat_template_kwargs` — llama-server has no top-level `reasoning_effort` at
 * all, so this is what Unsloth Desktop, LM Studio and any other llama.cpp server
 * need.
 */
export type ReasoningWire = "serverDefault" | "flat" | "chatTemplate";

export type SeatRole = "proponent" | "opponent" | "judge" | "rationale" | "summarizer" | "custom";

/** Every seat role, for validating values read back out of storage. */
export const SEAT_ROLES: readonly SeatRole[] = [
  "proponent",
  "opponent",
  "judge",
  "rationale",
  "summarizer",
  "custom",
];

/** The debating roles — the ones a person may take. See `canBeTakenByOperator`. */
const TAKEABLE_ROLES: readonly SeatRole[] = ["proponent", "opponent", "rationale", "custom"];

/** Which ballot is waiting on the operator. Matches the engine's enum. */
export type BallotKind = "poll" | "review" | "adjourn" | "extension";

/** One tool call as the action picker produces it: the same shape the engine
 *  validates for a model, so there is one dialect rather than two. */
export interface HumanActionCall {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
}

/** What a taken turn submits. */
export interface HumanTurnSubmit {
  text: string;
  thinking: string;
  calls: HumanActionCall[];
}

/** What a taken ballot submits — one variant per ballot kind, so a client
 *  cannot answer a poll with a deadlock reading. */
export type HumanBallotSubmit =
  | { kind: "poll"; choice: string; reason: string | null; confidence: number | null }
  | { kind: "review"; reading: string; reason: string | null; vote: Verdict | null }
  | { kind: "adjourn"; confirm: boolean; reason: string | null }
  | { kind: "extension"; grant: boolean; reason: string | null };

export const ROLE_LABEL: Record<SeatRole, string> = {
  proponent: "Proponent",
  opponent: "Opponent",
  judge: "Judge",
  rationale: "Rationale",
  summarizer: "Summarizer",
  custom: "Custom",
};

/**
 * What each role is for, as one plain sentence.
 *
 * The rail's role pill showed this for only two of the five roles, so most of
 * them explained themselves nowhere. Every role now has one, including the
 * custom case, which is the only way a user learns what filling that row does.
 */
export const ROLE_HINT: Record<SeatRole, string> = {
  proponent: "Argues for the motion. It raises the points it wants to win.",
  opponent: "Argues against the motion, and is expected to steelman it first — that is, to state the other side as well as its own side could.",
  judge: "Votes on each point once the argument is over. It does not argue itself.",
  rationale: "Looks for weaknesses in the reasoning — contradictions, claims nothing supports, conclusions that do not follow.",
  summarizer: "Keeps track of what has happened and writes the answer at the end. It never argues a side.",
  custom: "An extra delegate you added. It argues the motion alongside the others.",
};

/**
 * Which roles the operator may argue themselves.
 *
 * The debating roles only, and the same rule the engine enforces: an Arbiter
 * ruling on an argument they took part in is not adjudication, and a
 * Summarizer writing the result they argued for is not a record.
 */
export function canBeTakenByOperator(role: unknown): boolean {
  return (
    typeof role === "string" &&
    (TAKEABLE_ROLES as readonly string[]).includes(role)
  );
}

export type DecisionProvider = "seat" | "jev";

export interface DecisionConfig {
  provider: DecisionProvider;
  baseUrl: string;
  apiKey: string | null;
  model: string;
  timeoutSecs: number;
}

export interface DebateConfig {
  motion: string;
  extraInstructions: string | null;
  /** The record of an unfinished debate being resumed, carried in the prompt.
   *
   * Its own field rather than folded into `extraInstructions`: that field is the
   * operator's standing instructions, they are saved, and a resume was overwriting
   * them with up to 6000 characters of transcript — permanently, so the *next*
   * unrelated debate was still being handed a dead debate's argument. */
  resumeRecord?: string | null;
  endpoints: EndpointConfig[];
  delegates: DelegateConfig[];
  maxRounds: number;
  autoAdjourn: boolean;
  closingStatements: boolean;
  decision: DecisionConfig;
  /** Total extra rounds the summarizer may ask for across the debate. */
  maxExtraRounds: number;
  /** Whether the arguing seats may look terms up on Wikipedia and Wiktionary. */
  allowWeb: boolean;
  /** How many pages one seat may read in a single turn. */
  maxFetchesPerTurn: number;
  /** How much of a page one seat may read, in characters. */
  maxFetchChars: number;
  /** Images attached to the motion for the council to argue over. */
  images: ImageAttachment[];
}

/** One image on the motion. `dataUrl` carries the bytes to the engine. */
export interface ImageAttachment {
  id: string;
  name: string;
  mediaType: string;
  dataUrl: string;
}

/** What the record keeps: what was attached, without the bytes. */
export interface ImageInfo {
  id: string;
  name: string;
}

/**
 * The direct answer to the motion. `agreed` is the engine's verdict, not the
 * summarizer's claim: prose cannot settle a debate the board did not.
 */
export interface Conclusion {
  agreed: boolean;
  answer: string;
  lean: string | null;
  /** The tally behind the lean, e.g. "2 carried, 1 fell, 1 deadlocked". */
  tally: string;
}

export interface DelegateBrief {
  id: string;
  label: string;
  model: string;
  color: string;
  endpoint: string;
  /**
   * The seat's fixed role — as opposed to `DelegateConfig.role`, which is the
   * free-text role line and is null for every essential seat. Two fields, two
   * meanings, one name; anything reading `state.delegates` wants this one.
   */
  role: SeatRole;
  /** Taken by the operator rather than a model. */
  human?: boolean;
}

export type EngineEvent =
  | {
      type: "debateStarted";
      motion: string;
      rounds: number;
      maxExtraRounds: number;
      delegates: DelegateBrief[];
      /** What images the motion carries, without the bytes. */
      images: ImageInfo[];
    }
  | { type: "roundStarted"; round: number }
  | { type: "intermissionStarted"; round: number }
  | {
      type: "turnStarted";
      turn: number;
      delegate: string;
      label: string;
      color: string;
      model: string;
      round: number;
    }
  /** The floor belongs to the operator: this seat is taken, so the turn waits
   *  for a submit. `brief` is the same board-plus-floor text a model would be
   *  shown, which is what keeps a human arguing from the record. */
  | {
      type: "yourTurn";
      turn: number;
      delegate: string;
      label: string;
      round: number;
      brief: string;
    }
  /** A ballot the operator must answer for their seat. */
  | {
      type: "ballotAsked";
      delegate: string;
      kind: BallotKind;
      question: string;
      options: string[];
      pointId: number | null;
    }
  | {
      /** What the engine is doing right now, in words meant for the operator. */
      type: "turnActivity";
      turn: number;
      delegate: string;
      what: string;
    }
  | { type: "turnProgress"; turn: number; delegate: string; elapsedSecs: number; tokens: number }
  | { type: "turnSkipped"; turn: number; delegate: string; label: string }
  | { type: "token"; turn: number; text: string }
  | { type: "reasoning"; turn: number; text: string }
  | { type: "speechFinished"; turn: number; speech: Speech }
  | { type: "actionTaken"; turn: number; delegate: string; action: string; detail: string }
  | { type: "actionRejected"; turn: number; delegate: string; tool: string; reason: string }
  | {
      type: "turnUsage";
      turn: number;
      delegate: string;
      promptTokens: number;
      completionTokens: number;
    }
  | {
      type: "webFetched";
      turn: number;
      delegate: string;
      source: string;
      url: string;
      title: string;
      text: string;
      truncated: boolean;
    }
  | { type: "pointUpdated"; point: Point; tally: Tally; resolved: boolean }
  | { type: "pollOpened"; poll: Poll }
  | {
      type: "pollBallotCast";
      pollId: number;
      delegate: string;
      choice: string;
      result: [string, number][];
    }
  | { type: "pollClosed"; poll: Poll; outcome: string | null }
  | { type: "reviewOpened"; pointId: number; review: StalemateReview }
  | {
      type: "reviewBallotCast";
      pointId: number;
      delegate: string;
      reading: StalemateReading;
      result: [string, number][];
    }
  | { type: "reviewClosed"; pointId: number; review: StalemateReview; outcome: StalemateReading | null }
  | { type: "adjournMoved"; by: string; reason: string; round: number }
  | {
      type: "adjournBallotCast";
      delegate: string;
      confirm: boolean;
      result: [string, number][];
    }
  | { type: "adjournDecided"; passed: boolean; yes: number; no: number; reason: string }
  | { type: "extensionProposed"; round: number; rounds: number; reason: string }
  | {
      /** One seat's answer to a request for more rounds. */
      type: "extensionBallotCast";
      round: number;
      delegate: string;
      grant: boolean;
      reason: string | null;
    }
  | { type: "extensionGranted"; added: number; total: number; budgetLeft: number }
  | { type: "extensionDeclined"; round: number }
  | { type: "rosterUpdated"; delegates: DelegateBrief[]; speech: Speech | null }
  | { type: "notice"; text: string; tone: NoticeTone }
  | { type: "debateEnded"; verdict: DebateVerdict }
  /**
   * A seat failed, or the whole run gave up. `fatal` decides whether the
   * operator keeps their controls: a per-seat failure must not take Stop away
   * from a debate that is still going.
   */
  | { type: "failed"; message: string; fatal: boolean };

/**
 * Effort levels worth offering, grouped by who takes them.
 *
 * Kept short on purpose. Qwen3.8's template *raises* on a value it does not
 * know — `high` and `max` come back as HTTP 400 — so a free-text box needs a
 * datalist that knows what this model actually accepts, and the operator needs
 * to know that guessing is not free.
 */
/**
 * How much a model thinks before answering, as one number.
 *
 * Server-specific, and a wrong value is usually rejected rather than ignored,
 * so this needs to name the servers a user is most likely to be pointing at.
 * The per-server quirks (Qwen's xhigh returning empty on llama.cpp,
 * llama-server lacking the field entirely) live in the README, not in a
 * tooltip nobody can read — a native `title` cannot render a newline, so the
 * old three-paragraph string arrived as one run-on line.
 */
export const REASONING_EFFORT_HINT =
  "Higher lets a model think longer before answering. What each provider accepts is listed as you type.";

export const VERDICT_GLYPH: Record<Verdict, string> = {
  win: "▲",
  lose: "▼",
  stalemate: "◆",
};

/**
 * The same three outcomes as words, for screen readers and tooltips.
 *
 * The glyphs alone are not self-describing — a tooltip reading `win` or
 * `lose` was leaking the engine's enum, and `▲`/`▼`/`◆` mean nothing to a
 * screen reader. Every place that explains a rating uses this.
 */
export const VERDICT_LABEL: Record<Verdict, string> = {
  win: "supports",
  lose: "opposes",
  stalemate: "neither",
};

/**
 * Why a tie could not be settled by the delegates.
 *
 * "Decidable" is a neologism most readers will not have met, and "too dense
 * for this council" blamed the delegates for the point being hard. Each says
 * what happened instead.
 */
export const READING_LABEL: Record<StalemateReading, string> = {
  unsolvable: "no clear answer",
  too_dense: "too tangled to decide",
  decidable: "could be decided",
};

export const STATUS_LABEL: Record<PointStatus, string> = {
  open: "open",
  carried: "carried",
  fell: "fell",
  stalemate: "stalemate",
  withdrawn: "withdrawn",
};
