//! The event stream the engine emits. The UI renders from these and nothing else,
//! so the Tauri layer is a straight pass-through.

use crate::types::{
    CarriedPoint, DebateVerdict, Point, Poll, SeatRole, Speech, StalemateReading, StalemateReview,
    Tally, Verdict,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EngineEvent {
    DebateStarted {
        motion: String,
        rounds: u32,
        /// Total extra rounds the summarizer may ask for, for the budget line.
        max_extra_rounds: u32,
        delegates: Vec<DelegateBrief>,
        /// What images the motion carries, without the bytes. The UI keeps
        /// these so the record says what was argued over.
        images: Vec<crate::types::ImageInfo>,
    },
    RoundStarted {
        round: u32,
    },
    /// The round is over and the floor belongs to the summarizer: it lays the
    /// round out on the record before anyone argues again.
    IntermissionStarted {
        round: u32,
    },
    TurnStarted {
        turn: u64,
        delegate: String,
        label: String,
        color: String,
        model: String,
        round: u32,
    },
    /// The floor belongs to the operator: this seat is taken by a human, so
    /// instead of a model reply the turn waits for a submit. `brief` is the
    /// same board-plus-floor text a model would be shown, which is what keeps
    /// the human honest — they argue from the record, not from memory.
    YourTurn {
        turn: u64,
        delegate: String,
        label: String,
        round: u32,
        brief: String,
    },
    /// A ballot the operator must answer for their seat: a poll, a deadlock
    /// review, an adjournment confirmation, or an extension ballot. `options`
    /// are the legal answers; anything else is refused at submit time.
    BallotAsked {
        delegate: String,
        kind: BallotKind,
        question: String,
        options: Vec<String>,
        point_id: Option<u32>,
    },
    /// A request is in flight and no tokens have arrived yet, or tokens are
    /// arriving slowly. The UI uses this to show that a slow model is still
    /// alive, rather than a cursor staring back at nothing.
    TurnProgress {
        turn: u64,
        delegate: String,
        elapsed_secs: u64,
        /// Streamed pieces so far, which is what "still alive" means. Not a
        /// token count: the server's own numbers arrive in
        /// [`EngineEvent::TurnUsage`] when the turn ends, and a server that was
        /// never asked for them never sends any.
        tokens: u64,
    },
    /// What this turn is doing right now, in words meant for the operator.
    ///
    /// A seat using a tool is indistinguishable from a hung one: the words stop,
    /// nothing resolves, and the turn simply sits there. A poll asks every other
    /// seat a question, so the opener's turn can be open for a minute with
    /// nothing on the transcript. This is how the room says which it is.
    TurnActivity {
        turn: u64,
        delegate: String,
        /// Plain language: "reading the web", "asking 3 seats", "voting".
        what: String,
    },
    TurnSkipped {
        turn: u64,
        delegate: String,
        label: String,
    },
    Token {
        turn: u64,
        text: String,
    },
    /// What one turn actually cost, when the server counted it.
    ///
    /// Absent unless the endpoint opted in, and never a zero: a server that
    /// does not report usage produces no event at all, so the UI can tell "free"
    /// apart from "not measured".
    TurnUsage {
        turn: u64,
        delegate: String,
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    /// A seat read a page from the reference shelf.
    ///
    /// The text rides along so the operator can open the same page the model
    /// did. A seat citing a source nobody else can see is no more trustworthy
    /// than a seat inventing one, and the second is at least consistent.
    WebFetched {
        turn: u64,
        delegate: String,
        /// `wikipedia`, `wiktionary`, or `web_search`.
        source: String,
        url: String,
        title: String,
        text: String,
        truncated: bool,
    },
    /// A piece of the model's private thinking, streamed exactly like a token.
    /// The UI renders it as a collapsible aside, never as speech.
    Reasoning {
        turn: u64,
        text: String,
    },
    SpeechFinished {
        turn: u64,
        speech: Speech,
    },
    ActionTaken {
        turn: u64,
        delegate: String,
        action: String,
        detail: String,
    },
    ActionRejected {
        turn: u64,
        delegate: String,
        tool: String,
        reason: String,
    },
    PointUpdated {
        point: Point,
        tally: Tally,
        /// Set when this update decided the point.
        resolved: bool,
    },
    PollOpened {
        poll: Poll,
    },
    PollBallotCast {
        poll_id: u32,
        delegate: String,
        choice: String,
        result: Vec<(String, u32)>,
    },
    PollClosed {
        poll: Poll,
        outcome: Option<String>,
    },
    ReviewOpened {
        point_id: u32,
        review: StalemateReview,
    },
    ReviewBallotCast {
        point_id: u32,
        delegate: String,
        reading: StalemateReading,
        result: Vec<(String, u32)>,
    },
    ReviewClosed {
        point_id: u32,
        review: StalemateReview,
        outcome: Option<StalemateReading>,
    },
    /// A seat moved to adjourn. Closing takes a majority of seats, so the
    /// motion goes to every other seat for confirmation.
    AdjournMoved {
        by: String,
        reason: String,
        round: u32,
    },
    AdjournBallotCast {
        delegate: String,
        confirm: bool,
        result: Vec<(String, u32)>,
    },
    AdjournDecided {
        passed: bool,
        yes: u32,
        no: u32,
        reason: String,
    },
    /// The summarizer asked for more rounds. The loop is suspended until the
    /// operator answers through `resolve_extension`.
    ExtensionProposed {
        round: u32,
        rounds: u32,
        reason: String,
    },
    /// One seat's answer to the summarizer's request for more rounds.
    ///
    /// Emitted for every seat asked, including the ones that failed or said
    /// nothing, so the operator can see the count build rather than watching a
    /// number jump at the end. `reason: None` means the seat did not answer.
    ExtensionBallotCast {
        round: u32,
        delegate: String,
        grant: bool,
        reason: Option<String>,
    },
    ExtensionGranted {
        added: u32,
        total: u32,
        budget_left: u32,
    },
    ExtensionDeclined {
        round: u32,
    },
    RosterUpdated {
        delegates: Vec<DelegateBrief>,
        speech: Option<Speech>,
    },
    Notice {
        text: String,
        tone: NoticeTone,
    },
    DebateEnded {
        verdict: DebateVerdict,
    },
    /// A seat failed, or the whole run gave up. `fatal` is the difference the
    /// UI acts on: a per-seat failure must not cost the operator the controls
    /// for a debate that is still going.
    Failed {
        message: String,
        fatal: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NoticeTone {
    Info,
    Warn,
    Error,
}

/// Which ballot is waiting on the operator. Serialized so the UI can route
/// each kind to its own prompt without string-matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BallotKind {
    Poll,
    Review,
    Adjourn,
    Extension,
}

impl BallotKind {
    /// In words, for the refusals that name the mismatch.
    pub fn as_words(self) -> &'static str {
        match self {
            Self::Poll => "poll",
            Self::Review => "deadlock review",
            Self::Adjourn => "adjournment",
            Self::Extension => "extension",
        }
    }

    /// Every kind here is a question the council asks a seat, so all of them
    /// can be put to the operator.
    pub fn can_ask(self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegateBrief {
    pub id: String,
    pub label: String,
    pub model: String,
    pub color: String,
    pub endpoint: String,
    pub role: SeatRole,
    /// Taken by the operator. The rail marks the seat, and a reload still knows
    /// it does not belong to a model.
    #[serde(default)]
    pub human: bool,
}

/// A point flattened for the transcript, so the UI need not walk the board.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PointSummary {
    pub id: u32,
    pub text: String,
    pub raised_by: String,
    pub status: String,
    pub score: i32,
    pub votes: Vec<(String, Verdict)>,
    pub carried: CarriedPoint,
}
