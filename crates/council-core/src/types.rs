//! Core domain types for the Council chamber.
//!
//! A debate is a *board of points*. Every model in the council may raise points,
//! attack other points, cite evidence, and — crucially — cast a `win` / `lose` /
//! `stalemate` verdict on any point, plus concede one outright. The board is the
//! only source of truth: the models argue, but the board adjudicates.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type DelegateId = String;
pub type EndpointId = String;
pub type PointId = u32;
pub type PollId = u32;
pub type TurnId = u64;

// ---------------------------------------------------------------------------
// Configuration (authored in the UI, handed to the engine)
// ---------------------------------------------------------------------------

/// How an endpoint wants reasoning effort expressed.
///
/// There is no standard here and the shapes are not interchangeable, so this is
/// a setting rather than a guess. The same field name means three different
/// things depending on who is serving the weights:
///
/// - **OpenAI, vLLM, NVIDIA NIM** take `reasoning_effort` at the top level of
///   the request body, alongside `temperature`.
/// - **llama.cpp's `llama-server` has no top-level `reasoning_effort` at all**
///   (ggml-org/llama.cpp#20408). It takes it — and `enable_thinking` — through
///   `chat_template_kwargs`, which is passed straight to the model's Jinja
///   template. Unsloth Desktop and LM Studio are llama.cpp, so this is the shape
///   most local servers need.
/// - SGLang honours `chat_template_kwargs` the same way llama.cpp does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ReasoningWire {
    /// Send nothing and let the server's own default stand.
    ///
    /// The default, and the only safe one to ship: a reasoning model left to
    /// itself may default to a level that is slow, or that fails.
    #[default]
    ServerDefault,
    /// `{"reasoning_effort": "medium"}` beside `temperature`.
    Flat,
    /// `{"chat_template_kwargs": {"reasoning_effort": "medium"}}`.
    ChatTemplate,
}

impl ReasoningWire {
    pub fn label(self) -> &'static str {
        match self {
            Self::ServerDefault => "server default",
            Self::Flat => "top-level field",
            Self::ChatTemplate => "chat template kwargs",
        }
    }

    /// What this mode sends.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ServerDefault => "serverDefault",
            Self::Flat => "flat",
            Self::ChatTemplate => "chatTemplate",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "flat" => Self::Flat,
            "chatTemplate" => Self::ChatTemplate,
            _ => Self::ServerDefault,
        }
    }
}

/// Which API a server speaks.
///
/// Not a capability description — the wire is the same for three of these, and
/// what differs is where the address comes from and how reasoning effort is
/// spelled. Selecting one fills in sensible defaults that remain editable,
/// because a self-hosted proxy in front of any of them is a normal thing to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Provider {
    /// Anything speaking `/v1/chat/completions`: llama.cpp, vLLM, Ollama, LM
    /// Studio, Unsloth, OpenAI itself. The original and the default.
    ///
    /// The wire names are spelled out rather than derived, because serde's
    /// `camelCase` turns this into `openAiCompatible` and `DeepSeek` into
    /// `deepSeek`, while the frontend uses the names the products are actually
    /// written with. `camelCase` is right for `ReasoningWire` — whose variants are
    /// single words — and wrong here, where every variant is a proper noun.
    #[default]
    #[serde(rename = "openaiCompatible")]
    OpenAiCompatible,
    /// Google's OpenAI-compatible surface.
    #[serde(rename = "gemini")]
    Gemini,
    /// OpenAI-compatible; reasoning arrives as `reasoning_content`.
    #[serde(rename = "deepseek")]
    DeepSeek,
    /// OpenAI-compatible, and fronts most other providers besides.
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// Anthropic’s Messages API. This is a separate wire protocol, not another
    /// OpenAI-compatible base URL.
    #[serde(rename = "anthropic")]
    Anthropic,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAI-compatible",
            Self::Gemini => "Google Gemini",
            Self::DeepSeek => "DeepSeek",
            Self::OpenRouter => "OpenRouter",
            Self::Anthropic => "Anthropic Claude",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openaiCompatible",
            Self::Gemini => "gemini",
            Self::DeepSeek => "deepseek",
            Self::OpenRouter => "openrouter",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "gemini" => Self::Gemini,
            "deepseek" => Self::DeepSeek,
            "openrouter" => Self::OpenRouter,
            "anthropic" => Self::Anthropic,
            _ => Self::OpenAiCompatible,
        }
    }

    /// The documented base URL, which a user can still overwrite.
    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "http://127.0.0.1:8888",
            // Google's compatibility layer lives under a versioned prefix that
            // is not `/v1`, which is why it needs stating rather than inferred.
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            Self::DeepSeek => "https://api.deepseek.com",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Anthropic => "https://api.anthropic.com",
        }
    }

    /// How this provider names its reasoning level, if it has one.
    ///
    /// Gemini spells it `thinking_level` inside a `thinking_config` object and
    /// its own names; the OpenAI-compatible default uses `reasoning_effort`.
    pub fn reasoning_mechanism(self) -> ReasoningMechanism {
        match self {
            Self::Gemini => ReasoningMechanism::GeminiThinkingConfig,
            _ => ReasoningMechanism::EffortField,
        }
    }
}

/// Where a reasoning level goes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningMechanism {
    /// `reasoning_effort` beside `temperature`, then the wire shape from
    /// [`ReasoningWire`].
    EffortField,
    /// Gemini's `thinking_config: { thinking_level }`.
    GeminiThinkingConfig,
}

/// An OpenAI-compatible chat-completions server.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct EndpointConfig {
    pub id: EndpointId,
    pub name: String,
    /// Which API this server speaks. Defaults and hints, not a switch: every
    /// value below still applies and can be overridden.
    pub provider: Provider,
    /// Base URL such as `http://127.0.0.1:8000/v1`. A trailing `/chat/completions`
    /// is tolerated and normalised away.
    pub base_url: String,
    pub api_key: Option<String>,
    /// Extra request headers, e.g. `{"X-Title": "council"}`.
    pub headers: BTreeMap<String, String>,
    pub timeout_secs: u64,
    /// When false the engine falls back to asking for an inline JSON block and
    /// parsing it, for servers/models without native tool-calling support.
    pub native_tools: bool,
    /// When false, responses are requested in one piece. Unsloth's Cloudflare
    /// tunnel drops server-sent events, so a tunnelled endpoint needs this off.
    pub stream: bool,
    /// Accept self-signed certificates. Only for a local server you started.
    pub allow_invalid_certs: bool,
    /// Ask the server to report token usage.
    ///
    /// On by default; it is the only honest token count. A streamed response
    /// only carries usage when `stream_options.include_usage` is set, and a
    /// strict server rejects an unrecognised body field outright rather than
    /// ignoring it. When this is on and the server refuses, the request is
    /// retried once without it and the refusal is remembered for the session,
    /// so turning it on costs one wasted round trip at worst — never a failed
    /// debate, and never a retry on every turn.
    pub include_usage: bool,
    /// Where reasoning effort goes on the wire. See [`ReasoningWire`].
    pub reasoning_wire: ReasoningWire,
    /// How hard the model should think, spelled exactly as *this* model spells
    /// it — there is no shared vocabulary, and Qwen3.8 in particular
    /// *rejects* the request rather than ignoring a value it does not know.
    ///
    /// Left unset, nothing is sent. Known sets: Qwen3.5/3.6/3.8 take
    /// `low`/`medium`/`xhigh`; OpenAI's reasoning models take
    /// `low`/`medium`/`high`; DeepSeek adds `max`.
    pub reasoning_effort: Option<String>,
    /// Turn the thinking block off entirely, for the models whose template
    /// gates it behind `enable_thinking`. `None` leaves the server's own
    /// choice, which for the Qwen3 line is on.
    pub thinking: Option<bool>,
}

impl EndpointConfig {
    /// The base with any completions suffix and trailing slash removed, and
    /// `localhost` forced onto IPv4.
    ///
    /// Local servers routinely bind `127.0.0.1` only, and a resolver that
    /// answers `localhost` with `::1` first gets connection-refused from a
    /// client that would otherwise have worked.
    pub fn base(&self) -> String {
        let mut url = self.base_url.trim();
        for suffix in ["/chat/completions", "/completions"] {
            if let Some(stripped) = url.strip_suffix(suffix) {
                url = stripped;
                break;
            }
        }
        let url = url.trim_end_matches('/');
        for scheme in ["http://", "https://"] {
            if let Some(rest) = url.strip_prefix(&format!("{scheme}localhost")) {
                if rest.is_empty() || rest.starts_with(':') || rest.starts_with('/') {
                    return format!("{scheme}127.0.0.1{rest}");
                }
            }
        }
        url.to_string()
    }

    /// True when the base URL already carries a path, e.g. `/v1`.
    fn has_path(&self) -> bool {
        self.base()
            .split_once("://")
            .and_then(|(_, rest)| rest.split_once('/'))
            .map(|(_, p)| !p.is_empty())
            .unwrap_or(false)
    }

    /// Candidate URLs, most likely first. The first is what we normally use;
    /// the second is tried if the server answers 404, because "did you mean
    /// `/v1`?" is the single most common misconfiguration.
    pub fn chat_completions_urls(&self) -> Vec<String> {
        let base = self.base();
        if self.has_path() {
            vec![format!("{base}/chat/completions")]
        } else {
            vec![
                format!("{base}/v1/chat/completions"),
                format!("{base}/chat/completions"),
            ]
        }
    }

    pub fn models_urls(&self) -> Vec<String> {
        let base = self.base();
        if self.has_path() {
            vec![format!("{base}/models")]
        } else {
            vec![format!("{base}/v1/models"), format!("{base}/models")]
        }
    }

    /// Anthropic Messages URLs. Unlike the OpenAI-compatible path, there is no
    /// `/chat/completions` fallback: trying another provider's endpoint shape
    /// would turn a wrong address into a misleading downstream error.
    pub fn anthropic_messages_urls(&self) -> Vec<String> {
        vec![Self::anthropic_path(self.base(), "/v1/messages", "/messages")]
    }

    /// Anthropic model-listing URLs, using the same path preservation rule as
    /// message calls.
    pub fn anthropic_models_urls(&self) -> Vec<String> {
        vec![Self::anthropic_path(self.base(), "/v1/models", "/models")]
    }

    fn anthropic_path(base: String, versioned: &str, unversioned: &str) -> String {
        let base = base.trim_end_matches('/').to_string();
        if base.ends_with(versioned) {
            base
        } else if base.ends_with("/v1") {
            format!("{base}{}", unversioned.trim_start_matches("/v1"))
        } else if base.ends_with(unversioned) {
            base
        } else {
            format!("{base}{versioned}")
        }
    }

    /// The primary URL, for callers that do not need the fallback.
    pub fn chat_completions_url(&self) -> String {
        self.chat_completions_urls()
            .into_iter()
            .next()
            .unwrap_or_default()
    }
}

/// One speaking seat in the council.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DelegateConfig {
    pub id: DelegateId,
    /// Human-facing name, e.g. "Advocate" or a nickname.
    pub label: String,
    /// The `model` string the endpoint expects.
    pub model: String,
    pub endpoint_id: EndpointId,
    /// Hex colour used by the UI and the 3D chamber.
    pub color: String,
    /// The seat's fixed role. The four convening essentials must each be
    /// present exactly once; the Rationale may sit at most once; anything
    /// else argues as a custom seat.
    pub seat_role: SeatRole,
    /// Short role line woven into the system prompt.
    pub role: Option<String>,
    /// Free-form persona / style instructions.
    pub persona: Option<String>,
    pub temperature: Option<f32>,
    /// Taken by the operator rather than a model. The engine waits for this
    /// seat's turn instead of calling an endpoint, so it needs no model and no
    /// endpoint — only on arguing seats; judging and summarizing stay with the
    /// models, and `validate` refuses anything else.
    pub human: bool,
}

/// One operator answer to a parked turn: the speech, the thinking behind it,
/// and the tool calls built with the action picker. Shaped exactly like a
/// model reply so the turn pipeline downstream cannot tell them apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanTurnSubmit {
    pub text: String,
    pub thinking: String,
    pub calls: Vec<crate::tools::RawToolCall>,
}

/// One operator answer to a parked ballot, per kind. The variants carry what
/// each ballot's own return type carries, so a submit maps onto it directly.
///
/// Internally tagged on `kind`, and the adjournment variant is `adjourn` to
/// match the ballot kind it answers — one name per kind across the whole
/// boundary, so a client never has to translate between two vocabularies.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HumanBallot {
    Poll {
        choice: String,
        reason: Option<String>,
        confidence: Option<f64>,
    },
    Review {
        reading: StalemateReading,
        reason: Option<String>,
        vote: Option<Verdict>,
    },
    Adjourn {
        confirm: bool,
        reason: Option<String>,
    },
    Extension {
        grant: bool,
        reason: Option<String>,
    },
}

impl HumanBallot {
    /// Which ballot this answer belongs to. Internal tagging erases the variant
    /// name on the wire, so it is recoverable only here — which is what lets a
    /// mismatched submit be refused by name rather than silently abstained.
    pub fn kind(&self) -> crate::events::BallotKind {
        use crate::events::BallotKind;
        match self {
            Self::Poll { .. } => BallotKind::Poll,
            Self::Review { .. } => BallotKind::Review,
            Self::Adjourn { .. } => BallotKind::Adjourn,
            Self::Extension { .. } => BallotKind::Extension,
        }
    }
}

/// The fixed roles a council is built around. Labels are fixed alongside:
/// Advocate argues, Critic opposes, Arbiter judges, Rationale hunts fallacies,
/// Summarizer lays out each round between rounds and the end result at the
/// end. Custom seats argue with a free-form role line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SeatRole {
    Proponent,
    Opponent,
    Judge,
    /// A second analyst: argues and votes like any other arguing seat, and
    /// sits after the Arbiter in a fresh council. Optional — a council
    /// convenes without one — but at most one may sit.
    Rationale,
    Summarizer,
    /// Any user-added seat. Speaks and votes like the arguing essentials.
    #[default]
    Custom,
}

impl SeatRole {
    /// The role line a fresh seat of this kind carries. Purely a default for
    /// new setups — the UI may refine the wording, never the role itself.
    pub fn blurb(self) -> &'static str {
        match self {
            Self::Proponent => "makes the case",
            Self::Opponent => "steelmans the other side, then attacks it",
            Self::Judge => "votes only on the record",
            Self::Rationale => "finds logical fallacies",
            Self::Summarizer => "lays out each round and the final result; never argues",
            Self::Custom => "argues the motion",
        }
    }

    /// False for the seat that watches: it takes no round turns, casts no
    /// ballots, and moves for nothing.
    pub fn argues(self) -> bool {
        !matches!(self, Self::Summarizer)
    }
}

/// How the council settles fixed-option questions (poll ballots).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DecisionProvider {
    /// Each seat answers with its own model — the current behaviour.
    #[default]
    Seat,
    /// A decision model (TypeSafe's Jev and its kin): typed question in,
    /// choice plus confidence out, no prose generated.
    Jev,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DecisionConfig {
    pub provider: DecisionProvider,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DebateConfig {
    /// The motion put to the council.
    pub motion: String,
    pub extra_instructions: Option<String>,
    /// The record of an unfinished debate being resumed, handed to every delegate.
    ///
    /// Separate from `extra_instructions` so it cannot overwrite the operator's own
    /// standing instructions: that field is saved with the rest of the setup, so a
    /// resume writing into it meant the *next*, unrelated debate was still being
    /// handed the previous one's transcript.
    pub resume_record: Option<String>,
    pub endpoints: Vec<EndpointConfig>,
    pub delegates: Vec<DelegateConfig>,
    pub max_rounds: u32,
    /// When true, the engine stops once every point has been decided.
    pub auto_adjourn: bool,
    /// Whether delegates deliver a closing address before the verdict.
    pub closing_statements: bool,
    /// Total extra rounds the summarizer may ask for across the whole debate.
    /// Each granted extension spends from this budget; the proposal tool is
    /// refused once it is exhausted.
    pub max_extra_rounds: u32,
    /// Who answers poll ballots: the seats, or a decision model.
    pub decision: DecisionConfig,
    /// Whether the arguing seats may look things up on the web.
    ///
    /// Off by default, and deliberately so: this is the only capability in the
    /// app that reaches off the machine. A debate should not acquire one
    /// without the operator asking for it. Only the Advocate, the Critic and
    /// the Rationale ever see the tool -- the Arbiter rules on the record it
    /// is given, and a judge with a browser stops judging the argument and
    /// starts quoting the first page it finds.
    pub allow_web: bool,
    /// How many pages one seat may read in a single turn. A seat that wants
    /// three sources has usually stopped arguing and started compiling.
    #[serde(default = "three")]
    pub max_fetches_per_turn: u32,
    /// How much of a page one seat may read, in characters.
    #[serde(default = "six_thousand")]
    pub max_fetch_chars: usize,
    /// Images attached to the motion for the council to argue over. Each
    /// seat sees them with its opening message; a vision-capable model on
    /// the other end is required for them to mean anything.
    #[serde(default)]
    pub images: Vec<ImageAttachment>,
}

/// One image attached to the motion.
///
/// Carried as a data URL so the engine can forward it to any
/// OpenAI-compatible vision endpoint unchanged. Bytes never reach the
/// library: the UI strips them when it saves, keeping only [`ImageInfo`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachment {
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub data_url: String,
}

/// The library-safe stub: what was attached, without the bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfo {
    pub id: String,
    pub name: String,
}

fn three() -> u32 {
    3
}
fn six_thousand() -> usize {
    6000
}

/// Hand-written so the in-code default and the wire default agree.
///
/// The derive gave `Default::default()` a budget of *zero* reads, while a saved
/// config that predates the field was given three. The two defaults for one
/// setting is a trap: a test that builds a config in Rust silently got a seat
/// that could read one page, and a saved one got three, and only the second
/// looked like the documentation. Everything else here is a zero, which is why
/// the derive went unnoticed for exactly as long as it did.
impl Default for DebateConfig {
    fn default() -> Self {
        Self {
            motion: String::new(),
            extra_instructions: None,
            resume_record: None,
            endpoints: Vec::new(),
            delegates: Vec::new(),
            max_rounds: 0,
            auto_adjourn: false,
            closing_statements: false,
            max_extra_rounds: 0,
            decision: DecisionConfig::default(),
            allow_web: false,
            max_fetches_per_turn: three(),
            max_fetch_chars: six_thousand(),
            images: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// The board
// ---------------------------------------------------------------------------

/// A delegate's position on a single point. This is the crux of the whole app:
/// every model rates every point `win`, `lose` or `stalemate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// The point survives scrutiny — it stands.
    Win,
    /// The point is refuted — it falls.
    Lose,
    /// Genuinely undecidable on the record as it stands.
    Stalemate,
}

impl Verdict {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "win" | "wins" | "upheld" | "carried" | "true" | "sustain" => Some(Self::Win),
            "lose" | "loss" | "lost" | "down" | "refuted" | "falls" | "false" => Some(Self::Lose),
            "stalemate" | "tie" | "draw" | "undecided" | "abstain" | "unproven" => {
                Some(Self::Stalemate)
            }
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Win => "win",
            Self::Lose => "lose",
            Self::Stalemate => "stalemate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PointStatus {
    /// Raised and still undecided — not enough votes on the record.
    #[default]
    Open,
    /// More delegates rate it `win` than `lose`.
    Carried,
    /// More rate it `lose` than `win`.
    Fell,
    /// Ratings cancel out, or the only votes cast are `stalemate`.
    Stalemate,
    /// The delegate who raised it took it back.
    Withdrawn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoteRecord {
    pub delegate: DelegateId,
    pub verdict: Verdict,
    pub reason: Option<String>,
    pub round: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Point {
    pub id: PointId,
    pub text: String,
    pub raised_by: DelegateId,
    pub round: u32,
    pub withdrawn: bool,
    /// Why the raiser withdrew it, if they did.
    pub withdrawn_reason: Option<String>,
    /// Points this one is offered as defeating (`make_point.beats`).
    pub beats: Vec<PointId>,
    pub votes: BTreeMap<DelegateId, VoteRecord>,
    /// The latest concrete objection to this point, per delegate.
    pub rebuttals: Vec<Remark>,
    /// The latest steelman, per delegate.
    pub steelmen: Vec<Remark>,
    /// Evidence filed against/for it.
    pub evidence: Vec<Remark>,
    /// Delegates who have conceded this point, and why.
    pub conceded_by: BTreeMap<DelegateId, String>,
    /// The latest deadlock review, if the point was ever deadlocked enough to
    /// need one. A closed review is the licence to adjourn over a stalemate.
    #[serde(default)]
    pub review: Option<StalemateReview>,
    /// The Arbiter's ruling, which settles the point whatever the tally says.
    #[serde(default)]
    pub ruling: Option<PointRuling>,
}

/// The Arbiter's decision on a point.
///
/// A tally is arithmetic: three seats to one, and the point carries. The
/// Arbiter is not arithmetic, it is the seat that rules, and the thing a judge
/// is *for* is the power to say a point is settled when the counting does not
/// show it. Before this, the Arbiter's only way to move a point was to be one
/// vote among several — real influence, but never decisive, and always
/// outnumbered by two seats that had already spoken.
///
/// The tally is left alone on purpose. A ruling that carried a point scoring
/// minus one should look like exactly what it is — the Arbiter overrode the
/// count — rather than quietly rewriting the count to match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointRuling {
    pub by: DelegateId,
    pub round: u32,
    /// True when the point carries, false when it falls.
    pub carries: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Remark {
    pub by: DelegateId,
    pub round: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tally {
    pub wins: u32,
    pub losses: u32,
    pub stalemates: u32,
    pub abstentions: u32,
    /// `wins - losses`. Positive favours the point.
    pub score: i32,
    pub status: PointStatus,
    /// True once a delegate other than the proponent has rated the point.
    pub contested: bool,
    /// The effective rating of every seat, implicit proponent votes included,
    /// so the UI never has to re-derive the rule.
    pub ratings: BTreeMap<DelegateId, Verdict>,
}

impl Tally {
    pub fn decided(&self) -> bool {
        !matches!(self.status, PointStatus::Open)
    }

    /// Decided *and* someone other than the raiser has weighed in. A point
    /// nobody else has touched is not settled just because its author likes it.
    pub fn settled(&self) -> bool {
        self.decided() && self.contested
    }
}

impl Point {
    /// The rating this point carries for `id`, if any — explicit or implicit.
    ///
    /// A proponent implicitly rates their own point `win` until they vote
    /// otherwise, concede it or withdraw it. Without this, a three-seat council
    /// could never carry anything, because one vote for and one against cancels
    /// out before the author has even spoken.
    pub fn rating_of(&self, id: &str) -> Option<Verdict> {
        if let Some(v) = self.votes.get(id) {
            return Some(v.verdict);
        }
        if id == self.raised_by && !self.withdrawn {
            return Some(Verdict::Win);
        }
        None
    }

    pub fn tally(&self, roster: &[DelegateId]) -> Tally {
        let mut t = Tally::default();
        for id in roster {
            match self.rating_of(id) {
                Some(Verdict::Win) => {
                    t.wins += 1;
                    t.ratings.insert(id.clone(), Verdict::Win);
                }
                Some(Verdict::Lose) => {
                    t.losses += 1;
                    t.ratings.insert(id.clone(), Verdict::Lose);
                }
                Some(Verdict::Stalemate) => {
                    t.stalemates += 1;
                    t.ratings.insert(id.clone(), Verdict::Stalemate);
                }
                None => t.abstentions += 1,
            }
        }
        t.contested = roster
            .iter()
            .any(|id| id != &self.raised_by && self.votes.contains_key(id));
        t.score = t.wins as i32 - t.losses as i32;
        // A ruling settles the point outright, and outranks the arithmetic
        // below. Withdrawn still wins: a retracted claim is not on the board to
        // be ruled on.
        t.status = match &self.ruling {
            Some(r) if !self.withdrawn => {
                if r.carries {
                    PointStatus::Carried
                } else {
                    PointStatus::Fell
                }
            }
            _ => resolve_status(self.withdrawn, &t),
        };
        if t.status == PointStatus::Withdrawn {
            // A retracted point has no standing at all; a leftover score would
            // just be confusing.
            t.score = 0;
        }
        t
    }
}

/// Pure scoring rule, isolated so it can be unit tested and reasoned about.
///
/// A split that cancels out is a stalemate even when nobody cast an explicit
/// `stalemate` vote — the council deadlocked either way. `Open` therefore means
/// the point is genuinely untallied, which is the only state worth more
/// argument.
pub fn resolve_status(withdrawn: bool, t: &Tally) -> PointStatus {
    if withdrawn {
        return PointStatus::Withdrawn;
    }
    match t.score.cmp(&0) {
        std::cmp::Ordering::Greater => PointStatus::Carried,
        std::cmp::Ordering::Less => PointStatus::Fell,
        std::cmp::Ordering::Equal if t.wins + t.losses + t.stalemates > 0 => {
            PointStatus::Stalemate
        }
        std::cmp::Ordering::Equal => PointStatus::Open,
    }
}

// ---------------------------------------------------------------------------
// Deadlock reviews
// ---------------------------------------------------------------------------

/// A delegate's ruling on *why* a point is deadlocked. Closing on a stalemate
/// requires this vote first: the council must say whether the matter is
/// unsolvable, too dense to be decided here, or actually decidable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StalemateReading {
    /// Undecidable in principle — no record could settle it.
    #[serde(rename = "unsolvable")]
    Unsolvable,
    /// Genuinely too dense for this council, here, now.
    #[serde(rename = "too_dense")]
    TooDense,
    /// The deadlock is a mistake — the point should be revoted, not buried.
    #[serde(rename = "decidable")]
    Decidable,
}

impl StalemateReading {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "unsolvable" | "undecidable" | "impossible" | "unresolvable" => {
                Some(Self::Unsolvable)
            }
            "too_dense" | "too dense" | "dense" | "too hard" | "too big" | "here" => {
                Some(Self::TooDense)
            }
            "decidable" | "decided" | "decide" | "clear" | "resolvable" | "mistake" => {
                Some(Self::Decidable)
            }
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsolvable => "unsolvable",
            Self::TooDense => "too dense for this council",
            Self::Decidable => "decidable",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StalemateBallot {
    pub delegate: DelegateId,
    pub reading: StalemateReading,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StalemateReview {
    pub point: PointId,
    pub opened_by: DelegateId,
    pub round: u32,
    pub ballots: Vec<StalemateBallot>,
    pub closed: bool,
    pub outcome: Option<StalemateReading>,
}

impl StalemateReview {
    /// Reading -> ballot count, in a fixed order for the UI.
    pub fn result(&self) -> Vec<(String, u32)> {
        const NAMES: [(&str, StalemateReading); 3] = [
            ("unsolvable", StalemateReading::Unsolvable),
            ("too_dense", StalemateReading::TooDense),
            ("decidable", StalemateReading::Decidable),
        ];
        NAMES
            .iter()
            .map(|(name, reading)| {
                let n = self
                    .ballots
                    .iter()
                    .filter(|b| b.reading == *reading)
                    .count() as u32;
                (name.to_string(), n)
            })
            .collect()
    }

    /// A review rules only on a strict majority of the readings cast. Splits
    /// are reported as inconclusive rather than fudged.
    pub fn outcome_of(&self) -> Option<StalemateReading> {
        let counts = self.result();
        let cast: u32 = counts.iter().map(|(_, n)| n).sum();
        if cast == 0 {
            return None;
        }
        let (top, top_n) = counts
            .iter()
            .max_by_key(|(_, n)| *n)
            .cloned()
            .expect("non-empty readings");
        if top_n * 2 <= cast {
            return None;
        }
        match top.as_str() {
            "unsolvable" => Some(StalemateReading::Unsolvable),
            "too_dense" => Some(StalemateReading::TooDense),
            _ => Some(StalemateReading::Decidable),
        }
    }
}

// ---------------------------------------------------------------------------
// Polls
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PollBallot {
    pub delegate: DelegateId,
    pub choice: String,
    pub reason: Option<String>,
    /// Calibrated confidence, when a decision model cast this ballot.
    #[serde(default)]
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Poll {
    pub id: PollId,
    pub question: String,
    pub options: Vec<String>,
    pub opened_by: DelegateId,
    pub round: u32,
    pub ballots: Vec<PollBallot>,
    pub closed: bool,
}

impl Poll {
    /// Option -> vote count, in the order the options were offered.
    pub fn result(&self) -> Vec<(String, u32)> {
        self.options
            .iter()
            .map(|opt| {
                let n = self
                    .ballots
                    .iter()
                    .filter(|b| b.choice.trim().eq_ignore_ascii_case(opt))
                    .count() as u32;
                (opt.clone(), n)
            })
            .collect()
    }

    /// A poll passes only on a strict majority of the votes actually cast.
    /// Ties and splits are reported as inconclusive rather than fudged.
    pub fn outcome(&self) -> Option<String> {
        let counts = self.result();
        let cast: u32 = counts.iter().map(|(_, n)| n).sum();
        if cast == 0 {
            return None;
        }
        let (top, top_n) = counts
            .iter()
            .max_by_key(|(_, n)| *n)
            .cloned()
            .expect("non-empty options");
        if top_n * 2 > cast {
            Some(top)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Speech
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnPhase {
    Queued,
    Speaking,
    Waiting,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Speech {
    pub id: TurnId,
    pub delegate: DelegateId,
    pub round: u32,
    pub text: String,
    /// The thinking behind the speech, when the server exposed it. For
    /// reading after the fact, not for arguing with.
    #[serde(default)]
    pub reasoning: String,
    /// Ordered log of what the delegate actually *did*, not just said.
    pub actions: Vec<String>,
    pub phase: TurnPhase,
}

// ---------------------------------------------------------------------------
// Verdict
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CarriedPoint {
    pub id: PointId,
    pub text: String,
    pub raised_by: DelegateId,
    pub score: i32,
    pub wins: u32,
    pub losses: u32,
    pub stalemates: u32,
    /// How the council ruled the deadlock, when it did.
    #[serde(default)]
    pub ruling: Option<StalemateReading>,
}

/// The direct answer to the motion, as the summarizer puts it.
///
/// Split in two because the two halves have different authority: `answer` and
/// `lean` are the summarizer's reading of the room, while `agreed` is the
/// engine's — a seat cannot declare a consensus the board did not reach.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conclusion {
    /// True only when the board actually settled. The engine overrides the
    /// summarizer's claim when open or deadlocked points remain.
    pub agreed: bool,
    /// The answer itself, in a sentence or two. Empty when nothing was settled.
    #[serde(default)]
    pub answer: String,
    /// Which way the evidence pointed even without agreement.
    #[serde(default)]
    pub lean: Option<String>,
    /// The tally behind that lean, rendered for the sidebar.
    #[serde(default)]
    pub tally: String,
}

/// How the debate ended. The UI used to call every ending "adjourned",
/// which made an early stop indistinguishable from ten full rounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndedHow {
    /// A confirmed adjournment motion carried.
    Adjourned,
    /// Auto-adjourn: every point settled before the rounds ran out.
    Settled,
    /// The Summarizer read the record as converged and ended it there.
    ///
    /// Distinct from [`Self::Settled`] because that one is a count — every point
    /// decided and contested — and this one is a judgement, which can stop a
    /// debate with a point still deadlocked if what has carried outweighs it.
    CalledSettled,
    /// All planned rounds were spoken.
    #[default]
    Complete,
    /// The operator stopped it.
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebateVerdict {
    pub motion: String,
    pub rounds: u32,
    #[serde(default)]
    pub ended_how: EndedHow,
    /// The Summarizer's own reason, when it was the one who ended it.
    pub settled_reason: Option<String>,
    pub carried: Vec<CarriedPoint>,
    pub fell: Vec<CarriedPoint>,
    pub stalemate: Vec<CarriedPoint>,
    pub withdrawn: Vec<CarriedPoint>,
    /// The point with the strongest positive score, if any.
    pub crux: Option<CarriedPoint>,
    pub summary: String,
    pub adjourned_by: Option<DelegateId>,
    /// The summarizer's end result, written after the last round. This is what
    /// the sidebar shows; `summary` above stays the engine's own tally line.
    #[serde(default)]
    pub final_summary: Option<String>,
    /// The direct answer to the motion, when the summarizer gave one.
    #[serde(default)]
    pub conclusion: Option<Conclusion>,
}

#[cfg(test)]
mod default_tests {
    use super::*;

    /// The provider names cross the Tauri boundary as strings, and the frontend
    /// spells them the way the products are written. serde's `camelCase` would
    /// turn `DeepSeek` into `deepSeek` and `OpenRouter` into `openRouter`, which
    /// the browser never sends — and the failure is a rejected command with no
    /// hint which field was wrong, so it is worth pinning here.
    #[test]
    fn provider_names_round_trip_the_way_the_frontend_spells_them() {
        let spellings = [
            (Provider::OpenAiCompatible, "openaiCompatible"),
            (Provider::Gemini, "gemini"),
            (Provider::DeepSeek, "deepseek"),
            (Provider::OpenRouter, "openrouter"),
            (Provider::Anthropic, "anthropic"),
        ];
        for (variant, wire) in spellings {
            assert_eq!(variant.as_str(), wire, "as_str disagrees with serde");
            assert_eq!(Provider::parse(wire), variant, "parse disagrees for {wire}");
            // And through serde itself, which is what the bridge actually uses.
            let json = serde_json::to_string(&variant).expect("serialises");
            assert_eq!(json, format!("\"{wire}\""));
            let back: Provider = serde_json::from_str(&json).expect("deserialises");
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn anthropic_routes_keep_their_own_versioned_paths() {
        let endpoint = EndpointConfig {
            provider: Provider::Anthropic,
            base_url: Provider::Anthropic.default_base_url().into(),
            ..EndpointConfig::default()
        };
        assert_eq!(
            endpoint.anthropic_messages_urls(),
            vec!["https://api.anthropic.com/v1/messages".to_string()]
        );
        assert_eq!(
            endpoint.anthropic_models_urls(),
            vec!["https://api.anthropic.com/v1/models".to_string()]
        );

        let proxy = EndpointConfig {
            provider: Provider::Anthropic,
            base_url: "https://proxy.example/anthropic".into(),
            ..EndpointConfig::default()
        };
        assert_eq!(
            proxy.anthropic_messages_urls(),
            vec!["https://proxy.example/anthropic/v1/messages".to_string()]
        );
    }

    /// A saved config from before providers existed has no `provider` field, and
    /// every endpoint in it was OpenAI-compatible by construction — it was the
    /// only wire the engine spoke.
    #[test]
    fn an_endpoint_without_a_provider_loads_as_openai_compatible() {
        let wire: EndpointConfig = serde_json::from_str(
            r#"{"id":"a","name":"Local","baseUrl":"http://127.0.0.1:8000/v1"}"#,
        )
        .expect("an endpoint saved before providers existed");
        assert_eq!(wire.provider, Provider::OpenAiCompatible);
        assert_eq!(Provider::default(), Provider::OpenAiCompatible);
    }

    #[test]
    fn the_reading_budget_defaults_the_same_both_ways() {
        // The derive used to give `Default::default()` a budget of zero reads
        // while a saved config got three. A test that built a config in Rust
        // then got a seat that could read exactly one page and called it a bug
        // in the loop.
        let direct = DebateConfig::default();
        assert_eq!(direct.max_fetches_per_turn, 3);
        assert_eq!(direct.max_fetch_chars, 6000);
        assert!(!direct.allow_web);

        // And the same after a round trip through the wire, with the fields
        // absent exactly as an older saved config would have them.
        let wire: DebateConfig =
            serde_json::from_str(r#"{"motion":"m","delegates":[],"endpoints":[]}"#)
                .expect("a config without the reading fields");
        assert_eq!(wire.max_fetches_per_turn, direct.max_fetches_per_turn);
        assert_eq!(wire.max_fetch_chars, direct.max_fetch_chars);
        assert_eq!(wire.allow_web, direct.allow_web);
    }

    #[test]
    fn web_access_is_never_inherited_from_an_older_setup() {
        let wire: DebateConfig =
            serde_json::from_str(r#"{"motion":"m","delegates":[],"endpoints":[]}"#)
                .expect("a config without the reading fields");
        assert!(!wire.allow_web, "reaching the network must be opt-in");
    }
}
