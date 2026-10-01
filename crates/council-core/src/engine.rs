//! The debate engine.
//!
//! One turn at a time, in seat order, for a fixed number of rounds. Each turn
//! the delegate sees the whole board, what it has personally said before, and
//! what it has missed; then it speaks and acts. The engine adjudicates every
//! action itself — the models never get to decide what counts.
//!
//! Cancellation is cooperative: checked between turns and before every call.

use futures_util::FutureExt;

use crate::client::{ChatBackend, ChatChunk, ChatMessage, ChatRequest, ChatResponse, LlmError};
use crate::decide::{ClassifyRequest, Decision, DecisionBackend, JevBackend};
use crate::events::{DelegateBrief, EngineEvent, NoticeTone};
use crate::prompt;
use crate::tools::{self, Action, RawToolCall};
use crate::types::*;
use crate::web::{untrusted, Retrieved, Source, WebBackend, WebError, HttpWeb};
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Marks a discarded partial stream. Chosen because no model emits it: it is
/// not printable, not whitespace, and not a character any tool payload carries.
pub const RESET_MARKER: &str = "\u{0}reset\u{0}";

/// Where events go. The Tauri layer forwards to the webview; tests collect.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: EngineEvent);
}

impl<F> EventSink for F
where
    F: Fn(EngineEvent) + Send + Sync + 'static,
{
    fn emit(&self, event: EngineEvent) {
        self(event)
    }
}

/// One line of the rolling public record.
#[derive(Debug, Clone)]
struct RecordEntry {
    delegate: String,
    round: u32,
    text: String,
}

struct Board {
    config: DebateConfig,
    ids: Vec<DelegateId>,
    points: Vec<Point>,
    next_point: PointId,
    polls: Vec<Poll>,
    next_poll: PollId,
    speeches: Vec<Speech>,
    next_turn: TurnId,
    round: u32,
    record: Vec<RecordEntry>,
    history: BTreeMap<DelegateId, Vec<ChatMessage>>,
    seen: BTreeMap<DelegateId, usize>,
    pressed: BTreeMap<DelegateId, Vec<String>>,
    adjourn: Option<DelegateId>,
    adjourn_reason: Option<String>,
    /// Round number from which adjournment may next be moved. A motion the
    /// council votes down stays down for the rest of its round.
    adjourn_cooldown_until: u32,
    open_poll: Option<PollId>,
    /// Extra rounds granted so far, spent from `max_extra_rounds`.
    extra_granted: u32,
    /// Set when the summarizer has declared the board settled, with its reason.
    ///
    /// The engine can stop a debate on its own — every point decided and
    /// contested — but that is a count, and it cannot weigh one deadlocked point
    /// against five decisively carried ones. The seat that has read every round
    /// can, and when it speaks the ending says so in its own words.
    settled_by: Option<(DelegateId, String)>,
}

impl Board {
    fn new(config: DebateConfig) -> Self {
        let ids = config
            .delegates
            .iter()
            .map(|d| d.id.clone())
            .collect::<Vec<_>>();
        Self {
            ids,
            config,
            points: Vec::new(),
            next_point: 1,
            polls: Vec::new(),
            next_poll: 1,
            speeches: Vec::new(),
            next_turn: 1,
            round: 0,
            record: Vec::new(),
            history: BTreeMap::new(),
            seen: BTreeMap::new(),
            pressed: BTreeMap::new(),
            adjourn: None,
            adjourn_reason: None,
            adjourn_cooldown_until: 0,
            open_poll: None,
            extra_granted: 0,
            settled_by: None,
        }
    }

    fn delegate(&self, id: &str) -> Option<&DelegateConfig> {
        self.config.delegates.iter().find(|d| d.id == id)
    }

    fn endpoint_for(&self, id: &str) -> Option<EndpointConfig> {
        let d = self.delegate(id)?;
        self.config
            .endpoints
            .iter()
            .find(|e| e.id == d.endpoint_id)
            .cloned()
    }

    fn note(&mut self, delegate: &str, round: u32, text: String) {
        if !text.trim().is_empty() {
            self.record.push(RecordEntry {
                delegate: delegate.to_string(),
                round,
                text,
            });
        }
    }

    /// Everything `id` has not yet been told about, oldest first.
    fn unseen(&self, id: &str) -> Vec<&RecordEntry> {
        let from = self.seen.get(id).copied().unwrap_or(0);
        self.record.iter().skip(from).collect()
    }

    fn point(&self, id: PointId) -> Option<&Point> {
        self.points.iter().find(|p| p.id == id)
    }

    fn summarizer_id(&self) -> Option<&DelegateId> {
        self.config
            .delegates
            .iter()
            .find(|d| d.seat_role == SeatRole::Summarizer)
            .map(|d| &d.id)
    }

    /// The seats that argue: everyone except the summarizer, in seat order.
    fn speakers(&self) -> Vec<DelegateConfig> {
        self.config
            .delegates
            .iter()
            .filter(|d| d.seat_role.argues())
            .cloned()
            .collect()
    }

    /// Everyone who may cast a ballot — poll, review, or adjournment. The
    /// summarizer watches; it never votes.
    fn voting_ids(&self) -> Vec<DelegateId> {
        match self.summarizer_id() {
            Some(s) => self.ids.iter().filter(|i| *i != s).cloned().collect(),
            None => self.ids.clone(),
        }
    }

    /// Extra rounds still unspent from the configured budget.
    fn extra_left(&self) -> u32 {
        self.config
            .max_extra_rounds
            .saturating_sub(self.extra_granted)
    }
}

/// Coalesces chunk callbacks so the UI gets ~40 updates a second, not ~200.
/// One pump carries speech, another carries thinking; they flush as different
/// events so the UI can render them differently.
struct Pump<'a> {
    sink: &'a dyn EventSink,
    turn: TurnId,
    thought: bool,
    buf: Mutex<String>,
    last: Mutex<Instant>,
}

impl<'a> Pump<'a> {
    fn content(sink: &'a dyn EventSink, turn: TurnId) -> Self {
        Self::of(sink, turn, false)
    }

    fn thought(sink: &'a dyn EventSink, turn: TurnId) -> Self {
        Self::of(sink, turn, true)
    }

    fn of(sink: &'a dyn EventSink, turn: TurnId, thought: bool) -> Self {
        Self {
            sink,
            turn,
            thought,
            buf: Mutex::new(String::new()),
            last: Mutex::new(Instant::now() - Duration::from_secs(1)),
        }
    }

    fn push(&self, chunk: &str) {
        if let Ok(mut b) = self.buf.lock() {
            b.push_str(chunk);
        }
        self.flush(false);
    }

    /// Drop what has been streamed so far, on the record as well as on screen:
    /// a request retried in another mode must not read as two overlapping ones.
    fn reset(&self) {
        if let Ok(mut b) = self.buf.lock() {
            b.clear();
        }
        self.sink.emit(EngineEvent::Token {
            turn: self.turn,
            text: RESET_MARKER.to_string(),
        });
    }

    fn flush(&self, force: bool) {
        let Ok(mut b) = self.buf.lock() else { return };
        if b.is_empty() {
            return;
        }
        if !force {
            let Ok(mut last) = self.last.lock() else { return };
            if last.elapsed() < Duration::from_millis(40) && b.chars().count() < 48 {
                return;
            }
            *last = Instant::now();
        }
        let text = std::mem::take(&mut *b);
        if self.thought {
            self.sink.emit(EngineEvent::Reasoning {
                turn: self.turn,
                text,
            });
        } else {
            self.sink.emit(EngineEvent::Token {
                turn: self.turn,
                text,
            });
        }
    }
}

/// What a raced request resolved to. Generic over the payload so a speaking
/// turn and a decision-model classification share one skip/heartbeat path.
enum Outcome<T> {
    Done(Result<T, LlmError>),
    Skipped,
}

pub struct Engine<B: ChatBackend> {
    backend: Arc<B>,
    sink: Arc<dyn EventSink>,
    board: Mutex<Board>,
    cancel: Arc<AtomicBool>,
    running: Mutex<bool>,
    /// Aborts the turn currently in flight, if any.
    skip_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    /// Answers poll ballots without burning a turn per voter. `None` means
    /// each seat decides with its own model. Rebuilt from the config on every
    /// run, unless a test put one here first.
    decider: Mutex<Option<Arc<dyn DecisionBackend>>>,
    /// Set by [`Engine::set_decider`]: a test or embedder supplied its own,
    /// so the config must not overwrite (or clear) it.
    decider_pinned: AtomicBool,
    /// A summarizer proposal waiting on the operator's answer, if any.
    extension_pending: Mutex<Option<ExtensionPending>>,
    /// The operator's answer to the waiting proposal: approved, and how many
    /// rounds. Taken exactly once by the suspended intermission.
    extension_answer: Mutex<Option<(bool, u32)>>,
    /// Wakes the suspended intermission when the operator answers — or when
    /// the debate is stopped while it waits.
    extension_notify: tokio::sync::Notify,
    /// The reference shelf, behind a trait so a debate can be tested without a
    /// network exactly as the models are. Built on first use, because a client
    /// that will not build is not a reason to refuse to run a debate whose web
    /// access is switched off anyway.
    web: Mutex<Option<Arc<dyn WebBackend>>>,
    /// Endpoint ids that already proved they reject `stream_options`: the
    /// usage request is dropped for them up front, so every turn on such a
    /// server does not pay a 400 plus a retry — and a notice — forever.
    usage_unsupported: Mutex<std::collections::BTreeSet<String>>,
}

/// How long a summarizer proposal may go unanswered before it is declined.
const EXTENSION_WAIT_SECS: u64 = 300;

/// A turn's pre-turn state, kept so a skip or a failure can undo what it wrote.
struct TurnRollback {
    history: Vec<ChatMessage>,
    pressed: Vec<String>,
    seen: usize,
}

/// Whether a reply is the documented empty-answer failure rather than a silence.
///
/// Reasoning models at their most aggressive setting sometimes return a clean
/// `stop` having thought for thousands of tokens and written nothing at all.
/// Nothing is truncated, nothing errored, and no client can tell it from a real
/// answer it simply gave — but the trace is present and the answer is not, which
/// is enough to recognise.
///
/// `finish_reason` is checked because `length` means the ordinary thing: the
/// reply ran out of room, and asking again with the same budget will not help.
fn answered_with_nothing(resp: &ChatResponse) -> bool {
    resp.finish_reason.as_deref() == Some("stop")
        && resp.content.trim().is_empty()
        && !resp.reasoning.trim().is_empty()
}

/// Whether a failed request is worth sending again.
///
/// Split on how the attempt *ended*, not only on what it returned. A request
/// that came back in a second — connection reset, refused, 502 — is a blip and
/// should be tried again, because the seat keeps its floor and the debate keeps
/// its place. A request that consumed most of the endpoint's budget and then
/// gave up is this endpoint being too slow, and sending it again costs the same
/// wait to learn the same thing.
///
/// This matters because speaking turns are deliberately uncapped: the timeout is
/// the only ceiling on a model that will not stop generating. Multiplying it
/// means one stuck seat can hold the council for three times as long as it used
/// to, which is exactly what a retry is supposed to prevent.
fn worth_retrying(transient: bool, exhausted_budget: bool, attempt: u32, max: u32) -> bool {
    transient && !exhausted_budget && attempt < max
}

/// How much of a seat's own past turn goes back into its next one.
///
/// A turn is uncapped on purpose — a cut-off mid-argument is worse than a long
/// one — and that makes the *history* unbounded, because every turn's whole
/// speech was kept in full. Eight retained nine-thousand-token speeches is a
/// seventy-thousand-token prompt, which is what the model was being handed when
/// it stalled: prefill took four seconds and then nothing came.
///
/// The transcript keeps everything, and so does the export. This is working
/// memory, not an archive: enough to recognise what you argued and where you
/// landed, which is all a seat needs to be coherent next turn.
const HISTORY_HEAD: usize = 400;
const HISTORY_TAIL: usize = 600;

fn clip_for_memory(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let keep = HISTORY_HEAD + HISTORY_TAIL;
    if chars.len() <= keep + 40 {
        return text.to_string();
    }
    let head: String = chars[..HISTORY_HEAD].iter().collect();
    let tail: String = chars[chars.len() - HISTORY_TAIL..].iter().collect();
    format!(
        "{head}\n[...{} characters of this turn are in the transcript but not \
         here...]\n{tail}",
        chars.len() - keep
    )
}

/// Drop every per-turn brief except the newest.
///
/// Each turn's brief is a full rendering of the whole board, and the board is
/// regenerated every turn — so keeping three of them means sending the model
/// three renderings, the two older ones of which it has already read and that
/// the newest one contradicts. It is the largest single thing in the prompt and
/// it grows along the debate, because each snapshot is bigger than the last: by
/// the last round a seat was carrying a quarter of its prompt as copies of a
/// board it could already see.
///
/// The seat's own words are all kept. Only the redundant state goes.
fn prune_superseded_briefs(history: &mut Vec<ChatMessage>) {
    let Some(newest) = history.iter().rposition(prompt::is_turn_brief) else {
        return;
    };
    let mut kept = Vec::with_capacity(history.len());
    for (i, m) in history.drain(..).enumerate() {
        if i != newest && prompt::is_turn_brief(&m) {
            continue;
        }
        kept.push(m);
    }
    *history = kept;
}

/// How many times a turn may go back to the model after a lookup.
///
/// A second read almost always means a search followed by the article it found,
/// which is the sequence worth supporting. Past that the turn is a model talking
/// to itself, and the words said in between are lost anyway.
const CONTINUATIONS: usize = 2;

/// How many record entries a seat's feed carries. Older entries are dropped
/// from the turn and the seat is told how many it is missing, rather than being
/// left to believe it has seen everything.
const RECORD_TAIL: usize = 40;

/// What came of a request for more rounds.
///
/// Typed rather than inferred from the action-log line: the line is prose meant
/// for a reader, and it grew a "granted by the council" tail — at which point
/// the caller's `ends_with("granted")` quietly stopped matching and the debate
/// declined to resume after the council had already granted it. A string was
/// never a safe place to keep a control decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionOutcome {
    Granted {
        added: u32,
        /// Who decided: the operator, or the council.
        source: &'static str,
    },
    Declined {
        source: &'static str,
    },
}

/// A summarizer proposal waiting on the operator. Only the count matters
/// here — the round and reason already went out on the proposal event.
#[derive(Debug, Clone)]
struct ExtensionPending {
    rounds: u32,
}

impl<B: ChatBackend> Engine<B> {
    pub fn new(backend: B, sink: Arc<dyn EventSink>) -> Self {
        Self {
            backend: Arc::new(backend),
            sink,
            board: Mutex::new(Board::new(DebateConfig::default())),
            cancel: Arc::new(AtomicBool::new(false)),
            running: Mutex::new(false),
            skip_tx: Mutex::new(None),
            decider: Mutex::new(None),
            decider_pinned: AtomicBool::new(false),
            extension_pending: Mutex::new(None),
            extension_answer: Mutex::new(None),
            extension_notify: tokio::sync::Notify::new(),
            web: Mutex::new(None),
            usage_unsupported: Mutex::new(std::collections::BTreeSet::new()),
        }
    }

    /// Supply the reference shelf. Tests use this to keep off the network; an
    /// embedder with a cache or a rate limit would use it for that.
    pub fn set_web(&self, web: Arc<dyn WebBackend>) {
        *self.web.lock().expect("web") = Some(web);
    }

    // -- the reference shelf -------------------------------------------------

    /// May this seat read the web?
    ///
    /// Only the seats that argue, and only when the operator has said so. The
    /// Arbiter is excluded on purpose: a judge who can look things up stops
    /// ruling on the record and starts ruling on the web, and the record is
    /// the only thing the verdict is supposed to be made of. The Summarizer
    /// summarizes and does not argue, so a page in its hands is a page in the
    /// final answer.
    fn seat_may_read(b: &Board, id: &str) -> bool {
        if !b.config.allow_web {
            return false;
        }
        matches!(
            b.delegate(id).map(|d| d.seat_role),
            Some(SeatRole::Proponent)
                | Some(SeatRole::Opponent)
                | Some(SeatRole::Rationale)
                | Some(SeatRole::Custom)
        )
    }

    /// Run a seat's reading calls, then hand it what it read and let it act.
    ///
    /// Every other tool in this engine mutates the board and ends there. These
    /// two answer the *model*, which takes a round trip the turn had not budgeted
    /// for: the seat asks, something happens on the network, and the seat has to
    /// see the answer before it can argue from it. So a fetch suspends the turn,
    /// the page comes back inside `<untrusted_data>`, and the seat replies again.
    ///
    /// Returns the reply to show, the reply's own thinking, the board actions to
    /// apply, and a line per page read.
    #[allow(clippy::too_many_arguments)]
    async fn read_the_web(
        &self,
        id: &str,
        round: u32,
        turn: TurnId,
        text: String,
        thinking: String,
        mut calls: Vec<RawToolCall>,
        req: &ChatRequest,
        tools: &[serde_json::Value],
        native: bool,
    ) -> (String, String, Vec<RawToolCall>, Vec<String>) {
        let mut log: Vec<String> = Vec::new();
        let may_read = {
            let b = self.board.lock().expect("board");
            Self::seat_may_read(&b, id)
        };
        if !may_read {
            // Left in place on purpose: `apply_actions` will refuse them by name,
            // which is the honest record. Inventing a tool and having it vanish
            // teaches a model nothing.
            return (text, thinking, calls, log);
        }

        let budget = {
            let b = self.board.lock().expect("board");
            b.config.max_fetches_per_turn.max(1) as usize
        };
        let backend = match self.web_backend() {
            Ok(w) => w,
            Err(e) => {
                self.sink.emit(EngineEvent::Notice {
                    text: format!("{id} cannot read the web: {e}"),
                    tone: NoticeTone::Warn,
                });
                return (text, thinking, calls, log);
            }
        };

        // Board actions said before the first read. Held, not applied: a seat
        // that stopped to look something up did so *because* it wanted to know
        // before committing. If the reading turn produces nothing, they are
        // used anyway, so a `make_point` is never lost to a lookup.
        let mut held: Vec<RawToolCall> = calls.iter().filter(|c| !tools::is_web(&c.name)).cloned().collect();
        let mut text = text;
        let mut thinking = thinking;
        let mut spent = 0usize;

        for pass in 0..CONTINUATIONS {
            if self.cancelled() {
                break;
            }
            let reads: Vec<RawToolCall> = calls
                .iter()
                .filter(|c| tools::is_web(&c.name))
                .cloned()
                .collect();
            if reads.is_empty() {
                break;
            }
            let left = budget.saturating_sub(spent);
            if left == 0 {
                self.sink.emit(EngineEvent::Notice {
                    text: format!(
                        "{id} asked to read {} more pages but the turn's budget is spent",
                        reads.len()
                    ),
                    tone: NoticeTone::Warn,
                });
                break;
            }
            if reads.len() > left {
                self.sink.emit(EngineEvent::Notice {
                    text: format!(
                        "{id} asked to read {} pages; this turn allows {left}",
                        reads.len()
                    ),
                    tone: NoticeTone::Info,
                });
            }

            let mut notes = String::new();
            for read in reads.iter().take(left) {
                self.activity(turn, id, "reading the reference shelf");
                spent += 1;
                if self.cancelled() {
                    break;
                }
                match self.read_one(turn, id, backend.as_ref(), read).await {
                    Ok((line, got)) => {
                        log.push(line);
                        notes.push_str(&got);
                    }
                    Err(why) => {
                        // A failed lookup is information too, and saying so beats
                        // silence: the seat can carry on without it, but only if
                        // it is told the page was not there.
                        log.push(format!("{id} read failed — {why}"));
                        notes.push_str(&format!("\nNOT FOUND: {why}\n"));
                    }
                }
            }
            if notes.trim().is_empty() {
                break;
            }

            // Naming what is left, rather than forbidding another read: the
            // natural sequence is a search and then the article it found, and a
            // blanket "do not read again" steers a model away from its own
            // results while the budget still has room for them.
            //
            // Capped at what the loop can actually deliver. The budget is the
            // ceiling on pages; the number of round trips is separate and
            // smaller, and a prompt that promises the budget and delivers less
            // is worse than one that says plainly how many turns there are.
            let remaining = budget
                .saturating_sub(spent)
                .min(CONTINUATIONS.saturating_sub(pass));
            let mut user = format!(
                "Here is what you asked to read.\n\n{notes}\n\n\
                 Now argue from it: say your position and act on the board in this same reply."
            );
            if remaining > 0 {
                user.push_str(&format!(
                    "\n\nYou may read {} more page{} this turn. If this was a search and \
                     you want the article itself, fetch one of the titles it returned — \
                     one article, not all of them.",
                    remaining,
                    if remaining == 1 { "" } else { "s" }
                ));
            }
            // Clear the note-to-self *before* the continuation streams. Reset
            // afterwards wiped the reply that had just arrived, so the live text
            // appeared and then vanished a moment before the real speech landed.
            Pump::content(self.sink.as_ref(), turn).reset();
            match self
                .continue_turn(id, round, turn, &text, user, req, tools, native)
                .await
            {
                Some((t, th, c)) => {
                    text = t;
                    thinking = th;
                    calls = c;
                    let board_now: Vec<RawToolCall> =
                        calls.iter().filter(|c| !tools::is_web(&c.name)).cloned().collect();
                    if board_now.is_empty() && !held.is_empty() {
                        // Nothing new to do, so the actions said before the
                        // reading stand after all.
                        break;
                    }
                    held = board_now;
                }
                None => break,
            }
        }

        let board_calls: Vec<RawToolCall> = calls
            .iter()
            .filter(|c| !tools::is_web(&c.name))
            .cloned()
            .collect();
        // Whatever the seat said on its way to the shelf, the record shows what
        // it did *after* reading; with nothing after, the earlier calls stand.
        let final_calls = if board_calls.is_empty() { held } else { board_calls };
        (text, thinking, final_calls, log)
    }

    /// One reading call, as (a line for the transcript, the text for the model).
    async fn read_one(
        &self,
        turn: TurnId,
        delegate: &str,
        backend: &dyn WebBackend,
        call: &RawToolCall,
    ) -> Result<(String, String), WebError> {
        let args = &call.arguments;
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        match call.name.as_str() {
            "web_search" | "search_web" => {
                let query = args
                    .get("query")
                    .or_else(|| args.get("q"))
                    .or_else(|| args.get("term"))
                    .or_else(|| args.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&text)
                    .trim();
                let out = crate::web::search(backend, query, 5).await?;
                // A search is handed to the model just as a page is, so it is
                // shown just as a page is. What a seat was told exists, and what
                // it was shown when it asked, are the same question.
                self.sink.emit(EngineEvent::WebFetched {
                    turn,
                    delegate: delegate.to_string(),
                    source: "web_search".to_string(),
                    url: String::new(),
                    title: query.to_string(),
                    text: out.clone(),
                    truncated: false,
                });
                let got = format!("\nSEARCH \"{query}\"\n{out}\n");
                Ok((format!("searched for {query:?}"), got))
            }
            _ => {
                let title = args
                    .get("title")
                    .or_else(|| args.get("topic"))
                    .or_else(|| args.get("term"))
                    .or_else(|| args.get("page"))
                    .or_else(|| args.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&text)
                    .trim();
                let source = Source::parse(
                    args.get("source")
                        .and_then(|v| v.as_str())
                        .unwrap_or("auto"),
                );
                let max_chars = {
                    let b = self.board.lock().expect("board");
                    b.config.max_fetch_chars.max(200)
                };
                let got: Retrieved = crate::web::fetch(backend, title, source, max_chars).await?;
                self.sink.emit(EngineEvent::WebFetched {
                    turn,
                    delegate: delegate.to_string(),
                    source: got.source.clone(),
                    url: got.url.clone(),
                    title: got.title.clone(),
                    text: got.text.clone(),
                    truncated: got.truncated,
                });
                let body = untrusted(&got.url, &got.text);
                Ok((
                    format!(
                        "read {} — {} ({}{})",
                        got.source,
                        got.title,
                        got.text.chars().count(),
                        if got.truncated { ", cut" } else { "" }
                    ),
                    format!("\nPAGE \"{}\"\n{body}\n", got.title),
                ))
            }
        }
    }

    /// One more round trip with the seat's own words and what it asked for.
    ///
    /// Shaped like the corrective re-prompt: the model has to see what it said
    /// before it can act on what came back.
    #[allow(clippy::too_many_arguments)]
    async fn continue_turn(
        &self,
        id: &str,
        _round: u32,
        turn: TurnId,
        text: &str,
        user: String,
        req: &ChatRequest,
        tools: &[serde_json::Value],
        native: bool,
    ) -> Option<(String, String, Vec<RawToolCall>)> {
        let (endpoint, model, temperature) = {
            let b = self.board.lock().expect("board");
            let me = b.delegate(id)?;
            let ep = b.endpoint_for(id)?;
            (ep, me.model.clone(), me.temperature.or(Some(0.4)))
        };
        let mut messages = req.messages.clone();
        if !text.trim().is_empty() {
            messages.push(ChatMessage::assistant(text.to_string()));
        }
        messages.push(ChatMessage::user(user));
        let req2 = ChatRequest {
            endpoint: self.endpoint_for_request(&endpoint),
            model,
            messages,
            tools: tools.to_vec(),
            tool_choice: if tools.is_empty() {
                "none".into()
            } else {
                "auto".into()
            },
            temperature,
            max_tokens: None,
        };
        let content_pump = Pump::content(self.sink.as_ref(), turn);
        let thought_pump = Pump::thought(self.sink.as_ref(), turn);
        let chunks = Arc::new(AtomicUsize::new(0));
        let on_chunk = |chunk: ChatChunk<'_>| {
            chunks.fetch_add(1, Ordering::Relaxed);
            match chunk {
                ChatChunk::Content(c) => content_pump.push(c),
                ChatChunk::Reasoning(r) => thought_pump.push(r),
                // The corrective is about doing rather than saying, so this
                // one's business is the argument, not the belt.
                ChatChunk::Tool(_) => {}
                ChatChunk::Reset => {
                    content_pump.reset();
                    thought_pump.reset();
                }
            }
        };
        let outcome = self.race_request(&req2, turn, id, &chunks, &on_chunk).await;
        content_pump.flush(true);
        thought_pump.flush(true);
        let resp = match outcome {
            Outcome::Done(Ok(r)) => r,
            Outcome::Done(Err(e)) => {
                // Losing the model after a fetch still leaves the page on the
                // record: the operator can see what was read even though the
                // seat never got to use it.
                self.sink.emit(EngineEvent::Notice {
                    text: format!(
                        "{} could not answer after reading: {}",
                        self.label_of_id(id),
                        describe_llm_error(e)
                    ),
                    tone: NoticeTone::Warn,
                });
                return None;
            }
            Outcome::Skipped => return None,
        };
        let mut text = resp.content.clone();
        let thinking: String = resp.reasoning.chars().take(4_000).collect();
        let calls = if native && !resp.tool_calls.is_empty() {
            resp.tool_calls.clone()
        } else {
            let thought_lines = resp.reasoning.lines().count();
            let combined = format!("{}\n{}", resp.reasoning, resp.content);
            let scraped = tools::scrape_inline_actions(&combined);
            text = scraped.text_from(thought_lines);
            scraped.calls
        };
        Some((text, thinking, calls))
    }

    /// The shelf, built on first use.
    fn web_backend(&self) -> Result<Arc<dyn WebBackend>, WebError> {
        let mut slot = self.web.lock().expect("web");
        if let Some(w) = slot.as_ref() {
            return Ok(w.clone());
        }
        let w: Arc<dyn WebBackend> = Arc::new(HttpWeb::new()?);
        *slot = Some(w.clone());
        Ok(w)
    }

    /// Convenience for tests and embedders that already hold a shared backend.
    pub fn with_backend(backend: Arc<B>, sink: Arc<dyn EventSink>) -> Self {
        Self {
            backend,
            sink,
            board: Mutex::new(Board::new(DebateConfig::default())),
            cancel: Arc::new(AtomicBool::new(false)),
            running: Mutex::new(false),
            skip_tx: Mutex::new(None),
            decider: Mutex::new(None),
            decider_pinned: AtomicBool::new(false),
            extension_pending: Mutex::new(None),
            extension_answer: Mutex::new(None),
            extension_notify: tokio::sync::Notify::new(),
            web: Mutex::new(None),
            usage_unsupported: Mutex::new(std::collections::BTreeSet::new()),
        }
    }

    /// Fix the ballot decider directly. `run` rebuilds it from the config when
    /// a decision model is configured, and otherwise leaves this alone.
    pub fn set_decider(&self, decider: Option<Arc<dyn DecisionBackend>>) {
        *self.decider.lock().expect("decider") = decider;
        self.decider_pinned.store(true, Ordering::SeqCst);
    }

    pub fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        // Wake a hung request so Stop takes effect now instead of at the
        // endpoint timeout, which may be minutes away.
        let _ = self
            .skip_tx
            .lock()
            .expect("skip")
            .take()
            .map(|tx| tx.send(()));
        // And wake an intermission suspended on an extension answer.
        // notify_one, not notify_waiters: notify_waiters stores no permit, so
        // a stop landing between the wait's check and its registration is lost
        // forever and the debate parks mid-loop with only another stop as an
        // exit.
        self.extension_notify.notify_one();
    }

    /// Abandon the turn currently in flight and move on. Returns false when no
    /// turn is running, in which case there is nothing to skip.
    pub fn skip_turn(&self) -> bool {
        self.skip_tx
            .lock()
            .expect("skip")
            .take()
            .map(|tx| tx.send(()).is_ok())
            .unwrap_or(false)
    }

    pub fn turn_in_flight(&self) -> bool {
        self.skip_tx.lock().expect("skip").is_some()
    }

    pub fn is_running(&self) -> bool {
        *self.running.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Answer the summarizer's waiting extension proposal. Returns a line for
    /// the record. Errors when nothing is waiting — the UI only offers the
    /// choice while a proposal is open, so this is a race, not a state.
    pub fn resolve_extension(&self, approved: bool, rounds: u32) -> Result<String, String> {
        let pending = self
            .extension_pending
            .lock()
            .expect("extension")
            .take();
        let Some(p) = pending else {
            return Err("no extension proposal is waiting for an answer".into());
        };
        let rounds = rounds.max(1);
        *self
            .extension_answer
            .lock()
            .expect("extension")
            = Some((approved, rounds));
        self.extension_notify.notify_one();
        Ok(if approved {
            format!("extended by {rounds} rounds on the summarizer's proposal")
        } else {
            format!(
                "declined the summarizer's proposal of {} extra rounds",
                p.rounds
            )
        })
    }

    /// An operator note pushed into the record; every delegate sees it next turn.
    pub fn inject(&self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let mut b = self.board.lock().expect("board");
        let round = b.round;
        b.note("operator", round, format!("OPERATOR NOTE: {}", text.trim()));
    }

    pub fn snapshot(&self) -> Snapshot {
        let b = self.board.lock().expect("board");
        // Finished means the engine is no longer running a debate. The old
        // `adjourn.is_some()` check only covered adjourn endings, so a
        // completed / settled / stopped debate reloaded as still-running.
        let finished = !self.is_running();
        let roster: Vec<String> = b.config.delegates.iter().map(|d| d.id.clone()).collect();
        let tallies = b
            .points
            .iter()
            .map(|p| (p.id, p.tally(&roster)))
            .collect::<std::collections::BTreeMap<_, _>>();
        Snapshot {
            motion: b.config.motion.clone(),
            round: b.round,
            max_rounds: b.config.max_rounds,
            delegates: briefs(&b),
            points: b.points.clone(),
            tallies,
            polls: b.polls.clone(),
            speeches: b.speeches.clone(),
            finished,
        }
    }

    // -- the loop ---------------------------------------------------------

    pub async fn run(&self, config: DebateConfig) {
        if let Err(why) = validate(&config) {
            self.sink.emit(EngineEvent::Failed {
                message: why,
                fatal: true,
            });
            return;
        }
        // Cleared before the flag is taken, not after: a Stop landing in that
        // window used to be silently erased and the debate began uncancellable.
        self.cancel.store(false, Ordering::SeqCst);
        {
            let mut r = self.running.lock().expect("running");
            if *r {
                self.sink.emit(EngineEvent::Notice {
                    text: "A debate is already running.".into(),
                    tone: NoticeTone::Warn,
                });
                return;
            }
            *r = true;
        }
        // No proposal or answer survives from an earlier run.
        *self.extension_pending.lock().expect("extension") = None;
        *self.extension_answer.lock().expect("extension") = None;
        // Assigned unconditionally, not only when a decision model is on:
        // otherwise turning it off for the next debate leaves the previous
        // run's URL and API key still being handed its board.
        let pinned = self.decider_pinned.load(Ordering::SeqCst);
        if !pinned {
            *self.decider.lock().expect("decider") = if matches!(
            config.decision.provider,
            crate::types::DecisionProvider::Jev
        ) {
            let d = &config.decision;
            Some(Arc::new(JevBackend::new(
                d.base_url.clone(),
                d.api_key.clone(),
                d.model.clone(),
                d.timeout_secs.max(5),
            )))
        } else {
            None
        };
        }
        *self.board.lock().expect("board") = Board::new(config);

        // A panic anywhere in the loop used to skip the `running = false`
        // below, and since the board is a poisoned lock by then, every
        // subsequent access panicked too — one bad turn bricked the engine
        // for the life of the process with no way back but a restart. The
        // unwind is caught, the board is recovered, and the run is reported.
        let result = AssertUnwindSafe(self.drive())
            .catch_unwind()
            .await
            .map_err(|_| "the debate ended unexpectedly".to_string())
            .and_then(|r| r);

        {
            let b = self.board.lock().unwrap_or_else(|e| e.into_inner());
            self.sink.emit(EngineEvent::RosterUpdated {
                delegates: briefs(&b),
                speech: None,
            });
        }
        *self.running.lock().unwrap_or_else(|e| e.into_inner()) = false;
        *self.skip_tx.lock().unwrap_or_else(|e| e.into_inner()) = None;
        match result {
            Ok(verdict) => self.sink.emit(EngineEvent::DebateEnded { verdict }),
            Err(why) => self.sink.emit(EngineEvent::Failed {
                message: why,
                fatal: true,
            }),
        }
    }

    async fn drive(&self) -> Result<DebateVerdict, String> {
        let (mut rounds, motion, speakers, closing, extra_budget) = {
            let b = self.board.lock().expect("board");
            (
                b.config.max_rounds.max(1),
                b.config.motion.clone(),
                b.speakers(),
                b.config.closing_statements,
                b.config.max_extra_rounds,
            )
        };
        self.sink.emit(EngineEvent::DebateStarted {
            motion: motion.clone(),
            rounds,
            max_extra_rounds: extra_budget,
            delegates: {
                let b = self.board.lock().expect("board");
                briefs(&b)
            },
            images: {
                let b = self.board.lock().expect("board");
                b.config
                    .images
                    .iter()
                    .map(|im| crate::types::ImageInfo {
                        id: im.id.clone(),
                        name: im.name.clone(),
                    })
                    .collect()
            },
        });

        let mut round = 0u32;
        // An extension granted *after* the closing has to carry the debate back
        // into the rounds it would otherwise have left, so the rounds phase is
        // a loop rather than a straight run.
        let mut ignore_settled = false;
        let mut closings_done = false;
        let mut end_extension_used = false;
        let mut final_summary: Option<String> = None;
        let mut conclusion: Option<Conclusion> = None;

        'debate: loop {
            // --- the rounds ------------------------------------------------
            loop {
                // Re-read: a granted extension grows the plan mid-debate.
                rounds = self.board.lock().expect("board").config.max_rounds.max(1);
                if round >= rounds || self.cancelled() {
                    break;
                }
                round += 1;
                {
                    let mut b = self.board.lock().expect("board");
                    b.round = round;
                }
                self.sink.emit(EngineEvent::RoundStarted { round });

                for seat in speakers.iter() {
                    // An adjournment ends the debate now. Auto-settlement does
                    // not: that one waits for the round to finish so every seat
                    // is heard and the round still gets laid out.
                    if self.cancelled() || self.adjourned() {
                        break;
                    }
                    if let Err(why) = self.take_turn(&seat.id, round).await {
                        self.sink.emit(EngineEvent::Failed {
                            message: format!("{}: {why}", seat.label),
                            fatal: false,
                        });
                    }
                }
                // The round is fully over, so the floor belongs to the
                // summarizer either way: laying the round out is the
                // intermission's job even when it is also the last one, and
                // skipping it is how a debate ended with no account of what had
                // happened. Only the *proposal* is suppressed once settled.
                if !self.cancelled() && !self.adjourned() {
                    let may_propose = ignore_settled || !self.settled();
                    self.run_intermission(round, may_propose).await;
                }
                if self.cancelled() {
                    break;
                }
                if !ignore_settled && self.settled() {
                    break;
                }
            }
            // An adjournment still gets its closing and its end result; only an
            // operator stop cuts the debate off without a word from anyone.
            // --- closing, once --------------------------------------------
            if closing && !closings_done {
                closings_done = true;
                for seat in speakers.iter() {
                    if self.cancelled() {
                        break;
                    }
                    self.closing_address(&seat.id).await;
                }
            }

            if self.cancelled() {
                break 'debate;
            }

            // --- the answer, which may ask for more rounds -----------------
            // Offered only when the board actually deadlocked: that is the one
            // end state more rounds could plausibly break, and it is the last
            // moment the summarizer can see it.
            let resumable = {
                // One lock, held across the whole check: locking again inside
                // the closure would deadlock on a non-reentrant mutex.
                let b = self.board.lock().expect("board");
                b.points
                    .iter()
                    .any(|pt| pt.tally(&b.ids).status == PointStatus::Stalemate)
            };
            let (fs, c, resumed) = self
                .conclusion(!end_extension_used && resumable)
                .await;
            final_summary = fs;
            conclusion = c;
            if !resumed {
                break 'debate;
            }
            // The operator granted more rounds for a deadlock. Run every round
            // they paid for rather than stopping again on the first tally,
            // which would make a grant of two rounds deliver one.
            end_extension_used = true;
            ignore_settled = true;
            self.sink.emit(EngineEvent::Notice {
                text: format!("resuming at round {} — the summarizer's request for more rounds on the deadlock was granted", round + 1),
                tone: NoticeTone::Info,
            });
        }
        // Name the ending while the board still knows why it stopped: an
        // adjournment, an early settlement, all rounds spoken, or the operator.
        let ended_how = self.ended_how(round, rounds);
        let v = self.render_verdict(round, ended_how, final_summary, conclusion);
        Ok(v)
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Why the debate stopped, as far as the board can tell. Read while the
    /// board still holds the reason.
    fn ended_how(&self, round: u32, rounds: u32) -> EndedHow {
        let b = self.board.lock().expect("board");
        if b.adjourn.is_some() {
            EndedHow::Adjourned
        } else if b.settled_by.is_some() {
            EndedHow::CalledSettled
        } else if self.cancelled() {
            EndedHow::Stopped
        } else if round < rounds {
            EndedHow::Settled
        } else {
            EndedHow::Complete
        }
    }

    /// True once a carried adjournment has closed the council.
    fn adjourned(&self) -> bool {
        self.board.lock().expect("board").adjourn.is_some()
    }

    /// True when there is nothing left to argue about.
    fn settled(&self) -> bool {
        let b = self.board.lock().expect("board");
        if b.adjourn.is_some() || b.settled_by.is_some() {
            return true;
        }
        if !b.config.auto_adjourn || b.points.is_empty() {
            return false;
        }
        b.points.iter().all(|p| p.tally(&b.ids).settled())
    }

    /// This endpoint's config with the usage request stripped when the server
    /// already proved it rejects `stream_options`. Every request is built
    /// through here so a downgraded endpoint stays downgraded for the session.
    fn endpoint_for_request(&self, ep: &EndpointConfig) -> EndpointConfig {
        if self.usage_unsupported.lock().expect("usage").contains(&ep.id) {
            let mut plain = ep.clone();
            plain.include_usage = false;
            plain
        } else {
            ep.clone()
        }
    }

    /// Runs one request with a heartbeat and a skip hatch. Every request the
    /// engine makes — speaking turn, poll ballot, closing address — goes
    /// through here, so no seat can ever go silent or unskippable again.
    /// `chunks` counts every streamed piece (speech and thinking alike) so the
    /// heartbeat can show that a slow model is still alive.
    async fn race_request(
        &self,
        req: &ChatRequest,
        turn: TurnId,
        who: &str,
        chunks: &Arc<AtomicUsize>,
        on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
    ) -> Outcome<ChatResponse> {
        let (skip_tx, mut skip_rx) = tokio::sync::oneshot::channel();
        *self.skip_tx.lock().expect("skip") = Some(skip_tx);
        let hb = {
            let sink = self.sink.clone();
            let chunks = chunks.clone();
            let who = who.to_string();
            let started = std::time::Instant::now();
            tokio::spawn(async move {
                // A slow model looks exactly like a dead one from the outside.
                // The heartbeat turns "staring at a cursor" into "47s, 0
                // tokens", which is a fact the operator can act on.
                loop {
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    sink.emit(EngineEvent::TurnProgress {
                        turn,
                        delegate: who.clone(),
                        elapsed_secs: started.elapsed().as_secs(),
                        tokens: chunks.load(Ordering::Relaxed) as u64,
                    });
                }
            })
        };
        // A seat should not lose its turn because a gateway took thirty seconds
        // to decide. Transient failures are retried inside the same turn, so the
        // debate keeps its place and the seat keeps its floor; the operator is
        // told each time, because a turn that quietly took three requests is a
        // turn somebody is waiting on and deserves to know about.
        //
        // Not retried: anything the server will say again. A 401 repeated five
        // times is five lost turns and a slower, less informative error.
        const RETRIES: u32 = 2;
        let mut attempt = 0u32;
        let outcome = loop {
            let began = std::time::Instant::now();
            let mut r = tokio::select! {
                r = self.backend.chat(req, on_chunk) => Outcome::Done(r),
                _ = &mut skip_rx => Outcome::Skipped,
            };
            let used = began.elapsed().as_secs_f64();
            // A documented failure, not a bad model: Qwen3.8 at its default
            // `xhigh` reasoning effort returns HTTP 200 with `finish_reason:
            // "stop"` and an *empty* answer after spending thousands of reasoning
            // tokens (QwenLM/Qwen3.8#216, ~17% of calls on llama.cpp and vLLM).
            // The reasoning trace is there and the answer is not, which is a fact
            // we can recognise — and a wasted turn we can simply take again.
            if let Outcome::Done(Ok(resp)) = &r {
                if answered_with_nothing(resp) {
                    self.sink.emit(EngineEvent::Notice {
                        text: format!(
                            "{who} spent {used:.0}s reasoning and returned no answer — \
                             asking again"
                        ),
                        tone: NoticeTone::Warn,
                    });
                    on_chunk(ChatChunk::Reset);
                    r = Outcome::Done(Err(LlmError::Stream(
                        "the model returned no answer after reasoning".into(),
                    )));
                }
            }
            let transient = matches!(&r, Outcome::Done(Err(e)) if e.is_transient());
            let budget = req.endpoint.timeout_secs.clamp(10, 3600) as f64;
            let exhausted = used >= budget * 0.6;
            let again = worth_retrying(transient, exhausted, attempt, RETRIES)
                && !self.cancelled();
            if transient && exhausted && !again && !self.cancelled() {
                self.sink.emit(EngineEvent::Notice {
                    text: format!(
                        "{who} used its whole {budget:.0}s budget and did not answer — \
                         not retrying, another attempt would take as long"
                    ),
                    tone: NoticeTone::Warn,
                });
            }
            if !again {
                break r;
            }
            attempt += 1;
            let why = match &r {
                Outcome::Done(Err(e)) => e.to_string(),
                _ => String::new(),
            };
            self.sink.emit(EngineEvent::Notice {
                text: format!(
                    "{who} did not answer ({why}) — retrying, {}/{}",
                    attempt, RETRIES
                ),
                tone: NoticeTone::Warn,
            });
            // Half of whatever streamed is discarded: it was the beginning of a
            // reply that will now arrive twice.
            on_chunk(ChatChunk::Reset);
            // Waiting out the backoff must not be a place Stop cannot reach.
            let back = Duration::from_millis(400u64 << (attempt - 1).min(4));
            tokio::select! {
                _ = tokio::time::sleep(back) => {}
                _ = &mut skip_rx => break Outcome::Skipped,
            }
        };
        hb.abort();
        *self.skip_tx.lock().expect("skip") = None;
        // One hook for every kind of request — speeches, ballots, the summary,
        // the closing, a continuation after reading — so the running cost is
        // complete by construction rather than by remembering each call site.
        if let Outcome::Done(Ok(resp)) = &outcome {
            // Remember a usage rejection once: the retry already saved this
            // turn, and future turns skip straight to the plain request.
            if resp
                .notes
                .iter()
                .any(|n| n.contains("rejected the usage request"))
            {
                self.usage_unsupported
                    .lock()
                    .expect("usage")
                    .insert(req.endpoint.id.clone());
            }
            if let Some(u) = resp.usage {
                self.sink.emit(EngineEvent::TurnUsage {
                    turn,
                    delegate: who.to_string(),
                    prompt_tokens: u.prompt_tokens,
                    completion_tokens: u.completion_tokens,
                });
            }
        }
        outcome
    }

    /// A decision-model classification with the same heartbeat and skip hatch
    /// as a speaking request.
    async fn race_classify(
        &self,
        ext: &Arc<dyn DecisionBackend>,
        req: &ClassifyRequest,
        turn: TurnId,
        who: &str,
    ) -> Outcome<Decision> {
        let (skip_tx, skip_rx) = tokio::sync::oneshot::channel();
        *self.skip_tx.lock().expect("skip") = Some(skip_tx);
        let hb = {
            let sink = self.sink.clone();
            let who = who.to_string();
            let started = std::time::Instant::now();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    sink.emit(EngineEvent::TurnProgress {
                        turn,
                        delegate: who.clone(),
                        elapsed_secs: started.elapsed().as_secs(),
                        tokens: 0,
                    });
                }
            })
        };
        let outcome = tokio::select! {
            r = ext.classify(req) => Outcome::Done(r),
            _ = skip_rx => Outcome::Skipped,
        };
        hb.abort();
        *self.skip_tx.lock().expect("skip") = None;
        outcome
    }

    // -- a single turn ---------------------------------------------------

    async fn take_turn(&self, id: &str, round: u32) -> Result<(), String> {
        let (messages, endpoint, model, temperature, native, label, color, restore) = {
            let mut b = self.board.lock().expect("board");
            let (Some(me), Some(ep)) = (b.delegate(id).cloned(), b.endpoint_for(id)) else {
                return Err("no endpoint is configured for this seat".into());
            };
            // Read before the prompt, not after: the prompt has to describe the
            // belt this seat is actually given, and that belt depends on both.
            let native = ep.native_tools;
            let judging = me.seat_role == SeatRole::Judge;
            let system = prompt::system_prompt(&me, &b.config, native, judging);
            let mut history = b
                .history
                .remove(id)
                .unwrap_or_default();
            if history.is_empty() {
                history.push(ChatMessage::system(system));
                // Images ride on the opening message: every seat's history
                // starts here, and history is trimmed to the opening plus the
                // last few turns, so they stay visible without being resent
                // (and re-billed) every turn.
                let mut opening =
                    ChatMessage::user(prompt::opening_message(&b.config));
                opening.images = b
                    .config
                    .images
                    .iter()
                    .map(|im| im.data_url.clone())
                    .collect();
                history.push(opening);
            }
            let board_text = prompt::board_snapshot(&b.points, &b.config.delegates);
            // Captured before anything below clears or advances them. Both used
            // to be read *after* the press queue was drained and the `seen` mark
            // moved, which made `TurnRollback` restore the cleared state rather
            // than the one the seat started the turn with: a skip or a failed
            // request silently destroyed every question pressed on that seat and
            // kept the advanced mark that hides the notes it never read.
            let seen_before = b.seen.get(id).copied().unwrap_or(0);
            let pressed_before = b.pressed.get(id).cloned().unwrap_or_default();

            let mut all_unseen = b.unseen(id);
            // How much of the tail the feed had to drop. Kept as a number, not
            // just a slice, because the seat has to be told: a record quietly
            // omitted leaves it arguing against a board it has not seen argued
            // into, and it has no way of knowing that.
            let dropped = all_unseen.len().saturating_sub(RECORD_TAIL);
            let unseen = if dropped > 0 {
                all_unseen.split_off(all_unseen.len() - RECORD_TAIL)
            } else {
                all_unseen
            };
            let budget = auto_record_budget(b.points.len());
            let mut since = unseen
                .iter()
                .map(|e| {
                    let who = b
                        .delegate(&e.delegate)
                        .map(|d| d.label.clone())
                        .unwrap_or_else(|| e.delegate.clone());
                    format!("R{} {}: {}", e.round, who, e.text)
                })
                .collect::<Vec<_>>()
                .join("\n");
            while since.chars().count() > budget {
                let cut = since
                    .char_indices()
                    .nth(since.chars().count() - budget + 1)
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                since = since[cut..].to_string();
            }

            let mut floor = String::new();
            if !pressed_before.is_empty() {
                floor.push_str(&format!(
                    "You are being pressed and must answer directly:\n- {}\n\n",
                    pressed_before.join("\n- ")
                ));
            }
            if dropped > 0 {
                floor.push_str(&format!(
                    "{} earlier record{} {} that your feed could not carry. Ask if you \
                     need them.\n\n",
                    dropped,
                    if dropped == 1 { "" } else { "s" },
                    if dropped == 1 { "exists" } else { "exist" },
                ));
            }
            floor.push_str(&if since.trim().is_empty() {
                "Nothing has happened since your last turn.".to_string()
            } else {
                format!("Since your last turn:\n{since}")
            });
            floor.push_str(&format!(
                "\n\nROUND {round} — {}. You have the floor. Act.",
                me.label
            ));

            history.push(ChatMessage::user(format!(
                "{board_text}\n\n———\n\n{floor}"
            )));
            prune_superseded_briefs(&mut history);
            // Only what they were actually shown. The head the tail-truncation dropped can
            // never be shown to this seat again, so the mark advances over it —
            // but no further. Deriving it from `record.len()` instead ran the
            // mark `dropped` entries past the last thing actually shown, and
            // those entries were gone from this seat's feed for good.
            let seen_upto = seen_before + dropped;
            b.seen.insert(id.to_string(), seen_upto);
            b.pressed.remove(id);
            b.history.insert(id.to_string(), history.clone());

            // What to put back if the turn never happens: a skip or a failure
            // used to leave the delegate holding an unanswered user message,
            // a cleared press queue, and a `seen` mark for notes they never
            // read. Consecutive user turns also break strict proxies.
            let restore = TurnRollback {
                history: history.clone(),
                pressed: pressed_before,
                seen: seen_before,
            };
            (
                history,
                ep,
                me.model.clone(),
                me.temperature.or(Some(0.4)),
                native,
                me.label.clone(),
                me.color.clone(),
                restore,
            )
        };

        let turn = {
            let mut b = self.board.lock().expect("board");
            let turn = b.next_turn;
            b.next_turn += 1;
            turn
        };

        self.sink.emit(EngineEvent::TurnStarted {
            turn,
            delegate: id.to_string(),
            label: label.clone(),
            color: color.clone(),
            model: model.clone(),
            round,
        });

        let tools = if native {
            let (may_read, judging) = {
                let b = self.board.lock().expect("board");
                (
                    Self::seat_may_read(&b, id),
                    b.delegate(id).map(|d| d.seat_role) == Some(SeatRole::Judge),
                )
            };
            let mut belt = tools::speaking_tools();
            if may_read {
                belt.extend(tools::web_tools());
            }
            if judging {
                belt.extend(tools::rule_tool());
            }
            belt
        } else {
            Vec::new()
        };
        let req = ChatRequest {
            endpoint: self.endpoint_for_request(&endpoint),
            model,
            messages,
            tools: tools.clone(),
            tool_choice: if tools.is_empty() {
                "none".into()
            } else {
                "auto".into()
            },
            temperature,
            // Speaking turns are uncapped: the server's maximum is the only
            // honest limit, and a cut-off mid-argument is worse than a long one.
            max_tokens: None,
        };

        let content_pump = Pump::content(self.sink.as_ref(), turn);
        let thought_pump = Pump::thought(self.sink.as_ref(), turn);
        let chunks = Arc::new(AtomicUsize::new(0));
        let sink = self.sink.clone();
        let counter = chunks.clone();
        let on_chunk = {
            let (content_pump, thought_pump) = (&content_pump, &thought_pump);
            move |chunk: ChatChunk<'_>| {
                counter.fetch_add(1, Ordering::Relaxed);
                match chunk {
                    ChatChunk::Content(c) => content_pump.push(c),
                    ChatChunk::Reasoning(r) => thought_pump.push(r),
                    ChatChunk::Tool(name) => sink.emit(EngineEvent::TurnActivity {
                        turn,
                        delegate: id.to_string(),
                        what: format!("calling `{name}`"),
                    }),
                    ChatChunk::Reset => {
                        content_pump.reset();
                        thought_pump.reset();
                    }
                }
            }
        };

        let outcome = self.race_request(&req, turn, id, &chunks, &on_chunk).await;
        content_pump.flush(true);
        thought_pump.flush(true);

        let resp = match outcome {
            Outcome::Done(Ok(r)) => r,
            Outcome::Done(Err(e)) => {
                let why = describe_llm_error(e);
                self.rollback_turn(id, restore);
                // Still leave a mark on the record: a hole in the transcript
                // reads as a silent seat rather than a failed request.
                let speech = Speech {
                    id: turn,
                    delegate: id.to_string(),
                    round,
                    text: String::new(),
                    reasoning: String::new(),
                    actions: vec![format!("(turn failed: {why})")],
                    phase: TurnPhase::Done,
                };
                {
                    let mut b = self.board.lock().expect("board");
                    b.speeches.push(speech.clone());
                }
                self.sink.emit(EngineEvent::SpeechFinished { turn, speech });
                return Err(why);
            }
            Outcome::Skipped => {
                self.rollback_turn(id, restore);
                self.sink.emit(EngineEvent::TurnSkipped {
                    turn,
                    delegate: id.to_string(),
                    label: label.clone(),
                });
                return Ok(());
            }
        };
        for note in &resp.notes {
            self.sink.emit(EngineEvent::Notice {
                text: format!("{label}: {note}"),
                tone: NoticeTone::Info,
            });
        }
        self.note_truncation(&resp, &label, "turn");

        let text = resp.content.clone();
        let thinking = resp
            .reasoning
            .clone()
            .chars()
            .take(4_000)
            .collect::<String>();
        // Reasoning-first models (DeepSeek-R1 and kin) often leave their tool
        // blocks in the thinking rather than the answer, so both are read — and
        // whatever was consumed comes back out of what the transcript shows.
        let mut text = text;
        let calls = if native && !resp.tool_calls.is_empty() {
            resp.tool_calls
        } else {
            let thought_lines = resp.reasoning.lines().count();
            let combined = format!("{}\n{}", resp.reasoning, text);
            let scraped = tools::scrape_inline_actions(&combined);
            text = scraped.text_from(thought_lines);
            scraped.calls
        };

        // Reading is part of the turn, not a detour from it: what the seat finds
        // is what it argues from, so this has to happen before the board hears
        // anything the seat said on its way to the shelf.
        let (text, thinking, calls, read_log) = self
            .read_the_web(id, round, turn, text, thinking, calls, &req, &tools, native)
            .await;

        self.activity(turn, id, "acting on the board");
        let mut actions_log = self.apply_actions(id, round, turn, &calls).await;
        // The pages read come first in the log: they happened before anything
        // the seat then did to the board.
        actions_log.splice(0..0, read_log.into_iter().map(|l| format!("{label} {l}")));
        if text.trim().is_empty() && thinking.trim().is_empty() && calls.is_empty() {
            // A model that just stops — blank reply, no thinking, no calls —
            // used to leave a hole in the transcript. Name it instead.
            self.sink.emit(EngineEvent::Notice {
                text: format!("{label} returned an empty turn — no speech, no thinking, no actions"),
                tone: NoticeTone::Warn,
            });
            actions_log.push("(empty turn — nothing said, nothing done)".into());
        } else if actions_log.is_empty() {
            // The other failure mode: a turn of announced intentions with no
            // tool calls behind them. One corrective re-prompt, then the
            // inaction is labelled on the record.
            let (text, thinking, calls, actions_log) = match self
                .correct_inaction(id, round, turn, &text, &thinking, &calls, &req, &tools, native)
                .await
            {
                Some(fix) => fix,
                None => {
                    actions_log.push("(no actions taken this turn)".into());
                    (text, thinking, calls, actions_log)
                }
            };
            self.finish_turn(id, round, turn, &label, text, thinking, calls, actions_log)
                .await;
            return Ok(());
        }

        self.finish_turn(id, round, turn, &label, text, thinking, calls, actions_log)
            .await;
        Ok(())
    }

    /// Say so when the server cut a reply short.
    ///
    /// Applied to every kind of request, not just speaking turns: the
    /// summarizer's intermission and end result are read as finished prose, so
    /// a half-sentence there reads as a conclusion rather than as a truncation.
    fn note_truncation(&self, resp: &ChatResponse, label: &str, what: &str) {
        match resp.finish_reason.as_deref() {
            Some("length") => self.sink.emit(EngineEvent::Notice {
                text: format!(
                    "{label}'s {what} hit a token limit — the server cut it short mid-sentence"
                ),
                tone: NoticeTone::Warn,
            }),
            Some("content_filter") => self.sink.emit(EngineEvent::Notice {
                text: format!("{label}'s {what} was stopped by the server's content filter"),
                tone: NoticeTone::Warn,
            }),
            _ => {}
        }
    }

    /// Put a seat's history, press queue and `seen` mark back the way they were
    /// before a turn that never happened.
    fn rollback_turn(&self, id: &str, restore: TurnRollback) {
        let mut b = self.board.lock().expect("board");
        // Drop the trailing user message this turn appended.
        let mut history = restore.history;
        history.pop();
        b.history.insert(id.to_string(), history);
        if restore.pressed.is_empty() {
            b.pressed.remove(id);
        } else {
            b.pressed.insert(id.to_string(), restore.pressed);
        }
        b.seen.insert(id.to_string(), restore.seen);
    }

    /// One corrective re-prompt when a turn announced intentions but emitted no
    /// tool calls. The model gets exactly one chance to act; if it still doesn't,
    /// the inaction is labelled on the record. Returns `None` when the
    /// re-prompt itself failed, so the caller can fall back to the label.
    async fn correct_inaction(
        &self,
        id: &str,
        round: u32,
        turn: TurnId,
        text: &str,
        _thinking: &str,
        _calls: &[RawToolCall],
        req: &ChatRequest,
        tools: &[serde_json::Value],
        native: bool,
    ) -> Option<(String, String, Vec<RawToolCall>, Vec<String>)> {
        let (endpoint, model, temperature) = {
            let b = self.board.lock().expect("board");
            let me = b.delegate(id)?;
            let ep = b.endpoint_for(id)?;
            (ep, me.model.clone(), me.temperature.or(Some(0.4)))
        };
        let mut messages = req.messages.clone();
        // The model needs to see its own words to understand the correction.
        messages.push(ChatMessage::assistant(text.to_string()));
        messages.push(ChatMessage::user(
            "You did not call any tools. If you intended to act — raise a point, \
             vote, steelman, rebut, cite, press, poll, or adjourn — call the tool \
             now. If you only meant to speak, that is fine, but say so explicitly \
             and stop announcing actions you are not taking.",
        ));
        let req2 = ChatRequest {
            endpoint: self.endpoint_for_request(&endpoint),
            model,
            messages,
            tools: tools.to_vec(),
            tool_choice: if tools.is_empty() {
                "none".into()
            } else {
                "auto".into()
            },
            temperature,
            max_tokens: req.max_tokens,
        };
        let content_pump = Pump::content(self.sink.as_ref(), turn);
        let thought_pump = Pump::thought(self.sink.as_ref(), turn);
        let chunks = Arc::new(AtomicUsize::new(0));
        let on_chunk = |chunk: ChatChunk<'_>| {
            chunks.fetch_add(1, Ordering::Relaxed);
            match chunk {
                ChatChunk::Content(c) => content_pump.push(c),
                ChatChunk::Reasoning(r) => thought_pump.push(r),
                ChatChunk::Tool(_) => {}
                ChatChunk::Reset => {
                    content_pump.reset();
                    thought_pump.reset();
                }
            }
        };
        let outcome = self.race_request(&req2, turn, id, &chunks, &on_chunk).await;
        content_pump.flush(true);
        thought_pump.flush(true);
        let resp = match outcome {
            Outcome::Done(Ok(r)) => r,
            Outcome::Done(Err(e)) => {
                self.sink.emit(EngineEvent::Notice {
                    text: format!("corrective re-prompt failed: {}", describe_llm_error(e)),
                    tone: NoticeTone::Warn,
                });
                return None;
            }
            Outcome::Skipped => return None,
        };
        let text2 = resp.content.clone();
        let thinking2 = resp
            .reasoning
            .clone()
            .chars()
            .take(4_000)
            .collect::<String>();
        let mut text2 = text2;
        let calls2 = if native && !resp.tool_calls.is_empty() {
            resp.tool_calls
        } else {
            let thought_lines = resp.reasoning.lines().count();
            let combined = format!("{}\n{}", resp.reasoning, text2);
            let scraped = tools::scrape_inline_actions(&combined);
            text2 = scraped.text_from(thought_lines);
            scraped.calls
        };
        if calls2.is_empty() {
            return None;
        }
        let actions_log = self.apply_actions(id, round, turn, &calls2).await;
        if actions_log.is_empty() {
            return None;
        }
        Some((text2, thinking2, calls2, actions_log))
    }

    /// Publish a finished turn: history, speeches, and the event the UI renders.
    async fn finish_turn(
        &self,
        id: &str,
        round: u32,
        turn: TurnId,
        _label: &str,
        text: String,
        thinking: String,
        _calls: Vec<RawToolCall>,
        actions_log: Vec<String>,
    ) {
        let speech = Speech {
            id: turn,
            delegate: id.to_string(),
            round,
            text,
            reasoning: thinking,
            actions: actions_log,
            phase: TurnPhase::Done,
        };
        {
            let mut b = self.board.lock().expect("board");
            let hist = b.history.entry(id.to_string()).or_default();
            // The clip goes into the history only. The transcript, the export
            // and the screen all get the whole speech.
            hist.push(ChatMessage::assistant(clip_for_memory(&speech.text)));
            if hist.len() > 12 {
                let head: Vec<ChatMessage> = hist.drain(..2).collect();
                let tail: Vec<ChatMessage> = hist.split_off(hist.len().saturating_sub(8));
                *hist = head.into_iter().chain(tail).collect();
            }
            b.speeches.push(speech.clone());
        }
        self.sink.emit(EngineEvent::SpeechFinished { turn, speech });
    }

    async fn apply_actions(
        &self,
        id: &str,
        round: u32,
        turn: TurnId,
        calls: &[RawToolCall],
    ) -> Vec<String> {
        let mut log = Vec::new();
        // No slider: identical calls are folded first (a model stuttering
        // `vote P1 win` six times means it once), and the ceiling below is a
        // safety bound against a looping model, not a target.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut rules_cast = 0u32;
        let mut rated_this_turn: std::collections::HashSet<PointId> =
            std::collections::HashSet::new();
        for call in calls.iter().take(Self::TOOL_CEILING) {
            let key =
                serde_json::to_string(&(&call.name, &call.arguments)).unwrap_or_default();
            if !seen.insert(key) {
                continue;
            }
            let action = match tools::parse_action(call) {
                Ok(a) => a,
                Err(why) => {
                    self.sink.emit(EngineEvent::ActionRejected {
                        turn,
                        delegate: id.to_string(),
                        tool: call.name.clone(),
                        reason: why,
                    });
                    continue;
                }
            };
            // A poll needs a round trip through every other model, so it is
            // handled outside the board mutation.
            if let Action::Poll {
                question,
                options,
                reason,
            } = action
            {
                let poll_id = {
                    let mut b = self.board.lock().expect("board");
                    let poll_id = b.next_poll;
                    b.next_poll += 1;
                    b.polls.push(Poll {
                        id: poll_id,
                        question: question.clone(),
                        options: options.clone(),
                        opened_by: id.to_string(),
                        round,
                        ballots: vec![],
                        closed: false,
                    });
                    b.open_poll = Some(poll_id);
                    let line = match &reason {
                        Some(r) => format!("opened poll: {question} ({r})"),
                        None => format!("opened poll: {question}"),
                    };
                    b.note(id, round, line);
                    poll_id
                };
                self.activity(
                    turn,
                    id,
                    format!(
                        "asking {} other seats",
                        self.board.lock().expect("board").voting_ids().len() - 1
                    ),
                );
                let poll = self
                    .board
                    .lock()
                    .expect("board")
                    .polls
                    .iter()
                    .find(|p| p.id == poll_id)
                    .cloned()
                    .expect("just pushed");
                self.sink.emit(EngineEvent::PollOpened { poll: poll.clone() });
                self.sink.emit(EngineEvent::ActionTaken {
                    turn,
                    delegate: id.to_string(),
                    action: "poll".into(),
                    detail: format!("opened poll: {question}"),
                });
                log.push(format!("opened poll: {question}"));
                // The opener's speech has not finished yet, so the UI still
                // shows their turn as live. Keying ballot heartbeats to it
                // keeps the clock ticking instead of freezing mid-poll.
                self.run_ballot(poll_id, turn).await;
                continue;
            }

            // Adjourning, like polling, needs a round trip through every
            // other seat, so motions run here rather than in the board
            // mutation.
            if let Action::Adjourn { reason } = action {
                let moved = self.open_adjournment(id, round, reason.clone());
                match moved {
                    Ok(line) => {
                        self.sink.emit(EngineEvent::ActionTaken {
                            turn,
                            delegate: id.to_string(),
                            action: "adjourn".into(),
                            detail: line.clone(),
                        });
                        log.push(line);
                        self.activity(turn, id, "asking the council to confirm");
                        let outcome = self.run_confirmation(id, round, turn).await;
                        log.push(outcome);
                    }
                    Err(why) => {
                        self.sink.emit(EngineEvent::ActionRejected {
                            turn,
                            delegate: id.to_string(),
                            tool: "adjourn".into(),
                            reason: why.clone(),
                        });
                        log.push(format!("(rejected adjourn: {why})"));
                    }
                }
                continue;
            }

            // A review, like a poll, needs a round trip through every other
            // seat, so it runs here rather than in the board mutation.
            if let Action::Review { point, reason } = action {
                let opened = self.open_review(id, round, point, reason);
                match opened {
                    Ok((point_id, line)) => {
                        self.sink.emit(EngineEvent::ActionTaken {
                            turn,
                            delegate: id.to_string(),
                            action: "review".into(),
                            detail: line.clone(),
                        });
                        log.push(line);
                        self.activity(turn, id, "asking the council about the deadlock");
                self.run_review(point_id, turn).await;
                    }
                    Err(why) => {
                        self.sink.emit(EngineEvent::ActionRejected {
                            turn,
                            delegate: id.to_string(),
                            tool: "review".into(),
                            reason: why.clone(),
                        });
                        log.push(format!("(rejected review: {why})"));
                    }
                }
                continue;
            }

            // One rating per point per turn, for every seat.
            //
            // The cap used to be one vote *per turn* for the Arbiter alone, on
            // the reasoning that a second vote diluted the point of its seat. It
            // did not: an Arbiter engaging four points has to rate four points,
            // and forbidding it pushed those judgements off the record entirely
            // rather than making them count for less.
            //
            // What actually needed a limit was rating the same point twice in
            // one breath — `win` then `lose`, last one winning — which is not
            // deliberation but a model thinking out loud, and it leaves the
            // record showing a verdict the seat passed through on the way
            // somewhere else. As many points as you like, once each; to change
            // where one lands, `rule`, which is what it is for.
            if let Action::Vote { point, .. } = &action {
                if !rated_this_turn.insert(*point) {
                    // Telling every seat to reach for `rule` would be the same
                    // mistake as putting it in everyone's belt: only the Arbiter
                    // has one, so the other two would be told to call an
                    // instrument they cannot reach. Each is pointed at the move
                    // it actually has.
                    let judging = self
                        .board
                        .lock()
                        .expect("board")
                        .delegate(id)
                        .map(|d| d.seat_role)
                        == Some(SeatRole::Judge);
                    let why = if judging {
                        format!(
                            "you have already rated P{point} this turn — if you want to move \
                             it off where the counting put it, that is what `rule` is for"
                        )
                    } else {
                        format!(
                            "you have already rated P{point} this turn — a later turn is \
                             where you change your mind, and saying why is what makes it \
                             count"
                        )
                    };
                    self.sink.emit(EngineEvent::ActionRejected {
                        turn,
                        delegate: id.to_string(),
                        tool: "vote".into(),
                        reason: why.clone(),
                    });
                    log.push(format!("(rejected vote: {why})"));
                    continue;
                }
            }

            // The Arbiter gets one ruling a turn. The prompt and the schema
            // both promise it; nothing here kept that promise, so an Arbiter
            // could rule every point in one reply and call it judgement. Now
            // that `rule` settles a point outright, the promise is the whole
            // point of the seat: a ruling nobody could make one of is just a
            // vote that skips the count.
            if let Action::Rule { point, .. } = &action {
                rules_cast += 1;
                let judging = self
                    .board
                    .lock()
                    .expect("board")
                    .delegate(id)
                    .map(|d| d.seat_role)
                    == Some(SeatRole::Judge);
                if judging && rules_cast > 1 {
                    let why = format!(
                        "one ruling a turn — P{point} is settled; vote on the rest"
                    );
                    self.sink.emit(EngineEvent::ActionRejected {
                        turn,
                        delegate: id.to_string(),
                        tool: "rule".into(),
                        reason: why.clone(),
                    });
                    log.push(format!("(rejected rule: {why})"));
                    continue;
                }
            }

            let detail = self.apply_action(id, round, &action);
            match detail {
                Ok(line) => {
                    let mut b = self.board.lock().expect("board");
                    b.note(id, round, line.clone());
                    log.push(line.clone());
                    drop(b);
                    self.sink.emit(EngineEvent::ActionTaken {
                        turn,
                        delegate: id.to_string(),
                        action: action.name().to_string(),
                        detail: line,
                    });
                }
                Err(why) => {
                    self.sink.emit(EngineEvent::ActionRejected {
                        turn,
                        delegate: id.to_string(),
                        tool: action.name().to_string(),
                        reason: why.clone(),
                    });
                    log.push(format!("(rejected {}: {why})", action.name()));
                }
            }
        }
        if calls.len() > Self::TOOL_CEILING {
            // Silently applying the first twelve is how a looping model gets to
            // look like a deliberating one.
            self.sink.emit(EngineEvent::Notice {
                text: format!(
                    "{} seat emitted {} calls in one turn; the first {} were applied",
                    self.label_of_id(id),
                    calls.len(),
                    Self::TOOL_CEILING
                ),
                tone: NoticeTone::Warn,
            });
            log.push(format!(
                "(the turn's {} further calls were not applied)",
                calls.len() - Self::TOOL_CEILING
            ));
        }
        log
    }

    /// Say what this turn is doing, so a wait is watchable.
    fn activity(&self, turn: TurnId, id: &str, what: impl Into<String>) {
        self.sink.emit(EngineEvent::TurnActivity {
            turn,
            delegate: id.to_string(),
            what: what.into(),
        });
    }

    /// How the board stands, in one line. The Summarizer is asked to judge
    /// agreement and cannot see the tally in the UI, so this is the same split
    /// the operator is looking at.
    fn tally_line(&self) -> String {
        let b = self.board.lock().expect("board");
        let (mut carried, mut fell, mut stuck, mut open, mut pulled) = (0, 0, 0, 0, 0);
        for p in &b.points {
            match p.tally(&b.ids).status {
                PointStatus::Carried => carried += 1,
                PointStatus::Fell => fell += 1,
                PointStatus::Withdrawn => pulled += 1,
                PointStatus::Open => open += 1,
                PointStatus::Stalemate => stuck += 1,
            }
        }
        format!(
            "{carried} carried, {fell} fell, {stuck} deadlocked, {open} undecided{}",
            if pulled > 0 {
                format!(", {pulled} withdrawn")
            } else {
                String::new()
            },
        )
    }

    fn label_of_id(&self, id: &str) -> String {
        self.board
            .lock()
            .expect("board")
            .delegate(id)
            .map(|d| d.label.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// Safety ceiling on actions applied per turn. Unreachable in normal
    /// deliberation once duplicates are folded; only a looping model gets here.
    const TOOL_CEILING: usize = 12;

    /// Pure state transition for one action. Returns the line it wrote to the
    /// public record, or why it was refused.
    fn apply_action(&self, id: &str, round: u32, action: &Action) -> Result<String, String> {
        let mut b = self.board.lock().expect("board");
        let label = b
            .delegate(id)
            .map(|d| d.label.clone())
            .unwrap_or_else(|| id.to_string());

        let emit_point = |b: &mut Board, point_id: PointId| {
            if let Some(p) = b.points.iter().find(|p| p.id == point_id) {
                let tally = p.tally(&b.ids);
                let resolved = tally.decided();
                let point = p.clone();
                self.sink.emit(EngineEvent::PointUpdated {
                    point,
                    tally,
                    resolved,
                });
            }
        };

        match action {
            Action::MakePoint { text, beats } => {
                // The only dedupe on this path used to be exact argument
                // equality within a single turn, so a model that re-raised a
                // claim next round — or restated it slightly within one turn —
                // put the same point on the board twice and the tally counted
                // it twice. Claims are matched on normalized text instead.
                let normalized = normalize_claim(text);
                if let Some(existing) = b
                    .points
                    .iter()
                    .find(|p| normalize_claim(&p.text) == normalized)
                {
                    return Err(format!(
                        "you have already raised this as P{} — vote on it, press \
                         {} about it, or withdraw it first",
                        existing.id, existing.id
                    ));
                }
                let point_id = b.next_point;
                b.next_point += 1;
                let valid_beats: Vec<PointId> =
                    beats.iter().copied().filter(|x| b.point(*x).is_some()).collect();
                b.points.push(Point {
                    ruling: None,
                    id: point_id,
                    text: text.clone(),
                    raised_by: id.to_string(),
                    round,
                    withdrawn: false,
                    withdrawn_reason: None,
                    beats: valid_beats.clone(),
                    votes: Default::default(),
                    rebuttals: vec![],
                    steelmen: vec![],
                    evidence: vec![],
                    conceded_by: Default::default(),
                    review: None,
                });
                let suffix = if valid_beats.is_empty() {
                    String::new()
                } else {
                    format!(
                        ", offered against {}",
                        valid_beats
                            .iter()
                            .map(|x| format!("P{x}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                emit_point(&mut b, point_id);
                Ok(format!("{label} raised P{point_id}: {text}{suffix}"))
            }

            Action::Vote {
                point,
                verdict,
                reason,
            } => {
                let Some(p) = b.point_mut(*point) else {
                    return Err(format!("P{point} does not exist"));
                };
                // Compare against the rating actually on the record, implicit
                // proponent win included: a proponent voting `lose` on their own
                // point really has changed their mind.
                let previous = p.rating_of(id);
                let changed = previous.is_some() && previous != Some(*verdict);
                p.votes.insert(
                    id.to_string(),
                    VoteRecord {
                        delegate: id.to_string(),
                        verdict: *verdict,
                        reason: reason.clone(),
                        round,
                    },
                );
                p.conceded_by.remove(id);
                let word = if changed { "re-voted" } else { "voted" };
                let suffix = reason
                    .as_ref()
                    .map(|r| format!(" — {}", r.trim()))
                    .unwrap_or_default();
                emit_point(&mut b, *point);
                Ok(format!(
                    "{label} {word} {} on P{point}{suffix}",
                    verdict.as_str()
                ))
            }

            Action::Concede { point, reason } => {
                let Some(p) = b.point_mut(*point) else {
                    return Err(format!("P{point} does not exist"));
                };
                if p.raised_by == id {
                    // Conceding your own claim is retracting it.
                    p.withdrawn = true;
                    p.withdrawn_reason = reason.clone();
                } else {
                    p.conceded_by
                        .insert(id.to_string(), reason.clone().unwrap_or_default());
                    p.votes.insert(
                        id.to_string(),
                        VoteRecord {
                            delegate: id.to_string(),
                            verdict: Verdict::Lose,
                            reason: reason.clone(),
                            round,
                        },
                    );
                }
                let suffix = reason
                    .as_ref()
                    .map(|r| format!(" — {}", r.trim()))
                    .unwrap_or_default();
                emit_point(&mut b, *point);
                Ok(format!("{label} conceded P{point}{suffix}"))
            }

            Action::Withdraw { point, reason } => {
                let Some(p) = b.point_mut(*point) else {
                    return Err(format!("P{point} does not exist"));
                };
                if p.raised_by != id {
                    return Err(format!("P{point} was not raised by {label}"));
                }
                p.withdrawn = true;
                p.withdrawn_reason = reason.clone();
                let suffix = reason
                    .as_ref()
                    .map(|r| format!(" — {}", r.trim()))
                    .unwrap_or_default();
                emit_point(&mut b, *point);
                Ok(format!("{label} withdrew P{point}{suffix}"))
            }

            Action::PollBallot { .. } => {
                Err("poll_ballot is only legal while answering a poll".into())
            }
            Action::Poll { .. } => {
                Err("a poll may only be opened on your own turn, not inside a ballot".into())
            }
            Action::StalemateBallot { .. } => {
                Err("stalemate_ballot is only legal while answering a deadlock review".into())
            }
            Action::Review { .. } => {
                Err("a review is opened from apply_actions, never here".into())
            }

            Action::Steelman {
                point,
                restatement,
            } => {
                let Some(p) = b.point_mut(*point) else {
                    return Err(format!("P{point} does not exist"));
                };
                p.steelmen.push(Remark {
                    by: id.to_string(),
                    round,
                    text: restatement.clone(),
                });
                let keep = p.steelmen.len().saturating_sub(3);
                p.steelmen.drain(..keep);
                emit_point(&mut b, *point);
                Ok(format!(
                    "{label} steelmanned P{point}: {restatement}"
                ))
            }

            Action::Rebut { point, argument } => {
                let Some(p) = b.point_mut(*point) else {
                    return Err(format!("P{point} does not exist"));
                };
                p.rebuttals.push(Remark {
                    by: id.to_string(),
                    round,
                    text: argument.clone(),
                });
                let keep = p.rebuttals.len().saturating_sub(3);
                p.rebuttals.drain(..keep);
                emit_point(&mut b, *point);
                Ok(format!("{label} rebutted P{point}: {argument}"))
            }

            Action::Cite {
                source,
                quote,
                point,
                stance,
            } => {
                let text = match quote {
                    Some(q) => format!("{source} — \"{q}\""),
                    None => source.clone(),
                };
                match point.and_then(|pid| b.point_mut(pid)) {
                    Some(p) => {
                        p.evidence.push(Remark {
                            by: id.to_string(),
                            round,
                            text: text.clone(),
                        });
                        let keep = p.evidence.len().saturating_sub(4);
                        p.evidence.drain(..keep);
                        let pid = p.id;
                        emit_point(&mut b, pid);
                        Ok(format!("{label} cited {text} ({stance}) re P{pid}"))
                    }
                    None => match point {
                        // Aimed at a point and missed it. Reported as a failure,
                        // because a citation that silently attaches to nothing is
                        // evidence the record believes it has and does not.
                        Some(pid) => Err(format!(
                            "P{pid} does not exist, so there is nothing for this to support"
                        )),
                        None => Ok(format!("{label} cited {text} ({stance})")),
                    },
                }
            }

            Action::Press {
                to,
                point,
                question,
            } => {
                // A seat is named, and a name can be wrong in two different ways:
                // misspelled, or shared. Misspellings are what actually happen —
                // `validate` refuses duplicate labels, so a name that resolves is
                // unambiguous — and an id sidesteps them entirely, so both are
                // accepted. Falling back to the id keeps the engine working for a
                // caller that has one.
                let named = to.as_deref().map(str::trim).filter(|t| !t.is_empty());
                let target = match named {
                    None => {
                        return Err(
                            "press needs `to` — who are you putting on the spot?".into()
                        );
                    }
                    Some(t) => b
                        .config
                        .delegates
                        .iter()
                        .find(|d| d.id == t)
                        .or_else(|| b.config.delegates.iter().find(|d| d.label.eq_ignore_ascii_case(t)))
                        .map(|d| d.id.clone()),
                };
                match target {
                    Some(tid) if tid != id => {
                        let line = match point {
                            Some(p) => format!("re P{p}: {question}"),
                            None => question.clone(),
                        };
                        let name = b
                            .delegate(&tid)
                            .map(|d| d.label.clone())
                            .unwrap_or_else(|| tid.clone());
                        b.pressed.entry(tid).or_default().push(line);
                        Ok(format!("{label} pressed {name} — {question}"))
                    }
                    Some(_) => Err("you cannot press yourself".into()),
                    // Naming the council back is what lets the model correct
                    // itself in one turn instead of guessing a third time.
                    None => {
                        let roster = b
                            .config
                            .delegates
                            .iter()
                            .map(|d| d.label.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        Err(format!(
                            "no seat called {:?} in this council — the seats are: {roster}",
                            to.as_deref().unwrap_or_default()
                        ))
                    }
                }
            }

            Action::Rule {
                point,
                ruling,
                reason,
            } => {
                // Only the seat that rules may rule. Otherwise every model grows
                // a tiebreak and the board stops being a count.
                if b.delegate(id).map(|d| d.seat_role) != Some(SeatRole::Judge) {
                    return Err("only the Arbiter may rule a point".into());
                }
                let Some(p) = b.point_mut(*point) else {
                    return Err(format!("P{point} does not exist"));
                };
                if p.withdrawn {
                    return Err(format!(
                        "P{point} was withdrawn — there is nothing left to rule on"
                    ));
                }
                if p.review.as_ref().is_some_and(|r| !r.closed) {
                    return Err(format!(
                        "P{point} is under an open deadlock review — let that finish first"
                    ));
                }
                let carries = *ruling == tools::Ruling::Carries;
                p.ruling = Some(PointRuling {
                    by: id.to_string(),
                    round,
                    carries,
                    reason: reason.clone(),
                });
                let word = if carries { "carries" } else { "falls" };
                emit_point(&mut b, *point);
                Ok(format!("{label} ruled P{point} {word} — {reason}"))
            }

            Action::ExtensionBallot { grant, reason } => Err(format!(
                "there is no extension on the floor to vote on — you would have {}",
                if *grant { "granted it" } else { "declined it" }
            ) + &reason
                .as_ref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()),

            Action::Adjourn { .. } => {
                Err("adjournment motions run through confirmation, never here".into())
            }
            Action::AdjournBallot { .. } => {
                Err("adjourn_ballot is only legal while answering an adjournment motion".into())
            }
            Action::ProposeExtension { .. } => Err(
                "only the Summarizer proposes extra rounds, between rounds — never mid-turn".into(),
            ),
            Action::Conclude { .. } => {
                Err("only the Summarizer answers the motion, once the council has closed".into())
            }
            Action::DeclareSettled { .. } => Err(
                "only the Summarizer settles the board, between rounds — never mid-turn".into(),
            ),
        }
    }

    /// Validate and open a deadlock review. Only deadlocked points qualify:
    /// reviewing a carried point is theatre, and an open point needs votes
    /// first, not a ruling on why it is stuck.
    fn open_review(
        &self,
        id: &str,
        round: u32,
        point_id: PointId,
        reason: Option<String>,
    ) -> Result<(PointId, String), String> {
        let (label, status) = {
            let b = self.board.lock().expect("board");
            let label = b
                .delegate(id)
                .map(|d| d.label.clone())
                .unwrap_or_else(|| id.to_string());
            let status = match b.point(point_id) {
                Some(p) => p.tally(&b.ids).status,
                None => return Err(format!("P{point_id} does not exist")),
            };
            (label, status)
        };
        match status {
            PointStatus::Stalemate => {}
            PointStatus::Open => {
                return Err(format!(
                    "P{point_id} has no votes yet — vote on it before reviewing it"
                ));
            }
            PointStatus::Withdrawn => {
                return Err(format!("P{point_id} was withdrawn — nothing to review"));
            }
            other => {
                return Err(format!(
                    "P{point_id} is {} — only deadlocked points go under review",
                    match other {
                        PointStatus::Carried => "carried",
                        PointStatus::Fell => "fell",
                        _ => "decided",
                    }
                ));
            }
        }
        // A fresh review supersedes any earlier one: sides may change their
        // minds, including about why they are stuck.
        let line = match &reason {
            Some(r) => format!("{label} opened a deadlock review on P{point_id} ({r})"),
            None => format!("{label} opened a deadlock review on P{point_id}"),
        };
        {
            let mut b = self.board.lock().expect("board");
            let Some(p) = b.point_mut(point_id) else {
                return Err(format!("P{point_id} does not exist"));
            };
            p.review = Some(StalemateReview {
                point: point_id,
                opened_by: id.to_string(),
                round,
                ballots: vec![],
                closed: false,
                outcome: None,
            });
            b.note(id, round, line.clone());
        }
        let (point, tally, resolved, review) = {
            let b = self.board.lock().expect("board");
            let Some(p) = b.point(point_id) else {
                return Err(format!("P{point_id} does not exist"));
            };
            let tally = p.tally(&b.ids);
            (
                p.clone(),
                tally.clone(),
                tally.decided(),
                p.review.clone().expect("just opened"),
            )
        };
        self.sink.emit(EngineEvent::PointUpdated {
            point,
            tally,
            resolved,
        });
        self.sink.emit(EngineEvent::ReviewOpened {
            point_id,
            review,
        });
        Ok((point_id, line))
    }

    // -- polls -----------------------------------------------------------

    async fn run_ballot(&self, poll_id: PollId, opener_turn: TurnId) {
        let (question, options, round, opener, voters) = {
            let b = self.board.lock().expect("board");
            let Some(p) = b.polls.iter().find(|p| p.id == poll_id) else {
                return;
            };
            (
                p.question.clone(),
                p.options.clone(),
                p.round,
                p.opened_by.clone(),
                b.voting_ids()
                    .into_iter()
                    .filter(|i| *i != p.opened_by)
                    .collect::<Vec<_>>(),
            )
        };

        for voter in voters {
            if self.cancelled() {
                break;
            }
            let outcome = self
                .cast_ballot(&voter, &question, &options, round, &opener, opener_turn)
                .await;
            if let Some((choice, reason, confidence)) = outcome {
                let result = {
                    let mut b = self.board.lock().expect("board");
                    b.note(
                        &voter,
                        round,
                        format!("ballot on \"{question}\": {choice}"),
                    );
                    let Some(p) = b.polls.iter_mut().find(|p| p.id == poll_id) else {
                        break;
                    };
                    p.ballots.push(PollBallot {
                        delegate: voter.clone(),
                        choice: choice.clone(),
                        reason: reason.clone(),
                        confidence,
                    });
                    p.result()
                };
                self.sink.emit(EngineEvent::PollBallotCast {
                    poll_id,
                    delegate: voter.clone(),
                    choice,
                    result,
                });
            }
        }

        let (poll, outcome) = {
            let mut b = self.board.lock().expect("board");
            b.open_poll = None;
            let Some(p) = b.polls.iter_mut().find(|p| p.id == poll_id) else {
                return;
            };
            p.closed = true;
            (p.clone(), p.outcome())
        };
        let summary = match &outcome {
            Some(win) => format!("carried: {win}"),
            None => {
                // "Inconclusive" hid two different facts, and the operator could
                // not tell a question nobody answered from a question the
                // council was split on. The opener does not vote on their own
                // question, so a three-seat council polls two seats and a dead
                // heat is the *expected* outcome, not the surprising one.
                let counts = poll.result();
                let cast: u32 = counts.iter().map(|(_, n)| n).sum();
                let top = counts.iter().map(|(_, n)| *n).max().unwrap_or(0);
                let seats = self.board.lock().expect("board").voting_ids().len();
                match (cast, top * 2 <= cast) {
                    (0, _) => format!("no seat answered — nothing was decided"),
                    (c, true) => format!(
                        "no majority — {c} of {seats} seats answered and split evenly"
                    ),
                    (c, false) => format!("no majority — {c} of {seats} seats answered"),
                }
            }
        };
        {
            let mut b = self.board.lock().expect("board");
            b.note(
                "council",
                round,
                format!("poll \"{question}\" closed — {summary}"),
            );
        }
        self.sink.emit(EngineEvent::PollClosed {
            poll: poll.clone(),
            outcome,
        });
    }

    async fn cast_ballot(
        &self,
        voter: &str,
        question: &str,
        options: &[String],
        round: u32,
        opener: &str,
        opener_turn: TurnId,
    ) -> Option<(String, Option<String>, Option<f64>)> {
        let (messages, endpoint, model, temperature, native, label) = {
            let mut b = self.board.lock().expect("board");
            let me = b.delegate(voter)?.clone();
            let ep = b.endpoint_for(voter)?;
            let native = ep.native_tools;
            let mut history = b
                .history
                .remove(voter)
                .unwrap_or_else(|| {
                    vec![
                        ChatMessage::system(prompt::system_prompt(
                            &me,
                            &b.config,
                            native,
                            me.seat_role == SeatRole::Judge,
                        )),
                        ChatMessage::user(prompt::opening_message(&b.config)),
                    ]
                });
            history.push(ChatMessage::user(prompt::poll_ballot_prompt(
                &Poll {
                    id: 0,
                    question: question.to_string(),
                    options: options.to_vec(),
                    opened_by: opener.to_string(),
                    round,
                    ballots: vec![],
                    closed: false,
                },
                opener,
            )));
            b.history.insert(voter.to_string(), history.clone());
            (
                history,
                ep,
                me.model.clone(),
                me.temperature.or(Some(0.4)),
                native,
                me.label.clone(),
            )
        };

        let tools = if native { tools::ballot_tool() } else { Vec::new() };
        let req = ChatRequest {
            endpoint: self.endpoint_for_request(&endpoint),
            model,
            messages,
            tools: tools.clone(),
            tool_choice: if tools.is_empty() { "none".into() } else { "auto".into() },
            temperature,
            // Uncapped: a reasoning model spends most of a small budget on
            // thinking, and a ballot cut off mid-string does not merely look
            // short — it stops parsing, so the seat is silently counted as
            // having abstained. A lost vote distorts the majority.
            max_tokens: None,
        };
        // Ballots are classification, not deliberation. When a decision model
        // is configured it answers here directly — no turn burned per voter —
        // and a failure falls back to the seat's own model below.
        // Cloned out of the lock first: holding a std guard across the
        // classify await would poison Send.
        let ext = self.decider.lock().expect("decider").clone();
        if let Some(ext) = ext {
            let context = {
                let b = self.board.lock().expect("board");
                Self::ballot_context(&b)
            };
            let label = label.clone();
            let req = ClassifyRequest {
                question: question.to_string(),
                options: options.to_vec(),
                context,
                voter: label.clone(),
            };
            // Routed through the same machinery as every other request: the
            // decision path used to bypass it entirely, which left Skip dead
            // and Stop unable to abort a hung classifier.
            match self.race_classify(&ext, &req, opener_turn, voter).await {
                Outcome::Done(Ok(d)) => return Some((d.choice, None, d.confidence)),
                Outcome::Done(Err(e)) => {
                    self.sink.emit(EngineEvent::Notice {
                        text: format!(
                            "{label}'s decision model failed ({}) — falling back to their seat",
                            describe_llm_error(e)
                        ),
                        tone: NoticeTone::Warn,
                    });
                }
                Outcome::Skipped => return None,
            }
        }

        // A ballot is a full model call, and it used to run with no progress
        // events and no skip hatch — a hung ballot froze the whole debate on
        // the opener's finished-looking turn. Never again.
        let chunks = Arc::new(AtomicUsize::new(0));
        let on_chunk = |_: ChatChunk<'_>| {
            chunks.fetch_add(1, Ordering::Relaxed);
        };
        let outcome = self
            .race_request(&req, opener_turn, voter, &chunks, &on_chunk)
            .await;
        let resp = match outcome {
            Outcome::Done(r) => match r {
                Ok(resp) => resp,
                Err(e) => {
                    self.sink.emit(EngineEvent::Notice {
                        text: format!("{label} never cast their ballot: {}", describe_llm_error(e)),
                        tone: NoticeTone::Warn,
                    });
                    return None;
                }
            },
            Outcome::Skipped => {
                self.sink.emit(EngineEvent::Notice {
                    text: format!("{label}'s ballot was skipped."),
                    tone: NoticeTone::Info,
                });
                return None;
            }
        };
        for note in &resp.notes {
            self.sink.emit(EngineEvent::Notice {
                text: format!("{label}: {note}"),
                tone: NoticeTone::Info,
            });
        }
        self.record_ballot(voter, &resp, "poll_ballot");

        // Votes the seat cast alongside its answer are still votes.
        self.apply_ballot_companions(voter, round, opener_turn, &resp, "poll_ballot")
            .await;
        let action = Self::ballot_action(&resp, "poll_ballot");
        let raw = match action {
            Some(Action::PollBallot { choice, reason }) => Some((choice, reason)),
            _ => self.option_named_in_prose(&resp, options),
        };
        // The poll was the one ballot kind that never said anything when a seat
        // failed to answer, so a lost vote and a deliberate silence looked
        // identical on the record — the exact confusion `unreadable_ballot`
        // exists to prevent.
        let Some((choice, reason)) = raw else {
            if !resp.content.trim().is_empty() || !resp.reasoning.trim().is_empty() {
                self.sink.emit(EngineEvent::Notice {
                    text: format!(
                        "{label} did not answer the poll — {}",
                        Self::unreadable_ballot(&resp)
                    ),
                    tone: NoticeTone::Warn,
                });
            }
            return None;
        };
        // Snap to a real option; models paraphrase.
        let choice = options
            .iter()
            .find(|o| {
                o.eq_ignore_ascii_case(&choice)
                    || o.to_ascii_lowercase().contains(&choice.to_ascii_lowercase())
                    || choice.to_ascii_lowercase().contains(&o.to_ascii_lowercase())
            })
            .cloned()
            .unwrap_or(choice);
        Some((choice, reason, None))
    }

    /// What a voter gets to ground a ballot: the board plus the tail
    /// of the record. Deliberation stays with the seats; this is just enough
    /// context to answer honestly.
    fn ballot_context(b: &Board) -> String {
        let board = prompt::board_snapshot(&b.points, &b.config.delegates);
        let tail: Vec<String> = b
            .record
            .iter()
            .rev()
            .take(10)
            .map(|e| {
                let who = b
                    .delegate(&e.delegate)
                    .map(|d| d.label.clone())
                    .unwrap_or_else(|| e.delegate.clone());
                format!("R{} {}: {}", e.round, who, e.text)
            })
            .collect();
        let mut ctx = format!("{board}\n\nRECENT RECORD (newest last):\n{}", tail.iter().rev().cloned().collect::<Vec<_>>().join("\n"));
        while ctx.chars().count() > 2_500 {
            let cut = ctx
                .char_indices()
                .nth(ctx.chars().count() - 2_500 + 1)
                .map(|(i, _)| i)
                .unwrap_or(0);
            ctx = ctx[cut..].to_string();
        }
        ctx
    }

    // -- deadlock reviews --------------------------------------------------

    async fn run_review(&self, point_id: PointId, opener_turn: TurnId) {
        let (round, opener, voters) = {
            let b = self.board.lock().expect("board");
            let Some(p) = b.point(point_id) else {
                return;
            };
            let review = match p.review.as_ref() {
                Some(r) => r.clone(),
                None => return,
            };
            (
                review.round,
                review.opened_by.clone(),
                b.voting_ids()
                    .into_iter()
                    .filter(|i| *i != review.opened_by)
                    .collect::<Vec<_>>(),
            )
        };

        for voter in voters {
            if self.cancelled() {
                break;
            }
            let outcome = self
                .cast_review_ballot(&voter, point_id, round, &opener, opener_turn)
                .await;
            if let Some((reading, reason, vote)) = outcome {
                let (result, label) = {
                    let mut b = self.board.lock().expect("board");
                    let label = b
                        .delegate(&voter)
                        .map(|d| d.label.clone())
                        .unwrap_or_else(|| voter.clone());
                    // A changed vote lands immediately: the review is the
                    // sanctioned moment to change your mind, and the tally
                    // moves while everyone watches.
                    if let Some(v) = vote {
                        if let Some(p) = b.point_mut(point_id) {
                            p.votes.insert(
                                voter.clone(),
                                VoteRecord {
                                    delegate: voter.clone(),
                                    verdict: v,
                                    reason: reason.clone(),
                                    round,
                                },
                            );
                            p.conceded_by.remove(&voter);
                        }
                    }
                    let result = {
                        let Some(p) = b.point_mut(point_id) else {
                            break;
                        };
                        let Some(r) = p.review.as_mut() else {
                            break;
                        };
                        r.ballots.retain(|x| x.delegate != voter);
                        r.ballots.push(StalemateBallot {
                            delegate: voter.clone(),
                            reading,
                            reason: reason.clone(),
                        });
                        r.result()
                    };
                    b.note(
                        &voter,
                        round,
                        format!(
                            "rules P{point_id} {}",
                            reading.as_str()
                        ),
                    );
                    (result, label)
                };
                self.sink.emit(EngineEvent::ReviewBallotCast {
                    point_id,
                    delegate: voter.clone(),
                    reading,
                    result,
                });
                self.sink.emit(EngineEvent::Notice {
                    text: format!("{label} rules P{point_id} {}", reading.as_str()),
                    tone: NoticeTone::Info,
                });
                self.emit_point(point_id);
            }
        }

        let summary = {
            let mut b = self.board.lock().expect("board");
            let Some(p) = b.point_mut(point_id) else {
                return;
            };
            let Some(r) = p.review.as_mut() else {
                return;
            };
            r.closed = true;
            r.outcome = r.outcome_of();
            let review = r.clone();
            let outcome = review.outcome;
            let summary = match outcome {
                Some(o) => format!("ruled {}", o.as_str()),
                None => "inconclusive — no majority".to_string(),
            };
            let round = b.round;
            b.note(
                "council",
                round,
                format!("review on P{point_id} closed — {summary}"),
            );
            (review, outcome, summary)
        };
        self.emit_point(point_id);
        self.sink.emit(EngineEvent::ReviewClosed {
            point_id,
            review: summary.0,
            outcome: summary.1,
        });
        self.sink.emit(EngineEvent::Notice {
            text: format!("Review on P{point_id} closed — {}", summary.2),
            tone: NoticeTone::Info,
        });
    }

    /// Re-emit a point's update after out-of-band mutations (review ballots).
    fn emit_point(&self, point_id: PointId) {
        let found = {
            let b = self.board.lock().expect("board");
            b.point(point_id).map(|p| {
                let tally = p.tally(&b.ids);
                (p.clone(), tally.clone(), tally.decided())
            })
        };
        if let Some((point, tally, resolved)) = found {
            self.sink.emit(EngineEvent::PointUpdated {
                point,
                tally,
                resolved,
            });
        }
    }

    async fn cast_review_ballot(
        &self,
        voter: &str,
        point_id: PointId,
        round: u32,
        opener: &str,
        opener_turn: TurnId,
    ) -> Option<(StalemateReading, Option<String>, Option<Verdict>)> {
        let (messages, endpoint, model, temperature, native, label) = {
            let mut b = self.board.lock().expect("board");
            let me = b.delegate(voter)?.clone();
            let ep = b.endpoint_for(voter)?;
            let native = ep.native_tools;
            let point_text = b
                .point(point_id)
                .map(|p| p.text.clone())
                .unwrap_or_default();
            let opener_label = b
                .delegate(opener)
                .map(|d| d.label.clone())
                .unwrap_or_else(|| opener.to_string());
            let mut history = b
                .history
                .remove(voter)
                .unwrap_or_else(|| {
                    vec![
                        ChatMessage::system(prompt::system_prompt(
                            &me,
                            &b.config,
                            native,
                            me.seat_role == SeatRole::Judge,
                        )),
                        ChatMessage::user(prompt::opening_message(&b.config)),
                    ]
                });
            history.push(ChatMessage::user(prompt::review_ballot_prompt(
                point_id,
                &point_text,
                &opener_label,
                round,
            )));
            b.history.insert(voter.to_string(), history.clone());
            (
                history,
                ep,
                me.model.clone(),
                me.temperature.or(Some(0.4)),
                native,
                me.label.clone(),
            )
        };

        let tools = if native {
            tools::review_tool()
        } else {
            Vec::new()
        };
        let req = ChatRequest {
            endpoint: self.endpoint_for_request(&endpoint),
            model,
            messages,
            tools: tools.clone(),
            tool_choice: if tools.is_empty() {
                "none".into()
            } else {
                "auto".into()
            },
            temperature,
            // Uncapped: a reasoning model spends most of a small budget on
            // thinking, and a ballot cut off mid-string does not merely look
            // short — it stops parsing, so the seat is silently counted as
            // having abstained. A lost vote distorts the majority.
            max_tokens: None,
        };
        // Keyed to the opener's turn like poll ballots: their speech has not
        // finished, so this keeps their clock ticking instead of freezing.
        let chunks = Arc::new(AtomicUsize::new(0));
        let on_chunk = |_: ChatChunk<'_>| {
            chunks.fetch_add(1, Ordering::Relaxed);
        };
        let outcome = self
            .race_request(&req, opener_turn, voter, &chunks, &on_chunk)
            .await;
        let resp = match outcome {
            Outcome::Done(r) => match r {
                Ok(resp) => resp,
                Err(e) => {
                    self.sink.emit(EngineEvent::Notice {
                        text: format!(
                            "{label} never ruled on P{point_id}: {}",
                            describe_llm_error(e)
                        ),
                        tone: NoticeTone::Warn,
                    });
                    return None;
                }
            },
            Outcome::Skipped => {
                self.sink.emit(EngineEvent::Notice {
                    text: format!("{label}'s ruling on P{point_id} was skipped."),
                    tone: NoticeTone::Info,
                });
                return None;
            }
        };
        for note in &resp.notes {
            self.sink.emit(EngineEvent::Notice {
                text: format!("{label}: {note}"),
                tone: NoticeTone::Info,
            });
        }
        self.record_ballot(voter, &resp, "stalemate_ballot");

        // Votes the seat cast alongside its answer are still votes.
        self.apply_ballot_companions(voter, round, opener_turn, &resp, "stalemate_ballot")
            .await;
        let action = Self::ballot_action(&resp, "stalemate_ballot");
        match action {
            Some(Action::StalemateBallot {
                reading,
                reason,
                vote,
            }) => Some((reading, reason, vote)),
            _ => {
                if !resp.content.trim().is_empty() || !resp.reasoning.trim().is_empty() {
                    self.sink.emit(EngineEvent::Notice {
                        text: format!(
                            "{label} did not rule on P{point_id} — {}",
                            Self::unreadable_ballot(&resp)
                        ),
                        tone: NoticeTone::Warn,
                    });
                }
                None
            }
        }
    }

    /// The stage between rounds belongs to the summarizer: it lays the round
    /// out on the record, and may once per intermission ask the operator for
    /// more rounds. A proposal suspends the loop until `resolve_extension`
    /// answers. Skippable and never fatal — a failed intermission loses
    /// context, not the debate.
    async fn run_intermission(&self, round: u32, may_propose: bool) {
        let (me_id, planned, extra_left) = {
            let b = self.board.lock().expect("board");
            let Some(me) = b
                .config
                .delegates
                .iter()
                .find(|d| d.seat_role == SeatRole::Summarizer)
                .cloned()
            else {
                // Convene-time validation guarantees this; the return is
                // belt-and-braces against a hand-built config.
                return;
            };
            // A settled board needs no rounds, so it gets no budget to ask
            // for with. Saying so beats silently withholding the tool.
            let left = if may_propose { b.extra_left() } else { 0 };
            (me.id, b.config.max_rounds.max(1), left)
        };
        let (endpoint, label, color, model, temperature) = {
            let b = self.board.lock().expect("board");
            let Some(me) = b.delegate(&me_id).cloned() else {
                return;
            };
            let Some(ep) = b.endpoint_for(&me_id) else {
                self.sink.emit(EngineEvent::Notice {
                    text: format!("{} has no endpoint configured", me.label),
                    tone: NoticeTone::Warn,
                });
                return;
            };
            (
                ep,
                me.label,
                me.color,
                me.model,
                me.temperature.or(Some(0.6)),
            )
        };
        let board_text = {
            let b = self.board.lock().expect("board");
            prompt::board_snapshot(&b.points, &b.config.delegates)
        };
        let record_tail = {
            let b = self.board.lock().expect("board");
            let mut tail: Vec<String> = b
                .record
                .iter()
                .rev()
                .take(60)
                .map(|e| {
                    let who = b
                        .delegate(&e.delegate)
                        .map(|d| d.label.clone())
                        .unwrap_or_else(|| e.delegate.clone());
                    format!("R{} {}: {}", e.round, who, e.text)
                })
                .collect();
            tail.reverse();
            let mut s = tail.join("\n");
            while s.chars().count() > 6_000 {
                let cut = s
                    .char_indices()
                    .nth(s.chars().count() - 6_000 + 1)
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                s = s[cut..].to_string();
            }
            s
        };
        let messages = {
            let mut b = self.board.lock().expect("board");
            let mut history = b.history.remove(&me_id).unwrap_or_default();
            if history.is_empty() {
                history.push(ChatMessage::system(prompt::summarizer_system(
                    &label,
                    &b.config.motion,
                )));
            }
            history.push(ChatMessage::user(format!(
                "{board_text}\n\nRECENT RECORD:\n{record_tail}\n\n———\n\n{}",
                prompt::intermission_prompt(round, planned, extra_left)
            )));
            b.history.insert(me_id.clone(), history.clone());
            history
        };

        let turn = {
            let mut b = self.board.lock().expect("board");
            let t = b.next_turn;
            b.next_turn += 1;
            t
        };
        self.sink.emit(EngineEvent::TurnStarted {
            turn,
            delegate: me_id.clone(),
            label: label.clone(),
            color,
            model: model.clone(),
            round,
        });
        self.sink.emit(EngineEvent::IntermissionStarted { round });

        let native = endpoint.native_tools;
        let request_tools = if native {
            tools::summarizer_tools()
        } else {
            Vec::new()
        };
        let req = ChatRequest {
            endpoint: self.endpoint_for_request(&endpoint),
            model,
            messages,
            tools: request_tools.clone(),
            tool_choice: if request_tools.is_empty() {
                "none".into()
            } else {
                "auto".into()
            },
            temperature,
            // Uncapped for the same reason speaking turns are: a reasoning model
            // spends most of a small budget on thinking, and a summary cut at
            // 600 tokens lands in the sidebar as a half-finished sentence.
            max_tokens: None,
        };
        let content_pump = Pump::content(self.sink.as_ref(), turn);
        let thought_pump = Pump::thought(self.sink.as_ref(), turn);
        let chunks = Arc::new(AtomicUsize::new(0));
        let on_chunk = |chunk: ChatChunk<'_>| {
            chunks.fetch_add(1, Ordering::Relaxed);
            match chunk {
                ChatChunk::Content(c) => content_pump.push(c),
                ChatChunk::Reasoning(r) => thought_pump.push(r),
                ChatChunk::Tool(_) => {}
                ChatChunk::Reset => {
                    content_pump.reset();
                    thought_pump.reset();
                }
            }
        };
        let outcome = self.race_request(&req, turn, &me_id, &chunks, &on_chunk).await;
        content_pump.flush(true);
        thought_pump.flush(true);
        let resp = match outcome {
            Outcome::Done(r) => match r {
                Ok(resp) => resp,
                Err(e) => {
                    self.sink.emit(EngineEvent::Notice {
                        text: format!(
                            "{label}'s round {round} summary failed: {}",
                            describe_llm_error(e)
                        ),
                        tone: NoticeTone::Warn,
                    });
                    return;
                }
            },
            Outcome::Skipped => {
                self.sink.emit(EngineEvent::TurnSkipped {
                    turn,
                    delegate: me_id.clone(),
                    label: label.clone(),
                });
                return;
            }
        };
        self.note_truncation(&resp, &label, &format!("round {round} summary"));
        let text = resp.content.clone();
        let thinking = resp.reasoning.chars().take(4_000).collect::<String>();
        let mut text = text;
        let calls = if native && !resp.tool_calls.is_empty() {
            resp.tool_calls
        } else {
            // Reasoning-first models often leave their blocks in the thinking
            // rather than the answer. Read both, and show only what was said:
            // a summary with the request JSON pasted under it read as a
            // rendering fault rather than as a request.
            let thought_lines = resp.reasoning.lines().count();
            let combined = format!("{}\n{}", resp.reasoning, text);
            let scraped = tools::scrape_inline_actions(&combined);
            text = scraped.text_from(thought_lines);
            scraped.calls
        };

        // The summary itself is the job, so the action row names it first;
        // anything the summarizer tried beyond that follows.
        let mut actions_log = vec![format!("round {round} summary")];
        let mut proposed = false;
        for call in &calls {
            match tools::parse_action(call) {
                Ok(Action::DeclareSettled { reason }) => {
                    // Its own words for why the record has converged. This is the
                    // one place a debate ends on a reading rather than a count, so
                    // it is worth taking verbatim.
                    let line = {
                        let mut b = self.board.lock().expect("board");
                        b.note(&me_id, round, format!("declared the board settled — {reason}"));
                        b.settled_by = Some((me_id.clone(), reason.clone()));
                        format!("the Summarizer called the board settled: {reason}")
                    };
                    self.sink.emit(EngineEvent::Notice {
                        text: line.clone(),
                        tone: NoticeTone::Info,
                    });
                    self.sink.emit(EngineEvent::ActionTaken {
                        turn,
                        delegate: me_id.clone(),
                        action: "settle".into(),
                        detail: line.clone(),
                    });
                    actions_log.push(line);
                }
                Ok(Action::ProposeExtension { rounds, reason }) => {
                    if proposed {
                        let why = "only one extension proposal per intermission";
                        self.sink.emit(EngineEvent::ActionRejected {
                            turn,
                            delegate: me_id.clone(),
                            tool: "propose_extension".into(),
                            reason: why.into(),
                        });
                        actions_log.push(format!("(rejected extension: {why})"));
                        continue;
                    }
                    proposed = true;
                    match self.open_extension(round, rounds, &reason, turn).await {
                        Ok(outcome) => {
                            actions_log.push(Self::extension_line(&rounds, &reason, &outcome))
                        }
                        Err(why) => {
                            self.sink.emit(EngineEvent::ActionRejected {
                                turn,
                                delegate: me_id.clone(),
                                tool: "propose_extension".into(),
                                reason: why.clone(),
                            });
                            actions_log.push(format!("(rejected extension: {why})"));
                        }
                    }
                }
                Ok(other) => {
                    // Accurate for this phase: between rounds the summarizer's
                    // only instrument is the proposal. It gets `conclude` once,
                    // after the council has closed.
                    let why = "between rounds the summarizer's only instrument is propose_extension";
                    self.sink.emit(EngineEvent::ActionRejected {
                        turn,
                        delegate: me_id.clone(),
                        tool: other.name().into(),
                        reason: why.into(),
                    });
                    actions_log.push(format!("(rejected {}: {why})", other.name()));
                }
                Err(why) => {
                    self.sink.emit(EngineEvent::ActionRejected {
                        turn,
                        delegate: me_id.clone(),
                        tool: call.name.clone(),
                        reason: why.clone(),
                    });
                    actions_log.push(format!("(rejected {}: {why})", call.name));
                }
            }
        }

        let speech = Speech {
            id: turn,
            delegate: me_id.clone(),
            round,
            text,
            reasoning: thinking,
            actions: actions_log,
            phase: TurnPhase::Done,
        };
        {
            let mut b = self.board.lock().expect("board");
            if !speech.text.trim().is_empty() {
                b.note(
                    &me_id,
                    round,
                    format!("ROUND {round} SUMMARY: {}", speech.text.trim()),
                );
            }
            let hist = b.history.entry(me_id.clone()).or_default();
            hist.push(ChatMessage::assistant(speech.text.clone()));
            if hist.len() > 12 {
                let head: Vec<ChatMessage> = hist.drain(..2).collect();
                let tail: Vec<ChatMessage> = hist.split_off(hist.len().saturating_sub(8));
                *hist = head.into_iter().chain(tail).collect();
            }
            b.speeches.push(speech.clone());
        }
        self.sink.emit(EngineEvent::SpeechFinished { turn, speech });
    }

    /// The line an extension outcome puts in the action log.
    fn extension_line(ask: &u32, reason: &str, outcome: &ExtensionOutcome) -> String {
        match outcome {
            ExtensionOutcome::Granted { source, .. } => {
                format!("proposed {ask} extra rounds ({reason}) — granted by {source}")
            }
            ExtensionOutcome::Declined { source } => {
                format!("proposed {ask} extra rounds ({reason}) — declined by {source}")
            }
        }
    }

    /// Validate a summarizer proposal, put it to the operator, and suspend
    /// until answered. Returns a line for the intermission's action log.
    async fn open_extension(
        &self,
        round: u32,
        rounds: u32,
        reason: &str,
        opener_turn: TurnId,
    ) -> Result<ExtensionOutcome, String> {
        let label = {
            let b = self.board.lock().expect("board");
            b.delegate(
                b.summarizer_id()
                    .map(|s| s.as_str())
                    .unwrap_or_default(),
            )
            .map(|d| d.label.clone())
            .unwrap_or_else(|| "Summarizer".into())
        };
        let left = self.board.lock().expect("board").extra_left();
        if left == 0 {
            return Err("the extension budget is exhausted — summarize as usual".into());
        }
        let ask = rounds.min(left);
        if ask < rounds {
            self.sink.emit(EngineEvent::Notice {
                text: format!(
                    "{label} asked for {rounds} extra rounds but only {left} remain — clamped to {ask}"
                ),
                tone: NoticeTone::Info,
            });
        }
        *self.extension_pending.lock().expect("extension") = Some(ExtensionPending { rounds: ask });
        *self.extension_answer.lock().expect("extension") = None;
        self.sink.emit(EngineEvent::ExtensionProposed {
            round,
            rounds: ask,
            reason: reason.to_string(),
        });
        // The council votes, and the operator may settle it at any moment while
        // that happens. Bounded throughout: without a deadline the debate sits
        // waiting on round trips, and with Stop as the only exit Stop throws
        // away the whole run. An unanswered request is simply declined.
        let voted = tokio::time::timeout(
            Duration::from_secs(EXTENSION_WAIT_SECS),
            self.run_extension_vote(round, ask, reason, opener_turn),
        )
        .await
        .ok()
        .flatten();

        // The operator's answer, if they gave one, is the answer.
        let operator = self.extension_answer.lock().expect("extension").take();
        let (approved, granted, source) = match operator {
            Some((ok, n)) => (ok, n, "you"),
            None => match voted {
                Some((ok, src)) => (ok, ask, src),
                None => {
                    if self.cancelled() {
                        (false, 0, "the debate was stopped")
                    } else {
                        self.extension_pending.lock().expect("extension").take();
                        self.sink.emit(EngineEvent::Notice {
                            text: "No answer on the extension request — continuing as planned."
                                .into(),
                            tone: NoticeTone::Info,
                        });
                        (false, 0, "nobody answered")
                    }
                }
            },
        };
        self.extension_pending.lock().expect("extension").take();
        if approved && granted > 0 && !self.cancelled() {
            let (added, total, budget_left) = {
                let mut b = self.board.lock().expect("board");
                let added = granted.min(b.extra_left());
                b.config.max_rounds = b.config.max_rounds.saturating_add(added);
                b.extra_granted += added;
                let line = format!(
                    "extended by {added} rounds (now {} total)",
                    b.config.max_rounds
                );
                b.note("council", round, line);
                (
                    added,
                    b.config.max_rounds,
                    b.config.max_extra_rounds.saturating_sub(b.extra_granted),
                )
            };
            if added == 0 {
                self.sink.emit(EngineEvent::ExtensionDeclined { round });
                return Ok(ExtensionOutcome::Declined { source: "the budget" });
            }
            self.sink.emit(EngineEvent::ExtensionGranted {
                added,
                total,
                budget_left,
            });
            self.sink.emit(EngineEvent::Notice {
                text: format!("{label}: extended by {added} on {source}'s decision — {total} rounds"),
                tone: NoticeTone::Info,
            });
            Ok(ExtensionOutcome::Granted {
                added,
                source,
            })
        } else {
            self.sink.emit(EngineEvent::ExtensionDeclined { round });
            if !self.cancelled() {
                self.sink.emit(EngineEvent::Notice {
                    text: format!("Extension declined by {source} — summarizing as usual."),
                    tone: NoticeTone::Info,
                });
            }
            Ok(ExtensionOutcome::Declined { source })
        }
    }

    /// The end result, written by the summarizer after the last round for the
    /// sidebar. Skipped when the operator stopped the debate — they asked out,
    /// not for one more call.
    ///
    /// `may_propose` is true only when the board deadlocked and this debate has
    /// not already spent its end-of-debate extension. The third element of the
    /// return says the operator granted more rounds, so the debate resumes.
    async fn conclusion(
        &self,
        may_propose: bool,
    ) -> (Option<String>, Option<Conclusion>, bool) {
        if self.cancelled() {
            return (None, None, false);
        }
        let (me_id, round) = {
            let b = self.board.lock().expect("board");
            let Some(me) = b
                .config
                .delegates
                .iter()
                .find(|d| d.seat_role == SeatRole::Summarizer)
                .cloned()
            else {
                return (None, None, false);
            };
            (me.id, b.round)
        };
        let (endpoint, label, color, model, temperature) = {
            let b = self.board.lock().expect("board");
            let (Some(me), Some(ep)) = (b.delegate(&me_id).cloned(), b.endpoint_for(&me_id))
            else {
                return (None, None, false);
            };
            (
                ep,
                me.label,
                me.color,
                me.model,
                me.temperature.or(Some(0.6)),
            )
        };
        let board_text = {
            let b = self.board.lock().expect("board");
            prompt::board_snapshot(&b.points, &b.config.delegates)
        };
        let tally_line = self.tally_line();
        let prompt_text = {
            let b = self.board.lock().expect("board");
            prompt::conclusion_prompt(&b.config, may_propose, &tally_line)
        };
        let turn = {
            let mut b = self.board.lock().expect("board");
            let t = b.next_turn;
            b.next_turn += 1;
            t
        };
        self.sink.emit(EngineEvent::TurnStarted {
            turn,
            delegate: me_id.clone(),
            label: label.clone(),
            color,
            model: model.clone(),
            round,
        });
        let native = {
            let b = self.board.lock().expect("board");
            b.endpoint_for(&me_id).map(|e| e.native_tools).unwrap_or(false)
        };
        let mut answer_tools = if native {
            tools::conclude_tool()
        } else {
            Vec::new()
        };
        // Offered only on a real deadlock, and only once per debate. Not
        // advertised otherwise: a tool whose call is guaranteed to be refused
        // just invites the model to spend a turn arguing for it.
        if may_propose {
            answer_tools.extend(tools::summarizer_tools());
        }
        let motion = self.board.lock().expect("board").config.motion.clone();
        let req = ChatRequest {
            endpoint: self.endpoint_for_request(&endpoint),
            model,
            messages: vec![
                ChatMessage::system(prompt::summarizer_system(&label, &motion)),
                ChatMessage::user(format!("{board_text}\n\n———\n\n{prompt_text}")),
            ],
            tools: answer_tools.clone(),
            tool_choice: if answer_tools.is_empty() {
                "none".into()
            } else {
                "auto".into()
            },
            temperature,
            // Uncapped: this is the end result the sidebar shows, and a result
            // that stops mid-sentence is worse than a long one.
            max_tokens: None,
        };
        let content_pump = Pump::content(self.sink.as_ref(), turn);
        let thought_pump = Pump::thought(self.sink.as_ref(), turn);
        let chunks = Arc::new(AtomicUsize::new(0));
        let on_chunk = |chunk: ChatChunk<'_>| {
            chunks.fetch_add(1, Ordering::Relaxed);
            match chunk {
                ChatChunk::Content(c) => content_pump.push(c),
                ChatChunk::Reasoning(r) => thought_pump.push(r),
                ChatChunk::Tool(_) => {}
                ChatChunk::Reset => {
                    content_pump.reset();
                    thought_pump.reset();
                }
            }
        };
        let outcome = self.race_request(&req, turn, &me_id, &chunks, &on_chunk).await;
        content_pump.flush(true);
        thought_pump.flush(true);
        let resp = match outcome {
            Outcome::Done(r) => match r {
                Ok(resp) => resp,
                Err(e) => {
                    self.sink.emit(EngineEvent::Notice {
                        text: format!(
                            "{label}'s end result failed: {}",
                            describe_llm_error(e)
                        ),
                        tone: NoticeTone::Warn,
                    });
                    return (None, None, false);
                }
            },
            Outcome::Skipped => {
                self.sink.emit(EngineEvent::TurnSkipped {
                    turn,
                    delegate: me_id.clone(),
                    label: label.clone(),
                });
                return (None, None, false);
            }
        };
        self.note_truncation(&resp, &label, "end result");

        // The summarizer's call, if it made one. Prose-only replies fall through
        // to the deterministic result below rather than being guessed at.
        let mut text = resp.content.trim().to_string();
        let calls = if native && !resp.tool_calls.is_empty() {
            resp.tool_calls.clone()
        } else {
            let thought_lines = resp.reasoning.lines().count();
            let combined = format!("{}\n{}", resp.reasoning, resp.content);
            let scraped = tools::scrape_inline_actions(&combined);
            text = scraped.text_from(thought_lines);
            scraped.calls
        };
        let mut stated = None;
        let mut resumed = false;
        for c in &calls {
            match tools::parse_action(c) {
                Ok(Action::Conclude {
                    agreed,
                    answer,
                    lean,
                }) => stated = Some((agreed, answer, lean)),
                Ok(Action::ProposeExtension { rounds, reason }) if may_propose => {
                    // The deadlock is the whole argument for this: the board
                    // could not decide, and the summarizer is saying more rounds
                    // might. The operator still decides.
                    match self.open_extension(round, rounds, &reason, turn).await {
                        Ok(ExtensionOutcome::Granted { added, source }) => {
                            // The rounds are already on the board's max; this
                            // only decides whether the debate goes on.
                            resumed = added > 0 && !self.cancelled();
                            let _ = source;
                        }
                        Ok(_) => {}
                        Err(why) => {
                            self.sink.emit(EngineEvent::ActionRejected {
                                turn,
                                delegate: me_id.clone(),
                                tool: "propose_extension".into(),
                                reason: why,
                            });
                        }
                    }
                }
                Ok(other) => {
                    self.sink.emit(EngineEvent::ActionRejected {
                        turn,
                        delegate: me_id.clone(),
                        tool: other.name().into(),
                        reason: "not an instrument for this moment".into(),
                    });
                }
                Err(_) => {}
            }
        }

        let thinking = resp.reasoning.chars().take(4_000).collect::<String>();
        let speech = Speech {
            id: turn,
            delegate: me_id.clone(),
            round,
            text: text.clone(),
            reasoning: thinking,
            actions: vec!["final result".into()],
            phase: TurnPhase::Done,
        };
        {
            let mut b = self.board.lock().expect("board");
            b.speeches.push(speech.clone());
        }
        self.sink.emit(EngineEvent::SpeechFinished { turn, speech });

        // The engine does not second-guess the seat's reading, except to catch a
        // claim of consensus on a board that decided nothing at all.
        //
        // It used to require every point to be decided, which meant one deadlocked
        // point among five decisively carried ones reported "the council could not
        // agree" and blanked the answer. That is not a council failing to agree —
        // it is a live dispute about a side issue next to a result the board
        // reached, and throwing the result away loses the thing the debate
        // actually found.
        let nothing_decided = {
            let b = self.board.lock().expect("board");
            let decided = b.points.iter().any(|p| {
                matches!(
                    p.tally(&b.ids).status,
                    PointStatus::Carried | PointStatus::Fell
                )
            });
            b.points.is_empty() || !decided
        };
        let tally_line = self.tally_line();

        let conclusion = match stated {
            Some((claimed, answer, lean)) => {
                if claimed && nothing_decided {
                    // Not allowed to narrate a consensus the board never reached.
                    self.sink.emit(EngineEvent::Notice {
                        text: "the Summarizer claimed agreement on a board that decided \
                                nothing — recorded as unresolved"
                            .into(),
                        tone: NoticeTone::Warn,
                    });
                }
                Some(Conclusion {
                    agreed: claimed && !nothing_decided,
                    answer: if claimed && !nothing_decided { answer } else { String::new() },
                    lean: lean.filter(|l| !l.trim().is_empty()),
                    tally: tally_line,
                })
            }
            None => {
                // No answer given. Say so honestly rather than leaving the
                // section blank, which reads as a bug. If it did ask for more
                // rounds, that is the answer of the moment.
                Some(Conclusion {
                    agreed: false,
                    answer: String::new(),
                    lean: if resumed {
                        Some("still deadlocked — more rounds were requested".into())
                    } else {
                        None
                    },
                    tally: tally_line,
                })
            }
        };

        (if text.is_empty() { None } else { Some(text) }, conclusion, resumed)
    }

    // -- adjournment -----------------------------------------------------

    /// Validate an adjournment motion: nothing already closed, no deadlocked
    /// point without its review, and no motion voted down earlier this round.
    fn open_adjournment(
        &self,
        id: &str,
        round: u32,
        reason: String,
    ) -> Result<String, String> {
        let mut b = self.board.lock().expect("board");
        let label = b
            .delegate(id)
            .map(|d| d.label.clone())
            .unwrap_or_else(|| id.to_string());
        if b.adjourn.is_some() {
            return Err("the council has already been adjourned".into());
        }
        if round < b.adjourn_cooldown_until {
            return Err(format!(
                "adjournment was just voted down — it may be moved again from round {}",
                b.adjourn_cooldown_until
            ));
        }
        // The mandate: no closing on a stalemate without the vote.
        let unreviewed: Vec<PointId> = b
            .points
            .iter()
            .filter(|p| {
                p.tally(&b.ids).status == PointStatus::Stalemate
                    && !p.review.as_ref().map(|r| r.closed).unwrap_or(false)
            })
            .map(|p| p.id)
            .collect();
        if !unreviewed.is_empty() {
            let list = unreviewed
                .iter()
                .map(|x| format!("P{x}"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "{list} deadlocked without review — call `review` on each before adjourning"
            ));
        }
        let line = format!("{label} moved to adjourn — {reason}");
        b.note(id, round, line.clone());
        drop(b);
        self.sink.emit(EngineEvent::AdjournMoved {
            by: id.to_string(),
            reason,
            round,
        });
        Ok(line)
    }

    /// Put the motion to every other seat. The mover counts as yes; closing
    /// takes a strict majority of seats, so abstentions and silence weigh
    /// against it. Returns the line the mover's turn logs.
    async fn run_confirmation(&self, mover: &str, round: u32, opener_turn: TurnId) -> String {
        // A majority of the seats that may vote — the summarizer watches.
        let (mover_label, voters, seats) = {
            let b = self.board.lock().expect("board");
            let voting = b.voting_ids();
            let seats = voting.len();
            (
                b.delegate(mover)
                    .map(|d| d.label.clone())
                    .unwrap_or_else(|| mover.to_string()),
                voting.into_iter().filter(|i| i != mover).collect::<Vec<_>>(),
                seats,
            )
        };
        let mut yes = 1u32;
        let mut no = 0u32;
        for voter in voters {
            if self.cancelled() {
                return "adjournment left undecided — the debate was stopped".into();
            }
            if let Some((confirm, _)) = self
                .cast_confirm_ballot(&voter, mover, round, opener_turn)
                .await
            {
                if confirm {
                    yes += 1;
                } else {
                    no += 1;
                }
                self.sink.emit(EngineEvent::AdjournBallotCast {
                    delegate: voter.clone(),
                    confirm,
                    result: vec![("yes".into(), yes), ("no".into(), no)],
                });
            }
        }
        if self.cancelled() {
            return "adjournment left undecided — the debate was stopped".into();
        }
        let passed = yes * 2 > seats as u32;
        let summary = if passed {
            let mut b = self.board.lock().expect("board");
            b.adjourn = Some(mover.to_string());
            b.adjourn_reason = Some(
                b.record
                    .iter()
                    .rev()
                    .find(|e| e.delegate == mover)
                    .map(|e| e.text.clone())
                    .unwrap_or_default(),
            );
            let line = format!("council agreed to adjourn ({yes}–{no})");
            b.note("council", round, line.clone());
            line
        } else {
            let mut b = self.board.lock().expect("board");
            b.adjourn_cooldown_until = round + 1;
            let line = format!("adjournment voted down ({yes}–{no}) — debate continues");
            b.note("council", round, line.clone());
            line
        };
        let reason = self
            .board
            .lock()
            .expect("board")
            .adjourn_reason
            .clone()
            .unwrap_or_default();
        self.sink.emit(EngineEvent::AdjournDecided {
            passed,
            yes,
            no,
            reason,
        });
        self.sink.emit(EngineEvent::Notice {
            text: format!("{mover_label}: {summary}"),
            tone: if passed {
                NoticeTone::Info
            } else {
                NoticeTone::Warn
            },
        });
        summary
    }

    async fn cast_confirm_ballot(
        &self,
        voter: &str,
        mover: &str,
        round: u32,
        opener_turn: TurnId,
    ) -> Option<(bool, Option<String>)> {
        let (messages, endpoint, model, temperature, native, label) = {
            let mut b = self.board.lock().expect("board");
            let me = b.delegate(voter)?.clone();
            let ep = b.endpoint_for(voter)?;
            let native = ep.native_tools;
            let mover_label = b
                .delegate(mover)
                .map(|d| d.label.clone())
                .unwrap_or_else(|| mover.to_string());
            let motion = b
                .record
                .iter()
                .rev()
                .find(|e| e.delegate == mover)
                .map(|e| e.text.clone())
                .unwrap_or_else(|| "adjourn".to_string());
            let mut history = b
                .history
                .remove(voter)
                .unwrap_or_else(|| {
                    vec![
                        ChatMessage::system(prompt::system_prompt(
                            &me,
                            &b.config,
                            native,
                            me.seat_role == SeatRole::Judge,
                        )),
                        ChatMessage::user(prompt::opening_message(&b.config)),
                    ]
                });
            let context = Self::ballot_context(&b);
            history.push(ChatMessage::user(format!(
                "{context}\n\n———\n\n{}",
                prompt::adjourn_confirm_prompt(&mover_label, &motion, round)
            )));
            b.history.insert(voter.to_string(), history.clone());
            (
                history,
                ep,
                me.model.clone(),
                me.temperature.or(Some(0.4)),
                native,
                me.label.clone(),
            )
        };

        let tools = if native {
            tools::adjourn_tool()
        } else {
            Vec::new()
        };
        let req = ChatRequest {
            endpoint: self.endpoint_for_request(&endpoint),
            model,
            messages,
            tools: tools.clone(),
            tool_choice: if tools.is_empty() {
                "none".into()
            } else {
                "auto".into()
            },
            temperature,
            // Uncapped: a reasoning model spends most of a small budget on
            // thinking, and a ballot cut off mid-string does not merely look
            // short — it stops parsing, so the seat is silently counted as
            // having abstained. A lost vote distorts the majority.
            max_tokens: None,
        };
        // Same keying as polls and reviews: the mover's speech has not
        // finished, so this keeps their clock ticking.
        let chunks = Arc::new(AtomicUsize::new(0));
        let on_chunk = |_: ChatChunk<'_>| {
            chunks.fetch_add(1, Ordering::Relaxed);
        };
        let outcome = self
            .race_request(&req, opener_turn, voter, &chunks, &on_chunk)
            .await;
        let resp = match outcome {
            Outcome::Done(r) => match r {
                Ok(resp) => resp,
                Err(e) => {
                    self.sink.emit(EngineEvent::Notice {
                        text: format!(
                            "{label} never answered the adjournment: {}",
                            describe_llm_error(e)
                        ),
                        tone: NoticeTone::Warn,
                    });
                    return None;
                }
            },
            Outcome::Skipped => {
                self.sink.emit(EngineEvent::Notice {
                    text: format!("{label}'s adjournment ballot was skipped."),
                    tone: NoticeTone::Info,
                });
                return None;
            }
        };
        for note in &resp.notes {
            self.sink.emit(EngineEvent::Notice {
                text: format!("{label}: {note}"),
                tone: NoticeTone::Info,
            });
        }
        self.record_ballot(voter, &resp, "adjourn_ballot");

        // Votes the seat cast alongside its answer are still votes.
        self.apply_ballot_companions(voter, round, opener_turn, &resp, "adjourn_ballot")
            .await;
        let action = Self::ballot_action(&resp, "adjourn_ballot");
        match action {
            Some(Action::AdjournBallot { confirm, reason }) => Some((confirm, reason)),
            _ => {
                // Name the cause. A ballot the server cut off mid-string reads
                // as the seat having abstained, which quietly moves the
                // majority; a seat that declined to answer is a different fact.
                if !resp.content.trim().is_empty() || !resp.reasoning.trim().is_empty() {
                    self.sink.emit(EngineEvent::Notice {
                        text: format!(
                            "{label} did not answer the adjournment — {}",
                            Self::unreadable_ballot(&resp)
                        ),
                        tone: NoticeTone::Warn,
                    });
                }
                None
            }
        }
    }

    // -- the vote on an extension -----------------------------------------

    /// Put the summarizer's request to every seat, and count it.
    ///
    /// The operator can still settle it at any moment — that is checked between
    /// ballots and the result is theirs if they do. But the debate does not sit
    /// waiting for a human any more than it waits for a model: three seats and a
    /// majority is a decision, so the request resolves itself. With an odd
    /// number of voters it cannot even tie.
    async fn run_extension_vote(
        &self,
        round: u32,
        ask: u32,
        reason: &str,
        opener_turn: TurnId,
    ) -> Option<(bool, &'static str)> {
        let voters = {
            let b = self.board.lock().expect("board");
            b.voting_ids()
        };
        let seats = voters.len();
        if seats == 0 {
            return None;
        }
        let mut yes = 0u32;
        let mut no = 0u32;
        self.activity(
            opener_turn,
            "council",
            format!("asking {seats} seats about more rounds"),
        );

        for voter in voters {
            if self.cancelled() {
                return None;
            }
            // The operator's answer outranks whatever the room is about to say.
            if self.extension_answer.lock().expect("extension").is_some() {
                return None;
            }
            match self
                .cast_extension_ballot(&voter, round, ask, reason, opener_turn)
                .await
            {
                Some((grant, why)) => {
                    if grant {
                        yes += 1;
                    } else {
                        no += 1;
                    }
                    self.sink.emit(EngineEvent::ExtensionBallotCast {
                        round,
                        delegate: voter.clone(),
                        grant,
                        reason: why,
                    });
                    // Stop when the rest cannot change it, rather than making
                    // the last seat wait on a vote that is already decided.
                    if yes as usize > seats / 2 {
                        return Some((true, "the council"));
                    }
                    if no as usize > seats / 2 {
                        return Some((false, "the council"));
                    }
                }
                None => {
                    // A seat that does not answer has not voted against; it is
                    // simply out, and the majority is of those left.
                    self.sink.emit(EngineEvent::ExtensionBallotCast {
                        round,
                        delegate: voter.clone(),
                        grant: false,
                        reason: None,
                    });
                }
            }
        }
        // No early majority, and nothing left to break the tie: whatever the
        // seats that answered actually decided.
        Some((yes > no, "the council"))
    }

    /// One seat's answer to the request for more rounds.
    async fn cast_extension_ballot(
        &self,
        voter: &str,
        round: u32,
        ask: u32,
        reason: &str,
        opener_turn: TurnId,
    ) -> Option<(bool, Option<String>)> {
        let (messages, endpoint, model, temperature, native, label) = {
            let mut b = self.board.lock().expect("board");
            let me = b.delegate(voter)?.clone();
            let ep = b.endpoint_for(voter)?;
            let native = ep.native_tools;
            let mut history = b
                .history
                .remove(voter)
                .unwrap_or_else(|| {
                    vec![
                        ChatMessage::system(prompt::system_prompt(
                            &me,
                            &b.config,
                            native,
                            me.seat_role == SeatRole::Judge,
                        )),
                        ChatMessage::user(prompt::opening_message(&b.config)),
                    ]
                });
            let context = Self::ballot_context(&b);
            history.push(ChatMessage::user(format!(
                "{context}\n\n———\n\n{}",
                prompt::extension_ballot_prompt(ask, reason, round)
            )));
            b.history.insert(voter.to_string(), history.clone());
            (
                history,
                ep,
                me.model.clone(),
                me.temperature.or(Some(0.4)),
                native,
                me.label.clone(),
            )
        };

        let tools = if native {
            tools::extension_ballot_tool()
        } else {
            Vec::new()
        };
        let req = ChatRequest {
            endpoint: self.endpoint_for_request(&endpoint),
            model,
            messages,
            tools: tools.clone(),
            tool_choice: if tools.is_empty() {
                "none".into()
            } else {
                "auto".into()
            },
            temperature,
            // Uncapped, for the same reason every other ballot is: a ballot cut
            // off mid-string stops parsing, and a seat silently out of the vote
            // moves the majority.
            max_tokens: None,
        };
        let chunks = Arc::new(AtomicUsize::new(0));
        let on_chunk = |_: ChatChunk<'_>| {
            chunks.fetch_add(1, Ordering::Relaxed);
        };
        let outcome = self
            .race_request(&req, opener_turn, voter, &chunks, &on_chunk)
            .await;
        let resp = match outcome {
            Outcome::Done(Ok(resp)) => resp,
            Outcome::Done(Err(e)) => {
                self.sink.emit(EngineEvent::Notice {
                    text: format!(
                        "{label} never answered the extension request: {}",
                        describe_llm_error(e)
                    ),
                    tone: NoticeTone::Warn,
                });
                return None;
            }
            Outcome::Skipped => return None,
        };

        // Votes the seat cast alongside its answer are still votes.
        self.record_ballot(voter, &resp, "extension_ballot");
        self.apply_ballot_companions(voter, round, opener_turn, &resp, "extension_ballot")
            .await;
        let action = Self::ballot_action(&resp, "extension_ballot");
        match action {
            Some(Action::ExtensionBallot { grant, reason }) => Some((grant, reason)),
            _ => {
                if !resp.content.trim().is_empty() || !resp.reasoning.trim().is_empty() {
                    self.sink.emit(EngineEvent::Notice {
                        text: format!(
                            "{label} did not answer the extension request — {}",
                            Self::unreadable_ballot(&resp)
                        ),
                        tone: NoticeTone::Warn,
                    });
                }
                None
            }
        }
    }

    // -- reading a ballot --------------------------------------------------

    /// Every tool call in a reply, whether the server sent them natively or
    /// they were scraped out of prose.
    fn reply_calls(resp: &ChatResponse) -> Vec<RawToolCall> {
        if !resp.tool_calls.is_empty() {
            return resp.tool_calls.clone();
        }
        tools::scrape_inline_actions(&format!("{}\n{}", resp.reasoning, resp.content)).calls
    }

    /// The one action of `want` in a reply, wherever it sits.
    ///
    /// A ballot used to read `calls.first()` and give up. A model that opened
    /// with a vote, a cite, or an ironing call had its ballot silently dropped
    /// and was recorded as not having answered — which is not a neutral
    /// outcome: on an adjournment it quietly moves the majority. A seat that
    /// answered is now found wherever it put the answer.
    fn ballot_action(resp: &ChatResponse, want: &str) -> Option<Action> {
        Self::reply_calls(resp)
            .iter()
            .filter_map(|c| tools::parse_action(c).ok())
            .find(|a| a.name() == want)
    }

    /// Board actions a seat bundled with its ballot.
    ///
    /// A ballot is asked with one tool advertised, but a seat that has just
    /// spent a turn weighing four points will sometimes vote in the same
    /// breath — and an arbiter in particular may rule on the deadlock and vote
    /// on the points in one reply. Reading only the ballot left those votes on
    /// the floor, which is the worst possible place to lose them: the seat's
    /// stated reason talked about the votes, and the record showed none.
    async fn apply_ballot_companions(
        &self,
        id: &str,
        round: u32,
        turn: TurnId,
        resp: &ChatResponse,
        want: &str,
    ) -> Vec<String> {
        let others: Vec<RawToolCall> = Self::reply_calls(resp)
            .into_iter()
            // A call that will not parse is *not* dropped here: `apply_actions`
            // refuses it by name, and an invented tool that vanishes without a
            // word is the one thing the model cannot learn from.
            .filter(|c| tools::parse_action(c).map(|a| a.name() != want).unwrap_or(true))
            .collect();
        if others.is_empty() {
            return Vec::new();
        }
        // Boxed because this is mutually recursive with `apply_actions`: a poll
        // runs ballots, and a ballot can now carry a board action.
        Box::pin(self.apply_actions(id, round, turn, &others)).await
    }

    /// Put a seat's own ballot answer back into its history.
    ///
    /// A ballot is a model call whose reply was thrown away: the prompt went in
    /// and came out, and only the parsed action survived. That left the seat's
    /// history ending on a user message it had never answered, so the next
    /// speaking turn appended a second one — two user turns in a row, which
    /// strict proxies reject outright — and the seat could not see its own
    /// reasoning when asked to build on it.
    ///
    /// Recorded with the inline blocks stripped, so a server with no tool
    /// calling does not leave a wall of JSON sitting in the seat's own voice.
    fn record_ballot(&self, voter: &str, resp: &ChatResponse, want: &str) {
        let said = tools::scrape_inline_actions(&resp.content).text();
        // A model with native tools answers with a call and no prose at all, so
        // an empty reply here is the common case, not an absent one. Recording
        // the decision in the seat's own words is what keeps the conversation
        // alternating — and what lets it see what it decided last time.
        let text = if said.trim().is_empty() {
            let inline = tools::parse_inline_actions(&resp.content);
            let Some(action) = resp
                .tool_calls
                .iter()
                .chain(inline.iter())
                .filter_map(|c| tools::parse_action(c).ok())
                .find(|a| a.name() == want)
            else {
                return;
            };
            let why = action
                .reason_of()
                .map(|r| format!(" — {}", r.trim()))
                .unwrap_or_default();
            format!("{}{why}", action.describe())
        } else {
            said
        };
        if text.trim().is_empty() {
            return;
        }
        let mut b = self.board.lock().expect("board");
        if let Some(h) = b.history.get_mut(voter) {
            h.push(ChatMessage::assistant(text));
        }
    }

    /// The option a seat named in plain prose, when it named exactly one.
    ///
    /// A server with no tool support still has to be able to answer, and plenty
    /// of them answer in a sentence. This used to take the *first* option that
    /// appeared anywhere in the reply, as a bare substring, which meant a reply
    /// cut off by the token limit was quietly turned into a vote — the very
    /// outcome `unreadable_ballot` exists to distinguish from a real abstention.
    /// It also meant "Not yet" was read as "No".
    ///
    /// So: never on a truncated reply, only on a whole-word match, only when
    /// exactly one option matches, and always on the record as an inference.
    fn option_named_in_prose(
        &self,
        resp: &ChatResponse,
        options: &[String],
    ) -> Option<(String, Option<String>)> {
        if resp.finish_reason.as_deref() == Some("length") {
            return None;
        }
        let hits: Vec<&String> = options
            .iter()
            .filter(|o| names_a_whole_phrase(&resp.content, o))
            .collect();
        // Two options named in one breath is not an answer to either.
        let [only] = hits.as_slice() else {
            if hits.len() > 1 {
                self.sink.emit(EngineEvent::Notice {
                    text: format!(
                        "a seat named {} of the poll's options in one reply; counted as no answer",
                        hits.len()
                    ),
                    tone: NoticeTone::Warn,
                });
            }
            return None;
        };
        self.sink.emit(EngineEvent::Notice {
            text: format!("the seat's answer was read out of its prose: \"{only}\""),
            tone: NoticeTone::Info,
        });
        Some((only.to_string(), None))
    }

    /// Why a ballot would not parse, in terms the operator can act on.
    ///
    /// A seat whose ballot the server cut off is not the same fact as a seat
    /// that declined to answer: the first is a lost vote that moves the
    /// majority, the second is an abstention. They used to look identical.
    fn unreadable_ballot(resp: &ChatResponse) -> &'static str {
        match resp.finish_reason.as_deref() {
            Some("length") => "the server cut the reply short before the answer was complete",
            Some("content_filter") => "the server's content filter stopped the reply",
            _ => "no readable answer in the reply",
        }
    }

    // -- closing and verdict ---------------------------------------------

    async fn closing_address(&self, id: &str) {
        let (messages, endpoint, model, temperature, label, color, round) = {
            let mut b = self.board.lock().expect("board");
            let (Some(me), Some(ep)) = (b.delegate(id).cloned(), b.endpoint_for(id)) else {
                return;
            };
            let mut history = b.history.remove(id).unwrap_or_default();
            history.push(ChatMessage::user(prompt::closing_prompt(&b.config)));
            b.history.insert(id.to_string(), history.clone());
            let round = b.round;
            (
                history,
                ep,
                me.model.clone(),
                me.temperature.or(Some(0.6)),
                me.label,
                me.color,
                round,
            )
        };
        let turn = {
            let mut b = self.board.lock().expect("board");
            let t = b.next_turn;
            b.next_turn += 1;
            t
        };
        self.sink.emit(EngineEvent::TurnStarted {
            turn,
            delegate: id.to_string(),
            label: label.clone(),
            color,
            model: model.clone(),
            round,
        });
        let req = ChatRequest {
            endpoint: self.endpoint_for_request(&endpoint),
            model,
            messages,
            tools: Vec::new(),
            tool_choice: "none".into(),
            temperature,
            // Uncapped for the same reason as ballots: a closing address cut
            // mid-sentence reads as the seat's considered conclusion.
            max_tokens: None,
        };
        let content_pump = Pump::content(self.sink.as_ref(), turn);
        let thought_pump = Pump::thought(self.sink.as_ref(), turn);
        let chunks = Arc::new(AtomicUsize::new(0));
        let on_chunk = |chunk: ChatChunk<'_>| {
            chunks.fetch_add(1, Ordering::Relaxed);
            match chunk {
                ChatChunk::Content(c) => content_pump.push(c),
                ChatChunk::Reasoning(r) => thought_pump.push(r),
                ChatChunk::Tool(_) => {}
                ChatChunk::Reset => {
                    content_pump.reset();
                    thought_pump.reset();
                }
            }
        };
        let outcome = self.race_request(&req, turn, id, &chunks, &on_chunk).await;
        content_pump.flush(true);
        thought_pump.flush(true);
        let resp = match outcome {
            Outcome::Done(r) => match r {
                Ok(resp) => resp,
                Err(e) => {
                    self.sink.emit(EngineEvent::Notice {
                        text: format!("{label}'s closing address failed: {}", describe_llm_error(e)),
                        tone: NoticeTone::Warn,
                    });
                    return;
                }
            },
            Outcome::Skipped => {
                self.sink.emit(EngineEvent::TurnSkipped {
                    turn,
                    delegate: id.to_string(),
                    label: label.clone(),
                });
                return;
            }
        };
        let thinking = resp.reasoning.chars().take(4_000).collect::<String>();
        // A closing address is prose: the prompt says so and no tool is
        // advertised. A model that answers with a tool call anyway had its JSON
        // rendered verbatim as the speech, which is how a closing address came
        // to be a line of code with a scrollbar. Take the call out of the words
        // and say on the record that it was not acted on, rather than letting it
        // pass for the seat having spoken.
        let (text, stripped) = {
            let thought_lines = resp.reasoning.lines().count();
            let combined = format!("{}\n{}", resp.reasoning, resp.content);
            let scraped = tools::scrape_inline_actions(&combined);
            (scraped.text_from(thought_lines), scraped.calls)
        };
        if !stripped.is_empty() {
            let names: Vec<&str> = stripped.iter().map(|c| c.name.as_str()).collect();
            self.sink.emit(EngineEvent::Notice {
                text: format!(
                    "{label} called {} instead of speaking — a closing address is prose, so it was not acted on",
                    names.join(", ")
                ),
                tone: NoticeTone::Info,
            });
        }
        // A reply that was nothing but a tool call leaves no prose behind, and
        // an empty speech is a hole in the record. Name the absence instead.
        let text = if text.trim().is_empty() && !stripped.is_empty() {
            format!(
                "(no closing address — the reply was a {} call, which this stage does not act on)",
                stripped[0].name
            )
        } else {
            text
        };
        let speech = Speech {
            id: turn,
            delegate: id.to_string(),
            round,
            text,
            reasoning: thinking,
            actions: vec!["closing address".into()],
            phase: TurnPhase::Done,
        };
        let mut b = self.board.lock().expect("board");
        b.speeches.push(speech.clone());
        drop(b);
        self.sink.emit(EngineEvent::SpeechFinished { turn, speech });
    }

    fn render_verdict(
        &self,
        rounds: u32,
        ended_how: EndedHow,
        final_summary: Option<String>,
        conclusion: Option<Conclusion>,
    ) -> DebateVerdict {
        let b = self.board.lock().expect("board");
        let mut carried = vec![];
        let mut fell = vec![];
        let mut stalemate = vec![];
        let mut withdrawn = vec![];
        for p in &b.points {
            let t = p.tally(&b.ids);
            let entry = CarriedPoint {
                id: p.id,
                text: p.text.clone(),
                raised_by: p.raised_by.clone(),
                score: t.score,
                wins: t.wins,
                losses: t.losses,
                stalemates: t.stalemates,
                ruling: p.review.as_ref().and_then(|r| r.outcome),
            };
            match t.status {
                PointStatus::Carried => carried.push(entry),
                PointStatus::Fell => fell.push(entry),
                PointStatus::Stalemate => stalemate.push(entry),
                PointStatus::Withdrawn => withdrawn.push(entry),
                PointStatus::Open => {}
            }
        }
        carried.sort_by(|a, c| c.score.cmp(&a.score).then(a.id.cmp(&c.id)));
        let crux = carried.first().cloned();
        let summary = if b.points.is_empty() {
            "The council produced no points; there is nothing on the record.".to_string()
        } else {
            format!(
                "{} of {} points carried, {} fell, {} stalemate, {} withdrawn. {}",
                carried.len(),
                b.points.len(),
                fell.len(),
                stalemate.len(),
                withdrawn.len(),
                match &crux {
                    Some(c) => format!("The strongest ground is P{}: {}", c.id, c.text),
                    None => "No point carried.".to_string(),
                }
            )
        };
        DebateVerdict {
            motion: b.config.motion.clone(),
            rounds,
            ended_how,
            settled_reason: b.settled_by.as_ref().map(|(_, why)| why.clone()),
            carried,
            fell,
            stalemate,
            withdrawn,
            crux,
            summary,
            adjourned_by: b.adjourn.clone(),
            final_summary,
            conclusion,
        }
    }
}

impl Board {
    fn point_mut(&mut self, id: PointId) -> Option<&mut Point> {
        self.points.iter_mut().find(|p| p.id == id)
    }
}

/// Whether `hay` contains `needle` as a whole word or phrase.
///
/// Substring matching is what made "Not yet" register as a vote for "No". This
/// requires the characters on both sides of the match to be non-alphanumeric,
/// so an option is only found when the model wrote it as a word — punctuation,
/// case and spacing around it are the model's to choose.
fn names_a_whole_phrase(hay: &str, needle: &str) -> bool {
    let (h, n) = (hay.to_lowercase(), needle.trim().to_lowercase());
    if n.is_empty() {
        return false;
    }
    let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    let mut from = 0usize;
    while let Some(i) = h[from..].find(&n) {
        let at = from + i;
        if edge(h[..at].chars().next_back()) && edge(h[at + n.len()..].chars().next()) {
            return true;
        }
        // Overlap, not restart: "no" has to be looked for again one character
        // along, or "nono" would find it at 0, fail, and stop.
        from = at + n.len().min(1);
        if from >= h.len() {
            break;
        }
    }
    false
}

fn briefs(b: &Board) -> Vec<DelegateBrief> {
    b.config
        .delegates
        .iter()
        .map(|d| DelegateBrief {
            id: d.id.clone(),
            label: d.label.clone(),
            model: d.model.clone(),
            color: d.color.clone(),
            endpoint: d.endpoint_id.clone(),
            role: d.seat_role,
        })
        .collect()
}

/// Reduce a claim to a comparable shape: case, spacing and terminal
/// punctuation carry no meaning on the board, so two phrasings of one claim
/// should not both be admitted.
fn normalize_claim(raw: &str) -> String {
    let lowered = raw.trim().to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut space = false;
    for ch in lowered.chars() {
        if ch.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(ch);
    }
    out.trim_end_matches(['.', '!', '?', ';', ':', ',']).to_string()
}

/// How much of the rolling record a delegate is shown: enough to cover a busy
/// board, bounded so a long debate cannot blow the context window. Nobody can
/// know the right number in advance, so it scales with the board instead of
/// asking through a slider.
fn auto_record_budget(points: usize) -> usize {
    (1_500 + 400 * points).clamp(1_500, 12_000)
}

/// The engine's errors go straight to a human in the UI, so the underlying
/// cause has to survive. "could not reach X" on its own is useless.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blip_is_retried_but_a_stall_is_not() {
        // The distinction is how the attempt ended. A connection that died in a
        // second is worth another go; a connection that spent the endpoint's
        // whole budget and then gave up is not, and retrying it turns one lost
        // turn into three.
        assert!(worth_retrying(true, false, 0, 2), "an early failure is a blip");
        assert!(worth_retrying(true, false, 1, 2), "the second try is allowed");
        assert!(!worth_retrying(true, false, 2, 2), "but not a third");

        assert!(
            !worth_retrying(true, true, 0, 2),
            "a request that spent the budget is this endpoint being slow, and \
             another attempt would take exactly as long"
        );
        assert!(
            !worth_retrying(false, false, 0, 2),
            "a request the server will refuse again is not a blip"
        );
    }

    #[test]
    fn a_prose_answer_is_only_read_when_the_model_wrote_the_option_as_a_word() {
        // Substring matching turned "Not yet" into a vote for "No", and turned a
        // reply the server cut short into a vote at all.
        assert!(names_a_whole_phrase("I say No, plainly.", "No"));
        assert!(names_a_whole_phrase("yes — the narrow one", "the narrow one"));
        assert!(names_a_whole_phrase("My answer is: no.", "No"), "case-insensitive");

        assert!(!names_a_whole_phrase("Not yet.", "No"));
        assert!(!names_a_whole_phrase("Nobody has shown it.", "No"));
        assert!(!names_a_whole_phrase("I cannot choose between them", "the narrow one"));
        assert!(!names_a_whole_phrase("anything", ""));
        assert!(!names_a_whole_phrase("", "anything"));
    }

    #[test]
    fn a_whole_word_inside_a_longer_word_does_not_count() {
        assert!(!names_a_whole_phrase("nonsense", "no"));
        assert!(!names_a_whole_phrase("planned", "and"));
        // Overlapping search, so a match at index 0 that fails the edge test does
        // not stop the scan.
        assert!(names_a_whole_phrase("xno no", "no"));
    }

    #[test]
    fn claims_are_compared_on_their_meaning_not_their_punctuation() {
        assert_eq!(normalize_claim("  Roses   outrank lilies. "), "roses outrank lilies");
        assert_eq!(normalize_claim("ROSES OUTRANK LILIES"), "roses outrank lilies");
        assert_eq!(normalize_claim("Roses outrank lilies?"), "roses outrank lilies");
        // Genuinely different claims must not collide.
        assert_ne!(
            normalize_claim("Roses outrank lilies"),
            normalize_claim("Lilies outrank roses")
        );
    }

    #[test]
    fn record_budget_scales_with_the_board() {
        assert_eq!(auto_record_budget(0), 1_500);
        assert_eq!(auto_record_budget(5), 3_500);
        assert_eq!(auto_record_budget(100_000), 12_000);
    }
}

fn describe_llm_error(e: LlmError) -> String {
    e.to_string()
}

fn validate(cfg: &DebateConfig) -> Result<(), String> {
    if cfg.motion.trim().is_empty() {
        return Err("there is no motion to debate".into());
    }
    if matches!(
        cfg.decision.provider,
        crate::types::DecisionProvider::Jev
    ) {
        if cfg.decision.base_url.trim().is_empty() {
            return Err("a decision model is selected but it has no URL".into());
        }
        if cfg.decision.model.trim().is_empty() {
            return Err("a decision model is selected but no model is named".into());
        }
    }
    // Bounded here because these arrive from the webview and from localStorage,
    // and an extension adds to them: an unclamped value would overflow the
    // grant arithmetic and turn a long debate into a one-round one.
    const MAX_ROUNDS: u32 = 100;
    if cfg.max_rounds > MAX_ROUNDS {
        return Err(format!("rounds must be at most {MAX_ROUNDS}"));
    }
    if cfg.max_extra_rounds > MAX_ROUNDS {
        return Err(format!("extra rounds must be at most {MAX_ROUNDS}"));
    }
    if cfg.delegates.len() < 2 {
        return Err("a council needs at least two delegates".into());
    }
    // Custom seats are unlimited as written, and every one of them is a full turn
    // a round with its own history and its own model calls. Bounded here because
    // this arrives from the webview, and refused up front beats discovering it
    // through the bill.
    const MAX_CUSTOM: usize = 6;
    let custom = cfg
        .delegates
        .iter()
        .filter(|d| d.seat_role == SeatRole::Custom)
        .count();
    if custom > MAX_CUSTOM {
        return Err(format!(
            "a council can have at most {MAX_CUSTOM} extra seats; this one has {custom}"
        ));
    }
    // The four convening essentials each sit exactly once: three to argue,
    // one to record. The Rationale is permanent furniture but optional: a
    // council convenes without one, yet at most one may sit. Custom seats
    // may join them without limit.
    for role in [
        SeatRole::Proponent,
        SeatRole::Opponent,
        SeatRole::Judge,
        SeatRole::Summarizer,
    ] {
        let n = cfg
            .delegates
            .iter()
            .filter(|d| d.seat_role == role)
            .count();
        let name = match role {
            SeatRole::Proponent => "proponent (Advocate)",
            SeatRole::Opponent => "opponent (Critic)",
            SeatRole::Judge => "judge (Arbiter)",
            SeatRole::Summarizer => "summarizer",
            SeatRole::Rationale => "rationale",
            SeatRole::Custom => "custom",
        };
        if n == 0 {
            return Err(format!("a council needs exactly one {name} seat"));
        }
        if n > 1 {
            return Err(format!("a council seats only one {name}"));
        }
    }
    if cfg
        .delegates
        .iter()
        .filter(|d| d.seat_role == SeatRole::Rationale)
        .count()
        > 1
    {
        return Err("a council seats only one rationale".into());
    }
    // Seats are addressed by name — `press` takes a label — so two seats sharing
    // one makes the first match silently win, which means a press aimed at the
    // second lands on the first, or reports that you cannot press yourself when
    // you meant somebody else. Cheap to refuse here, impossible to notice later.
    let mut seen_labels: BTreeMap<String, ()> = BTreeMap::new();
    for d in &cfg.delegates {
        let key = d.label.trim().to_lowercase();
        if key.is_empty() {
            return Err("every seat needs a name".into());
        }
        if seen_labels.insert(key, ()).is_some() {
            return Err(format!(
                "two seats are called {} — rename one so they can be told apart",
                d.label
            ));
        }
    }
    for d in &cfg.delegates {
        if d.model.trim().is_empty() {
            return Err(format!("{} has no model selected", d.label));
        }
        match cfg.endpoints.iter().find(|e| e.id == d.endpoint_id) {
            Some(e) if !e.base_url.trim().is_empty() => {}
            _ => return Err(format!("{} has no endpoint configured", d.label)),
        }
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub motion: String,
    pub round: u32,
    pub max_rounds: u32,
    pub delegates: Vec<crate::events::DelegateBrief>,
    pub points: Vec<Point>,
    pub tallies: std::collections::BTreeMap<u32, Tally>,
    pub polls: Vec<Poll>,
    pub speeches: Vec<Speech>,
    pub finished: bool,
}
