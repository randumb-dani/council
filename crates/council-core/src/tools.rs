//! The council's toolbelt.
//!
//! Models are not asked to *describe* what they did — every consequential act
//! goes through a tool call, so the engine can adjudicate deterministically
//! rather than trusting prose. The three the app is built around are `vote`
//! (win / lose / stalemate on a point), `concede`, and `poll`; the rest exist to
//! give a debate some texture.

use crate::types::{PointId, StalemateReading, Verdict};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// A tool call as it came off the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// A tool call after validation: the arguments now fit the schema.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    MakePoint {
        text: String,
        beats: Vec<PointId>,
    },
    Vote {
        point: PointId,
        verdict: Verdict,
        reason: Option<String>,
    },
    Concede {
        point: PointId,
        reason: Option<String>,
    },
    Withdraw {
        point: PointId,
        reason: Option<String>,
    },
    Poll {
        question: String,
        options: Vec<String>,
        reason: Option<String>,
    },
    Review {
        point: PointId,
        reason: Option<String>,
    },
    StalemateBallot {
        reading: StalemateReading,
        reason: Option<String>,
        /// A changed vote on the point under review, cast in the same breath.
        vote: Option<Verdict>,
    },
    AdjournBallot {
        confirm: bool,
        reason: Option<String>,
    },
    PollBallot {
        choice: String,
        reason: Option<String>,
    },
    Steelman {
        point: PointId,
        restatement: String,
    },
    Rebut {
        point: PointId,
        argument: String,
    },
    Cite {
        source: String,
        quote: Option<String>,
        point: Option<PointId>,
        stance: String,
    },
    Press {
        to: Option<String>,
        point: Option<PointId>,
        question: String,
    },
    Adjourn {
        reason: String,
    },
    /// The summarizer's one instrument: ask the operator for more rounds.
    ProposeExtension {
        rounds: u32,
        reason: String,
    },
    /// The summarizer's answer to the motion, once everything has been said.
    Conclude {
        agreed: bool,
        answer: String,
        lean: Option<String>,
    },
    /// The Arbiter settling a point outright, whatever the tally says.
    Rule {
        point: PointId,
        ruling: Ruling,
        reason: String,
    },
    /// A seat's answer to the summarizer's request for more rounds.
    ExtensionBallot {
        grant: bool,
        reason: Option<String>,
    },
    /// The summarizer's judgement that the board has done its work.
    ///
    /// The engine can stop a debate on its own — every point decided and
    /// contested — but that is a count, not a reading. It cannot tell five points
    /// carried decisively from one point still deadlocked, so it keeps going or
    /// stops on arithmetic. The seat that has read every round can tell.
    DeclareSettled {
        reason: String,
    },
}

/// Which way the Arbiter settles a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ruling {
    Carries,
    Falls,
}

impl Ruling {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "carries" | "carry" | "win" | "wins" | "upheld" | "uphold" | "stands" | "true" => {
                Some(Self::Carries)
            }
            "falls" | "fall" | "lose" | "loses" | "struck" | "strike" | "down" | "false" => {
                Some(Self::Falls)
            }
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Carries => "carries",
            Self::Falls => "falls",
        }
    }
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Self::MakePoint { .. } => "make_point",
            Self::Vote { .. } => "vote",
            Self::Concede { .. } => "concede",
            Self::Withdraw { .. } => "withdraw",
            Self::Poll { .. } => "poll",
            Self::Review { .. } => "review",
            Self::StalemateBallot { .. } => "stalemate_ballot",
            Self::AdjournBallot { .. } => "adjourn_ballot",
            Self::PollBallot { .. } => "poll_ballot",
            Self::Steelman { .. } => "steelman",
            Self::Rebut { .. } => "rebut",
            Self::Cite { .. } => "cite",
            Self::Press { .. } => "press",
            Self::Adjourn { .. } => "adjourn",
            Self::ProposeExtension { .. } => "propose_extension",
            Self::Conclude { .. } => "conclude",
            Self::Rule { .. } => "rule",
            Self::ExtensionBallot { .. } => "extension_ballot",
            Self::DeclareSettled { .. } => "settle",
        }
    }

    /// The stated reasoning, for the actions that carry any.
    ///
    /// Separate from [`Self::describe`], which is a label for the transcript and
    /// deliberately short. A seat reading its own history back needs the
    /// *reason* — that is the whole of what it thought — and a bare "ballot: the
    /// narrow one" leaves it with the conclusion and nothing that produced it.
    pub fn reason_of(&self) -> Option<&str> {
        match self {
            Self::Vote { reason, .. }
            | Self::Concede { reason, .. }
            | Self::Withdraw { reason, .. }
            | Self::Poll { reason, .. }
            | Self::Review { reason, .. }
            | Self::StalemateBallot { reason, .. }
            | Self::AdjournBallot { reason, .. }
            | Self::PollBallot { reason, .. }
            | Self::ExtensionBallot { reason, .. } => reason.as_deref(),
            // These three state a reason outright, rather than optionally.
            Self::Rule { reason, .. } | Self::ProposeExtension { reason, .. } => Some(&**reason),
            Self::Adjourn { reason } => Some(&**reason),
            _ => None,
        }
        .filter(|r| !r.trim().is_empty())
        .map(|r| r.trim())
    }

    /// One-line summary shown in the transcript's action log.
    pub fn describe(&self) -> String {
        match self {
            Self::MakePoint { text, beats } => {
                let mut s = format!("raised a point: {text}");
                if !beats.is_empty() {
                    s.push_str(&format!(" (defeats {})", join_ids(beats)));
                }
                s
            }
            Self::Vote {
                point,
                verdict,
                reason,
            } => format!(
                "voted {} on P{}{}",
                verdict.as_str(),
                point,
                suffix(reason)
            ),
            Self::Concede { point, reason } => format!("conceded P{}{}", point, suffix(reason)),
            Self::Withdraw { point, reason } => {
                format!("withdrew P{}{}", point, suffix(reason))
            }
            Self::Poll { question, .. } => format!("opened a poll: {}", question),
            Self::Review { point, reason } => format!(
                "called a deadlock review on P{}{}",
                point,
                suffix(reason)
            ),
            Self::StalemateBallot {
                reading,
                reason,
                vote,
            } => {
                let mut s = format!("ruled {}", reading.as_str());
                if let Some(v) = vote {
                    s.push_str(&format!(" and re-voted {}", v.as_str()));
                }
                s.push_str(&suffix(reason));
                s
            }
            Self::PollBallot { choice, .. } => format!("ballot: {}", choice),
            Self::Steelman {
                point,
                restatement,
            } => format!("steelmanned P{}: {}", point, restatement),
            Self::Rebut { point, argument } => format!("rebutted P{}: {}", point, argument),
            Self::Cite {
                source,
                quote,
                point,
                stance,
            } => {
                let mut s = format!("cited {} ({})", source, stance);
                if let Some(p) = point {
                    s.push_str(&format!(" re P{}", p));
                }
                if let Some(q) = quote {
                    s.push_str(&format!(" — \"{}\"", q));
                }
                s
            }
            Self::Press { to, point, question } => {
                let mut s = String::from("pressed");
                if let Some(t) = to {
                    s.push_str(&format!(" {}", t));
                }
                if let Some(p) = point {
                    s.push_str(&format!(" re P{}", p));
                }
                s.push_str(&format!(" — {}", question));
                s
            }
            Self::Adjourn { reason } => format!("moved to adjourn: {}", reason),
            Self::ProposeExtension { rounds, reason } => format!(
                "proposed {rounds} extra round{}: {reason}",
                if *rounds == 1 { "" } else { "s" }
            ),
            Self::Conclude {
                agreed,
                answer,
                lean,
            } => {
                let mut s = if *agreed {
                    format!("answered the motion: {answer}")
                } else {
                    "could not agree on an answer".to_string()
                };
                if let Some(l) = lean {
                    s.push_str(&format!(" (leaning {l})"));
                }
                s
            }
            Self::Rule {
                point,
                ruling,
                reason,
            } => {
                let word = if *ruling == Ruling::Carries { "carries" } else { "falls" };
                format!("ruled P{point} {word}: {reason}")
            }
            Self::ExtensionBallot { grant, reason } => {
                let mut s = if *grant {
                    "would grant more rounds".to_string()
                } else {
                    "would not grant more rounds".to_string()
                };
                if let Some(r) = reason {
                    s.push_str(&format!(" — {r}"));
                }
                s
            }
            Self::AdjournBallot { confirm, reason } => {
                let mut s = format!(
                    "answered the adjournment: {}",
                    if *confirm { "yes" } else { "no" }
                );
                s.push_str(&suffix(reason));
                s
            }
            Self::DeclareSettled { reason } => {
                format!("declared the board settled{}", suffix(&Some(reason.clone())))
            }
        }
    }
}

fn suffix(reason: &Option<String>) -> String {
    match reason {
        Some(r) if !r.trim().is_empty() => format!(" — {}", r.trim()),
        _ => String::new(),
    }
}

fn join_ids(ids: &[PointId]) -> String {
    ids.iter()
        .map(|i| format!("P{}", i))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Argument extraction — deliberately lenient, because models are not tidy.
// ---------------------------------------------------------------------------

/// Look up a key, tolerating snake_case / camelCase / spacing differences.
fn field<'a>(args: &'a Value, names: &[&str]) -> Option<&'a Value> {
    let obj = args.as_object()?;
    for name in names {
        if let Some(v) = obj.get(*name) {
            return Some(v);
        }
    }
    // Normalised fallback: compare with separators and case removed, so that
    // `pointId`, `point_id` and `POINT ID` all land on the same key.
    let norm = |s: &str| {
        s.to_ascii_lowercase()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
    };
    for (k, v) in obj {
        if names.iter().any(|n| norm(n) == norm(k)) {
            return Some(v);
        }
    }
    None
}

fn text_field(args: &Value, names: &[&str]) -> Option<String> {
    let v = field(args, names)?;
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn point_field(args: &Value, names: &[&str]) -> Option<PointId> {
    let v = field(args, names)?;
    match v {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        // Models love writing "P3" or "point 3".
        Value::String(s) => point_from_text(s),
        _ => None,
    }
}

/// The single point id in a piece of prose.
///
/// Only the first run of digits counts, and a second run is an outright
/// rejection rather than a guess: joining every digit run turned "P1 beat P2"
/// into point 12, which silently pointed a vote at a claim the model never
/// named. Refusing costs one tool round-trip; guessing costs the record.
fn point_from_text(s: &str) -> Option<PointId> {
    let first = s.chars().skip_while(|c| !c.is_ascii_digit());
    let digits: String = first.take_while(char::is_ascii_digit).collect();
    let id: PointId = digits.parse().ok()?;
    let extra = s
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .skip_while(char::is_ascii_digit)
        .any(|c| c.is_ascii_digit());
    if extra {
        None
    } else {
        Some(id)
    }
}

/// A yes/no tolerant of how models actually answer: booleans, yes/no words,
/// 1/0.
fn bool_field(args: &Value, names: &[&str]) -> Option<bool> {
    let v = field(args, names)?;
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_u64().map(|x| x != 0),
        // "1" and "0" as *strings* are at least as common as the JSON numbers
        // above, and a missed ballot is silently counted as silence — which,
        // on a strict majority, is how an adjournment motion quietly dies.
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "1" | "yes" | "y" | "true" | "confirm" | "agree" | "aye" | "on" | "ok" => Some(true),
            "0" | "no" | "n" | "false" | "deny" | "disagree" | "nay" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn point_list_field(args: &Value, names: &[&str]) -> Vec<PointId> {
    let Some(v) = field(args, names) else {
        return Vec::new();
    };
    let items: Vec<Value> = match v {
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    items
        .iter()
        .filter_map(|item| match item {
            Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
            Value::String(s) => point_from_text(s),
            _ => None,
        })
        .collect()
}

fn string_list_field(args: &Value, names: &[&str]) -> Vec<String> {
    let Some(v) = field(args, names) else {
        return Vec::new();
    };
    let items: Vec<Value> = match v {
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    items
        .iter()
        .filter_map(|i| i.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse a raw tool call into a validated [`Action`].
pub fn parse_action(call: &RawToolCall) -> Result<Action, String> {
    let a = &call.arguments;
    // Some servers hand back the JSON as a string.
    let args: Value = match a {
        Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::Object(Map::new())),
        other => other.clone(),
    };
    let name = call.name.trim().to_ascii_lowercase();

    let action = match name.as_str() {
        "make_point" | "raise_point" | "point" => {
            let text = text_field(&args, &["text", "point", "claim", "statement", "body"])
                .ok_or("make_point needs `text`")?;
            Action::MakePoint {
                beats: point_list_field(&args, &["beats", "defeats", "refutes", "targets"]),
                text,
            }
        }
        "vote" => {
            let point = point_field(&args, &["point", "point_id", "target", "id", "on", "subject"])
                .ok_or("vote needs `point`")?;
            let raw = text_field(
                &args,
                &["verdict", "decision", "call", "vote", "result", "ruling", "position"],
            )
                .ok_or("vote needs `verdict`")?;
            let verdict = Verdict::parse(&raw)
                .ok_or_else(|| format!("unknown verdict {raw:?} (want win/lose/stalemate)"))?;
            Action::Vote {
                point,
                verdict,
                reason: text_field(&args, &["reason", "why", "justification", "rationale"]),
            }
        }
        "concede" => {
            let point = point_field(&args, &["point", "point_id", "target", "id", "on", "subject"])
                .ok_or("concede needs `point`")?;
            Action::Concede {
                point,
                reason: text_field(&args, &["reason", "why", "justification", "rationale"]),
            }
        }
        "withdraw" => {
            let point = point_field(&args, &["point", "point_id", "id"])
                .ok_or("withdraw needs `point`")?;
            Action::Withdraw {
                point,
                reason: text_field(&args, &["reason", "why"]),
            }
        }
        "poll" | "call_poll" | "motion" => {
            let question = text_field(&args, &["question", "text", "prompt", "query"])
                .ok_or("poll needs `question`")?;
            let mut options = string_list_field(&args, &["options", "choices", "candidates"]);
            if options.len() < 2 {
                // `poll` with a single answer is really a vote; treat it as one.
                if let Some(choice) =
                    text_field(&args, &["choice", "answer", "position", "vote", "option"])
                {
                    options = vec![choice];
                }
            }
            if options.is_empty() {
                return Err("poll needs `options`".into());
            }
            Action::Poll {
                question,
                options,
                reason: text_field(&args, &["reason", "why", "rationale"]),
            }
        }
        "review" | "call_review" | "deadlock_review" => {
            let point = point_field(&args, &["point", "point_id", "target", "id", "on", "subject"])
                .ok_or("review needs `point`")?;
            Action::Review {
                point,
                reason: text_field(&args, &["reason", "why", "rationale"]),
            }
        }
        "stalemate_ballot" | "review_ballot" => {
            let raw = text_field(&args, &["reading", "ruling", "call", "decision"])
                .ok_or("stalemate_ballot needs `reading` (unsolvable | too_dense | decidable)")?;
            let reading = StalemateReading::parse(&raw).ok_or_else(|| {
                format!("unknown reading {raw:?} (want unsolvable / too_dense / decidable)")
            })?;
            let vote = match text_field(&args, &["vote", "verdict", "revote"]) {
                Some(v) => Some(
                    Verdict::parse(&v).ok_or_else(|| {
                        format!("unknown vote {v:?} (want win/lose/stalemate)")
                    })?,
                ),
                None => None,
            };
            Action::StalemateBallot {
                reading,
                reason: text_field(&args, &["reason", "why", "rationale"]),
                vote,
            }
        }
        "poll_ballot" | "ballot" | "answer_poll" => {
            let choice = text_field(&args, &["choice", "answer", "option", "vote", "position"])
                .ok_or("poll_ballot needs `choice`")?;
            Action::PollBallot {
                choice,
                reason: text_field(&args, &["reason", "why", "rationale"]),
            }
        }
        "steelman" | "steelman_point" => {
            let point = point_field(&args, &["point", "point_id", "target", "id", "on", "subject"])
                .ok_or("steelman needs `point`")?;
            let restatement = text_field(
                &args,
                &["restatement", "text", "statement", "summary", "strongest"],
            )
            .ok_or("steelman needs `restatement`")?;
            Action::Steelman {
                point,
                restatement,
            }
        }
        "rebut" | "rebuttal" | "attack" | "oppose" => {
            let point = point_field(&args, &["point", "point_id", "target", "id", "on", "subject"])
                .ok_or("rebut needs `point`")?;
            let argument = text_field(&args, &["argument", "text", "rebuttal", "reason", "body"])
                .ok_or("rebut needs `argument`")?;
            Action::Rebut { point, argument }
        }
        "cite" | "evidence" | "source" => {
            let source = text_field(&args, &["source", "citation", "reference", "title"])
                .ok_or("cite needs `source`")?;
            Action::Cite {
                source,
                quote: text_field(&args, &["quote", "excerpt", "passage", "text"]),
                point: point_field(&args, &["point", "point_id", "about_point", "re"]),
                stance: text_field(&args, &["stance", "supports", "relation"])
                    .unwrap_or_else(|| "neutral".into())
                    .to_ascii_lowercase(),
            }
        }
        "press" | "question" | "ask" => {
            Action::Press {
                to: text_field(&args, &["to", "delegate", "target", "who"]),
                point: point_field(&args, &["point", "point_id", "re", "about"]),
                question: text_field(&args, &["question", "text", "query", "prompt"])
                    .ok_or("press needs `question`")?,
            }
        }
        "adjourn_ballot" | "confirm_adjournment" => {
            let confirm = bool_field(&args, &["confirm", "yes", "agree", "answer"])
                .ok_or("adjourn_ballot needs `confirm` (true/false)")?;
            Action::AdjournBallot {
                confirm,
                reason: text_field(&args, &["reason", "why", "rationale"]),
            }
        }
        "adjourn" | "close" | "end_debate" | "verdict" => Action::Adjourn {
            reason: text_field(&args, &["reason", "why", "rationale", "summary"])
                .unwrap_or_else(|| "the record is sufficient".into()),
        },
        "conclude" | "give_answer" | "answer" => {
            let agreed = bool_field(&args, &["agreed", "agree", "consensus", "settled"])
                .unwrap_or(false);
            let answer = text_field(&args, &["answer", "verdict", "conclusion", "summary"]);
            let lean = text_field(&args, &["lean", "leaning", "tilt", "toward"]);
            if !agreed && answer.is_none() && lean.is_none() {
                return Err("conclude needs at least an `answer` or a `lean`".into());
            }
            Action::Conclude {
                agreed,
                answer: answer.unwrap_or_default(),
                lean,
            }
        }
        "propose_extension" | "extend_debate" | "request_rounds" => {
            let rounds = point_field(
                &args,
                &["rounds", "additional_rounds", "extra_rounds", "count", "n"],
            )
            .filter(|n| *n >= 1)
            .ok_or("propose_extension needs `rounds` (at least 1)")?;
            Action::ProposeExtension {
                rounds,
                reason: text_field(&args, &["reason", "why", "rationale"])
                    .unwrap_or_else(|| "the board needs more rounds".into()),
            }
        }
        "rule" | "judge" | "ruling" | "decide" => {
            let point = point_field(&args, &["point", "id", "p"]).ok_or("rule needs a `point`")?;
            let raw = text_field(&args, &["ruling", "verdict", "decision", "outcome", "call"])
                .ok_or("rule needs a `ruling` of \"carries\" or \"falls\"")?;
            let ruling = Ruling::parse(&raw).ok_or_else(|| {
                format!("{raw:?} is not a ruling — use \"carries\" or \"falls\"")
            })?;
            let reason = text_field(&args, &["reason", "why", "because", "rationale"])
                .ok_or("rule needs a `reason` — a ruling the record cannot justify is an opinion")?;
            Action::Rule {
                point,
                ruling,
                reason,
            }
        }
        "settle" | "declare_settled" | "conclude_debate" => Action::DeclareSettled {
            reason: text_field(&args, &["reason", "why", "because"])
                .or_else(|| text_field(&args, &["summary", "text"]))
                .ok_or("settle needs a `reason` — what has the board settled?")?,
        },
        "extension_ballot" | "extend_ballot" | "vote_extension" => {
            let grant = bool_field(
                &args,
                &["grant", "yes", "approve", "extend", "allow", "agreed"],
            )
            .ok_or("extension_ballot needs `grant` (true/false)")?;
            Action::ExtensionBallot {
                grant,
                reason: text_field(&args, &["reason", "why", "because"]),
            }
        }
        other => return Err(format!("unknown tool {other:?}")),
    };
    Ok(action)
}

// ---------------------------------------------------------------------------
// Wire schemas
// ---------------------------------------------------------------------------

fn tool(name: &str, description: &str, parameters: Value) -> Value {
    json!({ "type": "function", "function": { "name": name, "description": description, "parameters": parameters } })
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
}

fn s(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

fn n(desc: &str) -> Value {
    json!({ "type": "integer", "description": desc })
}

/// The Arbiter's instrument, offered only to that seat.
///
/// A separate belt rather than another entry in `speaking_tools`, because the
/// power has to belong to exactly one seat: a ruling tool every model can reach
/// is not a ruling tool, it is a second vote with better manners.
pub fn rule_tool() -> Vec<Value> {
    vec![tool(
        "rule",
        "Settle a point outright: it carries, or it falls, whatever the votes say. Use this at most once per turn, and only when the counting has settled a question the record cannot. Give a reason the record can stand behind — a ruling nobody can justify is an opinion with a badge on it. To register a view without deciding anything, vote as usual.",
        obj(
            json!({
                "point": n("Id of the point you are ruling on."),
                "ruling": {
                    "type": "string",
                    "enum": ["carries", "falls"],
                    "description": "Whether the point stands or is struck."
                },
                "reason": s("Why the record supports this. One or two sentences.")
            }),
            &["point", "ruling", "reason"],
        ),
    )]
}

/// The two reading tools, offered only to the seats that argue and only when
/// the operator has switched web access on.
///
/// Kept out of the speaking belt by default: a seat that can look things up
/// starts looking things up, and a council that researches instead of
/// deliberating produces a very well-sourced way of talking past each other.
pub fn web_tools() -> Vec<Value> {
    vec![
        tool(
            "web_search",
            "Look something up on Wikipedia or Wiktionary and get back matching article titles. Use it when a term in the motion is one you are not certain of, or when you want to find out what a position actually claims before you refute it. Read an article afterwards with web_fetch.",
            obj(
                json!({
                    "query": s("What to look for: a term, a concept, or a claim."),
                }),
                &["query"],
            ),
        ),
        tool(
            "web_fetch",
            "Read one Wikipedia or Wiktionary article and get its text. For a word use a dictionary term and it will read the dictionary; for a subject, position or institution it will read the article. You may pass a URL instead of a title.",
            obj(
                json!({
                    "title": s(
                        "The exact article title from web_search, or a plain word to look up in the \
                         dictionary (Wiktionary), or a URL. An article and a dictionary term are both \
                         accepted here.",
                    ),
                    "source": {
                        "type": "string",
                        "enum": ["auto", "wikipedia", "wiktionary"],
                        "description": "Which book to read. 'auto' tries Wikipedia then the dictionary."
                    }
                }),
                &["title"],
            ),
        ),
    ]
}

/// The full toolbelt offered on a normal speaking turn.
pub fn speaking_tools() -> Vec<Value> {
    vec![
        tool(
            "make_point",
            "Put a new, self-contained claim on the board. Points are what the council actually votes on, so make each one a single, falsifiable assertion.",
            obj(
                json!({
                    "text": s("The claim, stated in one clear falsifiable sentence."),
                    "beats": {
                        "type": "array",
                        "items": n("Id of a point this one defeats."),
                        "description": "Existing point ids this claim refutes."
                    }
                }),
                &["text"],
            ),
        ),
        tool(
            "vote",
            "Rate an existing point, or re-rate one you have already rated. Use `win` if it survives scrutiny, `lose` if it is refuted, `stalemate` if the record genuinely cannot decide it.\n\nCall it once per point: rating three points means three `vote` calls in the same turn, and a turn that engages points should rate all of them rather than one. Revising is expected, not a concession. You may change your vote on any point at any point in the debate — including one of your own earlier votes — as the record answers the objection that moved you. Say in the reason what changed, because a revision with a stated cause is worth more to the council than a repetition of round one.\n\nTwo limits: rate each point at most once per turn, and rating a point is weighing where it stands, not deciding where it lands. Moving a point off the count is `rule`, and only the Arbiter has one.",
            obj(
                json!({
                    "point": n("Id of the point being rated."),
                    "verdict": {
                        "type": "string",
                        "enum": ["win", "lose", "stalemate"],
                        "description": "win = the point stands; lose = it falls; stalemate = undecidable."
                    },
                    "reason": s("One sentence of justification.")
                }),
                &["point", "verdict"],
            ),
        ),
        tool(
            "concede",
            "Concede a point outright. Use this when the other side has genuinely beaten the point — do not use it as a rhetorical flourish.",
            obj(
                json!({
                    "point": n("Id of the point being conceded."),
                    "reason": s("Why you are giving it up.")
                }),
                &["point"],
            ),
        ),
        tool(
            "withdraw",
            "Retract a point you raised yourself because you no longer hold it.",
            obj(
                json!({
                    "point": n("Id of your own point."),
                    "reason": s("Why you are retracting it.")
                }),
                &["point"],
            ),
        ),
        tool(
            "poll",
            "Put a question to the whole council with a fixed set of options. Every other delegate is polled immediately and the result is recorded. Best for factual or binary questions where argument is going nowhere.",
            obj(
                json!({
                    "question": s("The question put to the council."),
                    "options": {
                        "type": "array",
                        "items": s("One candidate answer."),
                        "minItems": 2,
                        "description": "Two to five mutually exclusive answers."
                    },
                    "reason": s("Why a poll is the right instrument here.")
                }),
                &["question", "options"],
            ),
        ),
        tool(
            "steelman",
            "Restate another delegate's point in the strongest, most plausible form it could take. Do this before attacking it.",
            obj(
                json!({
                    "point": n("Id of the point to steelman."),
                    "restatement": s("The point at its strongest, in your own words.")
                }),
                &["point", "restatement"],
            ),
        ),
        tool(
            "rebut",
            "Attack a specific point with a concrete counter-argument.",
            obj(
                json!({
                    "point": n("Id of the point being attacked."),
                    "argument": s("The counter-argument, in one or two sentences.")
                }),
                &["point", "argument"],
            ),
        ),
        tool(
            "cite",
            "Put evidence on the record.",
            obj(
                json!({
                    "source": s("Where it comes from — paper, dataset, documentation, measurement."),
                    "quote": s("The relevant passage or figure, verbatim if you can."),
                    "point": n("Point id this evidence bears on."),
                    "stance": {
                        "type": "string",
                        "enum": ["supports", "undermines", "neutral"],
                        "description": "How it bears on the point."
                    }
                }),
                &["source", "stance"],
            ),
        ),
        tool(
            "press",
            "Put a direct question to another delegate and demand an answer on the record.",
            obj(
                json!({
                    "to": s("Label of the seat being pressed, exactly as the council is listed — or its id."),
                    "point": n("Point id the question concerns."),
                    "question": s("The question, answerable yes/no or with a short answer.")
                }),
                &["question"],
            ),
        ),
        tool(
            "adjourn",
            "Move to close the council and render a verdict. A majority of seats must confirm, so move it only when the record is genuinely decided — ending early to escape a hard argument will be voted down.",
            obj(json!({ "reason": s("Why stopping now is right.") }), &["reason"]),
        ),
    ]
}

/// The tool offered once, at the very end, to state the answer.
pub fn conclude_tool() -> Vec<Value> {
    vec![tool(
        "conclude",
        "Answer the motion in a sentence or two, in the fewest words that carry the result. Set `agreed` true only if the board actually settled the question. When it did not, say so plainly and give the lean: which way the evidence pointed even without agreement. Never hedge, never summarise the debate again.",
        obj(
            json!({
                "agreed": {
                    "type": "boolean",
                    "description": "true only when the board settled the motion."
                },
                "answer": s("The direct answer, one or two sentences. Empty when nothing was settled."),
                "lean": s("Which way the evidence pointed even without agreement.")
            }),
            &["agreed"],
        ),
    )]
}

/// The summarizer's whole toolbelt: one instrument, used at most once per
/// intermission. It never argues, votes, polls, reviews, or adjourns — its
/// only power is to ask the operator for more rounds.
pub fn summarizer_tools() -> Vec<Value> {
    vec![
        tool(
            "settle",
            "Declare that the board has done its work and the debate should end here. The engine ends a debate on its own only when every single point is decided; that is a count, not a reading, and it cannot see that five points carried decisively matters more than one still deadlocked. Use this when the record has actually converged — and name what has converged, so the ending says why. Do not use it to end a debate early because it is going badly.",
            obj(
                json!({
                    "reason": s("What the board has settled, and what is left open. One or two sentences.")
                }),
                &["reason"],
            ),
        ),
        tool(
            "propose_extension",
        "Ask the operator for more rounds, when the board cannot settle in the rounds remaining. Use this at most once per intermission, only for genuinely undecided business — never to chase a better outcome for one side. The operator decides; if they decline, summarize the round as usual.",
        obj(
            json!({
                "rounds": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "How many extra rounds the board needs."
                },
                "reason": s("Why the remaining rounds are not enough.")
            }),
                &["rounds"],
            ),
        ),
    ]
}

/// The tool offered to each seat while a deadlock review is open. The vote
/// is optional but always available: a review is the sanctioned moment to
/// change your mind on the point, and the engine applies it immediately.
pub fn review_tool() -> Vec<Value> {
    vec![tool(
        "stalemate_ballot",
        "Rule on why the point under review is deadlocked: `unsolvable` if no record could settle it, `too_dense` if it is too dense for this council here, `decidable` if the deadlock is a mistake. You may also change your vote on the point in the same call — this is the moment to do it.",
        obj(
            json!({
                "reading": {
                    "type": "string",
                    "enum": ["unsolvable", "too_dense", "decidable"],
                    "description": "why the point is deadlocked"
                },
                "reason": s("One sentence of justification."),
                "vote": {
                    "type": "string",
                    "enum": ["win", "lose", "stalemate"],
                    "description": "Optional changed vote on the point under review."
                }
            }),
            &["reading"],
        ),
    )]
}

/// The tool offered to each seat while an adjournment motion is open.
pub fn adjourn_tool() -> Vec<Value> {
    vec![tool(
        "adjourn_ballot",
        "Answer the open motion to adjourn: true if the record is genuinely decided and closing now is right, false if live disagreement would be buried. Say why in one sentence.",
        obj(
            json!({
                "confirm": {
                    "type": "boolean",
                    "description": "true to close the council now, false to keep deliberating"
                },
                "reason": s("One sentence of justification.")
            }),
            &["confirm"],
        ),
    )]
}

/// The single tool offered while a poll is open.
pub fn ballot_tool() -> Vec<Value> {
    vec![tool(
        "poll_ballot",
        "Record your answer to the open council poll. Pick one of the offered options.",
        obj(
            json!({
                "choice": s("The option you are voting for."),
                "reason": s("One sentence of justification.")
            }),
            &["choice"],
        ),
    )]
}

/// Fallback for servers with no native tool-calling: ask for a fenced JSON block
/// and scrape it. Kept deliberately simple — the same `Action` parser runs on it.
///
/// Built on demand rather than held as a constant because it is a *description
/// of a belt*, and the belt is not the same for every seat: only the Arbiter is
/// offered `rule`. It used to be a `const` that nothing ever referenced, so a
/// non-native model was told to emit tool calls and never told what one looked
/// like — the scraper simply never saw anything to parse.
pub fn inline_protocol(judging: bool) -> String {
    let mut shapes = String::from(
        r#"End every turn with your calls — speak, then act. One turn usually means several blocks: a vote on each point you engaged with is several `vote` calls, one per point. The only legal block shapes are:

{"tool":"make_point","args":{"text":"...","beats":[1,2]}}
{"tool":"vote","args":{"point":1,"verdict":"win|lose|stalemate","reason":"..."}}
{"tool":"concede","args":{"point":1,"reason":"..."}}
{"tool":"withdraw","args":{"point":1,"reason":"..."}}
{"tool":"poll","args":{"question":"...","options":["...","..."],"reason":"..."}}
{"tool":"review","args":{"point":1,"reason":"..."}}
{"tool":"steelman","args":{"point":1,"restatement":"..."}}
{"tool":"rebut","args":{"point":1,"argument":"..."}}
{"tool":"cite","args":{"source":"...","quote":"...","point":1,"stance":"supports|undermines|neutral"}}
{"tool":"press","args":{"to":"<seat name as listed>","point":1,"question":"..."}}"#,
    );
    if judging {
        shapes.push_str(
            "\n{\"tool\":\"rule\",\"args\":{\"point\":1,\"ruling\":\"carries|falls\",\"reason\":\"...\"}}",
        );
    }
    shapes.push_str("\n{\"tool\":\"adjourn\",\"args\":{\"reason\":\"...\"}}");
    format!(
        "This server has no tool calling, so you write your calls yourself. When you act, \
         finish your message with a fenced block:\n\n\
         ```tool\n\
         {{\"tool\": \"<name>\", \"args\": {{ ... }} }}\n\
         ```\n\n\
         {shapes}\n\n\
         `to` is a seat's name, exactly as the council is listed, or its id. `point` is the bare number \
         the board shows. Write the block after your prose, not inside it."
    )
}

/// A reply with its tool calls taken out.
///
/// `calls` is what the model did; the kept lines are what it *said*. The two
/// have to be separated: a model that writes its call inline means it, but the
/// transcript should show the sentence around it, not a wall of JSON.
pub struct Scraped<'a> {
    pub calls: Vec<RawToolCall>,
    lines: Vec<&'a str>,
    kept: Vec<usize>,
}

impl<'a> Scraped<'a> {
    /// The reply as prose, with every consumed tool call removed.
    pub fn text(&self) -> String {
        self.kept
            .iter()
            .map(|i| self.lines[*i])
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    }

    /// Only the lines from `from` onwards. Used where the reply was scanned as
    /// thinking-then-speech, and only the speech half is for display.
    pub fn text_from(&self, from: usize) -> String {
        self.kept
            .iter()
            .filter(|i| **i >= from)
            .map(|i| self.lines[*i])
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    }
}

/// Take `{"tool": ..., "args": ...}` objects out of a plain-text response, and
/// report what is left.
///
/// Two passes over the *whole* text. Fenced blocks are the protocol the prompt
/// asks for; bare objects on a line of their own are what models actually emit
/// when they skip the fence. Both use brace matching rather than "is this
/// whole blob valid JSON", because models pretty-print, add a closing remark,
/// and put two calls in one block — each of which used to drop the call.
pub fn scrape_inline_actions(text: &str) -> Scraped<'_> {
    let lines: Vec<&str> = text.lines().collect();
    let mut calls: Vec<RawToolCall> = Vec::new();
    let mut drop = vec![false; lines.len()];

    // Pass 1: fenced blocks. A fence that yielded no call is left alone — it
    // is the model quoting the protocol, not acting.
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("```") {
            let mut j = i + 1;
            while j < lines.len() && !lines[j].trim_start().starts_with("```") {
                j += 1;
            }
            let body = lines[(i + 1).min(lines.len())..j.min(lines.len())].join("\n");
            let found = json_objects(&body);
            if !found.is_empty() {
                for v in found {
                    if let Some(c) = tool_call_from(&v, &mut calls.len()) {
                        calls.push(c);
                    }
                }
                for k in i..=j.min(lines.len().saturating_sub(1)) {
                    drop[k] = true;
                }
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }

    // Pass 2: bare objects on a line of their own. A line carrying prose as
    // well as the object is the model discussing a call, not making one, and
    // `starts_with` rejects it. Everything else is taken as real: a model that
    // writes the object and then explains itself has acted.
    for (idx, line) in lines.iter().enumerate() {
        if drop[idx] {
            continue;
        }
        let l = line.trim();
        if !l.starts_with("{\"tool\":") && !l.starts_with("{ \"tool\"") {
            continue;
        }
        let found = json_objects(l);
        if found.is_empty() {
            continue;
        }
        for v in found {
            if let Some(c) = tool_call_from(&v, &mut calls.len()) {
                calls.push(c);
            }
        }
        drop[idx] = true;
    }

    let kept = (0..lines.len()).filter(|i| !drop[*i]).collect();
    Scraped { calls, lines, kept }
}

/// Is this one of the reading tools?
///
/// They answer the model rather than the board, so the engine handles them
/// before any board action is applied — otherwise every one of them would be
/// refused as an unknown tool.
pub fn is_web(name: &str) -> bool {
    matches!(name, "web_search" | "web_fetch" | "search_web" | "fetch_page")
}

/// Scrape and keep only the calls, for callers that do not display the reply.
pub fn parse_inline_actions(text: &str) -> Vec<RawToolCall> {
    scrape_inline_actions(text).calls
}

/// Every top-level `{...}` object in `body`, by brace depth. String contents
/// are skipped so a brace inside a reason string does not throw the count off.
fn json_objects(body: &str) -> Vec<Value> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        if let Ok(v) = serde_json::from_str::<Value>(&body[start..=i]) {
                            out.push(v);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Turn a parsed object into a tool call, if it looks like one.
fn tool_call_from(v: &Value, seq: &mut usize) -> Option<RawToolCall> {
    let name = v
        .get("tool")
        .or_else(|| v.get("name"))
        .and_then(|n| n.as_str())?
        .to_string();
    let args = v
        .get("args")
        .or_else(|| v.get("arguments"))
        .or_else(|| v.get("params"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    *seq += 1;
    Some(RawToolCall {
        id: format!("inline-{seq}"),
        name,
        arguments: args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: Value) -> RawToolCall {
        RawToolCall {
            id: "t".into(),
            name: name.into(),
            arguments: args,
        }
    }

    #[test]
    fn parses_a_plain_vote() {
        let a = parse_action(&call(
            "vote",
            json!({"point": 3, "verdict": "win", "reason": "holds up"}),
        ))
        .unwrap();
        assert_eq!(
            a,
            Action::Vote {
                point: 3,
                verdict: Verdict::Win,
                reason: Some("holds up".into())
            }
        );
    }

    #[test]
    fn tolerates_alternate_key_names() {
        let a = parse_action(&call("vote", json!({"pointId": "P7", "decision": "refuted"}))).unwrap();
        assert_eq!(
            a,
            Action::Vote {
                point: 7,
                verdict: Verdict::Lose,
                reason: None
            }
        );
    }

    #[test]
    fn accepts_arguments_encoded_as_a_string() {
        let a = parse_action(&call(
            "make_point",
            json!(r#"{"text": "sky is blue", "beats": [2]}"#),
        ))
        .unwrap();
        assert_eq!(
            a,
            Action::MakePoint {
                text: "sky is blue".into(),
                beats: vec![2]
            }
        );
    }

    #[test]
    fn rejects_unknown_verdicts() {
        assert!(parse_action(&call("vote", json!({"point":1,"verdict":"maybe"}))).is_err());
    }

    #[test]
    fn scrape_inline_blocks() {
        let text = "Reasoning here.\n\n```tool\n{\"tool\":\"vote\",\"args\":{\"point\":1,\"verdict\":\"lose\"}}\n```\nand more.\n```tool\n{\"tool\":\"adjourn\",\"args\":{\"reason\":\"done\"}}\n```";
        let calls = parse_inline_actions(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "vote");
        assert_eq!(calls[1].name, "adjourn");
        assert_eq!(
            parse_action(&calls[0]).unwrap(),
            Action::Vote {
                point: 1,
                verdict: Verdict::Lose,
                reason: None
            }
        );
    }

    #[test]
    fn a_bare_call_before_a_fence_is_still_found() {
        // Regression: the old scanner only looked at text *after* the last
        // fence, so a call emitted before it vanished.
        let text = "{\"tool\":\"vote\",\"args\":{\"point\":1,\"verdict\":\"win\"}}\n\
                   Here is my reasoning.\n\
                   ```tool\n{\"tool\":\"adjourn\",\"args\":{\"reason\":\"done\"}}\n```";
        let names: Vec<String> = parse_inline_actions(text)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(names.contains(&"vote".to_string()), "bare call lost: {names:?}");
        assert!(names.contains(&"adjourn".to_string()), "fenced call lost: {names:?}");
    }

    #[test]
    fn a_bare_call_between_two_fences_is_still_found() {
        let text = "```tool\n{\"tool\":\"vote\",\"args\":{}}\n```\n\
                   {\"tool\":\"cite\",\"args\":{\"source\":\"s\",\"stance\":\"supports\"}}\n\
                   ```\n```";
        let names: Vec<String> = parse_inline_actions(text)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(names.contains(&"cite".to_string()), "got {names:?}");
        assert_eq!(
            names.iter().filter(|n| *n == "vote").count(),
            1,
            "a fenced call must not also be scraped bare: {names:?}"
        );
    }

    #[test]
    fn a_complete_adjourn_ballot_parses() {
        let text = r#"{"tool":"adjourn_ballot","args":{"confirm":true,"reason":"P1 carried +2 (Advocate win, Critic win, Arbiter stalemate); the structural criterion survives all objections; no live disagreement remains."}}"#;
        let calls = parse_inline_actions(text);
        assert_eq!(calls.len(), 1, "a complete ballot must be readable");
        match parse_action(&calls[0]).expect("parsed") {
            Action::AdjournBallot { confirm, .. } => assert!(confirm),
            other => panic!("wrong action: {other:?}"),
        }
    }

    #[test]
    fn a_ballot_cut_off_by_a_token_limit_is_not_silently_lost() {
        // What a 256-token budget does to a reasoning model: the answer is
        // truncated mid-string, the object never closes, and the seat is
        // counted as having silently abstained.
        let truncated = r#"{"tool":"adjourn_ballot","args":{"confirm":true,"reason":"P1 carried +2 (Advocate win, Critic"#;
        assert!(
            parse_inline_actions(truncated).is_empty(),
            "an unterminated object is not a readable ballot — which is why the engine must not cap these"
        );
    }

    #[test]
    fn scraping_also_removes_the_call_from_the_reply() {
        // The transcript shows what the seat said, not the JSON it used to say
        // it: a summary with the request pasted under it read as a fault.
        let raw = "The board is stuck on P3, so I am asking for more time.\n\
                   ```tool\n{\"tool\":\"propose_extension\",\"args\":{\"rounds\":2,\"reason\":\"P3 is unengaged\"}}\n```";
        let out = scrape_inline_actions(raw);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0].name, "propose_extension");
        let said = out.text();
        assert!(said.contains("The board is stuck on P3"), "{said}");
        assert!(!said.contains("propose_extension"), "json leaked: {said}");
        assert!(!said.contains("```"), "fence leaked: {said}");
    }

    #[test]
    fn a_bare_call_is_removed_from_the_reply_too() {
        let raw = "I concede the narrow version.\n{\"tool\":\"concede\",\"args\":{\"point\":1}}";
        let out = scrape_inline_actions(raw);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.text(), "I concede the narrow version.");
    }

    #[test]
    fn a_quoted_example_is_neither_acted_on_nor_deleted() {
        // The protocol is quoted back at us; that is prose, and prose stays.
        let raw = "The shape is {\"tool\":\"vote\",\"args\":{\"point\":1}} but I will vote.";
        let out = scrape_inline_actions(raw);
        assert!(out.calls.is_empty());
        assert_eq!(out.text(), raw);
    }

    #[test]
    fn text_from_limits_the_reply_to_its_speech_half() {
        // The engine scrapes thinking-then-speech together so a fence spanning
        // the boundary is still caught, but must show only the speech.
        let combined = "I should call vote on P1.\n\
                        ```tool\n{\"tool\":\"vote\",\"args\":{\"point\":1}}\n```\n\
                        I vote win on P1.";
        let thought_lines = 1;
        let out = scrape_inline_actions(combined);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.text_from(thought_lines), "I vote win on P1.");
        assert!(out.text().contains("I should call vote"), "thinking kept");
    }

    #[test]
    fn inline_scraper_ignores_a_stray_fence_and_salvages_a_truncated_one() {
        // A fence with nothing in it is just prose punctuation.
        assert!(parse_inline_actions("just prose, and a ``` stray fence").is_empty());
        // But a fence left unterminated by a token limit still holds a
        // complete, well-formed call: the action is real, so it is taken rather
        // than thrown away with the missing closing backticks.
        let salvaged = parse_inline_actions("```tool\n{\"tool\":\"vote\",\"args\":{}}");
        assert_eq!(salvaged.len(), 1);
        assert_eq!(salvaged[0].name, "vote");
    }

    #[test]
    fn extension_proposals_need_a_positive_count() {
        let good = call(
            "propose_extension",
            serde_json::json!({"rounds": 2, "reason": "undecided"}),
        );
        assert!(matches!(
            super::parse_action(&good),
            Ok(super::Action::ProposeExtension { rounds: 2, .. })
        ));
        let bad = call("propose_extension", serde_json::json!({"rounds": 0}));
        assert!(super::parse_action(&bad).is_err());
        // The belt is `settle` and `propose_extension` — one ends the debate,
        // the other extends it, and both belong to the seat that reads the round.
        let tools = super::summarizer_tools();
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(names, vec!["settle", "propose_extension"]);
    }

    #[test]
    fn a_point_id_taken_from_prose_takes_the_first_number_only() {
        // "P1 beat P2" used to join every digit run into point 12.
        assert_eq!(point_from_text("P3"), Some(3));
        assert_eq!(point_from_text("point 7"), Some(7));
        assert_eq!(point_from_text("12"), Some(12));
        assert_eq!(point_from_text("P1 beat P2"), None);
        assert_eq!(point_from_text("points 2 and 3"), None);
        assert_eq!(point_from_text("none"), None);
    }

    #[test]
    fn an_ambiguous_point_id_is_refused_rather_than_guessed() {
        // A refusal costs a round-trip; a wrong target corrupts the record.
        let call = call("vote", serde_json::json!({"point": "P1 beat P2", "verdict": "win"}));
        assert!(super::parse_action(&call).is_err());
    }

    #[test]
    fn string_one_and_zero_count_as_booleans() {
        // Adjournment needs a strict majority, and a missed ballot is counted
        // as silence — so "1" has to parse.
        let yes = call("adjourn_ballot", serde_json::json!({"confirm": "1"}));
        let no = call("adjourn_ballot", serde_json::json!({"confirm": "0"}));
        assert!(matches!(
            super::parse_action(&yes),
            Ok(super::Action::AdjournBallot { confirm: true, .. })
        ));
        assert!(matches!(
            super::parse_action(&no),
            Ok(super::Action::AdjournBallot { confirm: false, .. })
        ));
    }

    #[test]
    fn adjourn_ballots_accept_yes_no_words() {
        let yes = parse_action(&call("adjourn_ballot", json!({"confirm": "yes"}))).unwrap();
        assert!(matches!(
            yes,
            Action::AdjournBallot {
                confirm: true,
                ..
            }
        ));
        let no = parse_action(&call("adjourn_ballot", json!({"confirm": 0}))).unwrap();
        assert!(matches!(
            no,
            Action::AdjournBallot {
                confirm: false,
                ..
            }
        ));
        assert!(parse_action(&call("adjourn_ballot", json!({"confirm": "maybe"}))).is_err());
    }

    #[test]
    fn the_vote_tool_says_how_many_calls_a_turn_takes() {
        // One `vote` call rates one point. Models that are never told this
        // rate a single point per turn while the rest of the board sits silent.
        let tools = super::speaking_tools();
        let vote = tools
            .iter()
            .find(|t| t["function"]["name"] == "vote")
            .expect("the belt carries a vote tool");
        let desc = vote["function"]["description"].as_str().unwrap_or_default();
        assert!(
            desc.contains("once per point") && desc.contains("three `vote` calls"),
            "the vote description must teach batching: {desc}"
        );
    }

    #[test]
    fn descriptions_are_human_readable() {
        let a = Action::Vote {
            point: 4,
            verdict: Verdict::Stalemate,
            reason: Some("record is thin".into()),
        };
        assert_eq!(a.describe(), "voted stalemate on P4 — record is thin");
    }
}

/// The ballot a seat casts on the summarizer's request for more rounds.
pub fn extension_ballot_tool() -> Vec<Value> {
    vec![tool(
        "extension_ballot",
        "Answer the summarizer's request for extra rounds. Grant it only if the board genuinely has something left to settle that another round would settle; decline if the argument is repeating itself. One sentence of reason.",
        obj(
            json!({
                "grant": {
                    "type": "boolean",
                    "description": "true to grant the extra rounds, false to decline."
                },
                "reason": s("Why, in a sentence. What is still open, or why it is not.")
            }),
            &["grant"],
        ),
    )]
}

#[cfg(test)]
mod rule_tests {
    use super::*;
    use serde_json::json;

    fn call(args: serde_json::Value) -> RawToolCall {
        RawToolCall {
            id: "x".into(),
            name: "rule".into(),
            arguments: args,
        }
    }


    #[test]
    fn a_ruling_needs_a_point_a_direction_and_a_reason() {
        let ok = parse_action(&call(json!({
            "point": 3, "ruling": "carries", "reason": "the objection was answered"
        })))
        .expect("a well-formed ruling");
        match ok {
            Action::Rule { point, ruling, reason } => {
                assert_eq!(point, 3);
                assert_eq!(ruling, Ruling::Carries);
                assert_eq!(reason, "the objection was answered");
            }
            other => panic!("expected a rule, got {other:?}"),
        }

        // Every one of these settles a point outright, so none may slip through.
        for args in [
            json!({}),
            json!({"point": 3}),
            json!({"point": 3, "ruling": "carries"}),
            json!({"point": 3, "reason": "because"}),
            json!({"ruling": "carries", "reason": "because"}),
        ] {
            assert!(
                parse_action(&call(args.clone())).is_err(),
                "{args} should not parse as a ruling"
            );
        }
    }

    #[test]
    fn a_ruling_in_an_unknown_direction_is_refused() {
        // "Carries" and "falls" are the only two words that settle anything. A
        // model that says "mostly true" has not ruled on anything.
        let why = parse_action(&call(json!({
            "point": 1, "ruling": "mostly true", "reason": "because"
        })))
        .expect_err("an unclear ruling");
        assert!(why.contains("carries") || why.contains("falls"), "{why}");
        for word in ["carries", "falls", "upheld", "struck", "win", "lose", "stands"] {
            assert!(
                parse_action(&call(json!({"point": 1, "ruling": word, "reason": "r"}))).is_ok(),
                "{word} should be understood"
            );
        }
    }

    #[test]
    fn a_ruling_survives_the_inline_scraper() {
        // Models that do not call tools natively write the object as prose, so
        // a tool only reachable natively would be unreachable in practice.
        let raw = concat!(
            "It carries.\n",
            r#"{"tool":"rule","args":{"point":1,"ruling":"falls","reason":"refuted"}}"#
        );
        let calls = scrape_inline_actions(raw).calls;
        assert_eq!(calls.len(), 1, "the call should be found in prose");
        match parse_action(&calls[0]).expect("and parsed") {
            Action::Rule { point, ruling, .. } => {
                assert_eq!(point, 1);
                assert_eq!(ruling, Ruling::Falls);
            }
            other => panic!("expected a rule, got {other:?}"),
        }
    }

    #[test]
    fn an_extension_ballot_is_a_yes_or_a_no() {
        let b = |args: serde_json::Value| {
            parse_action(&RawToolCall {
                id: "x".into(),
                name: "extension_ballot".into(),
                arguments: args,
            })
        };
        assert!(matches!(
            b(json!({"grant": true})),
            Ok(Action::ExtensionBallot { grant: true, .. })
        ));
        assert!(matches!(
            b(json!({"grant": false, "reason": "we have said it"})),
            Ok(Action::ExtensionBallot { grant: false, .. })
        ));
        // A ballot with no answer is not an abstention that counts as no; it is
        // no answer at all, and the room is one seat lighter.
        assert!(b(json!({})).is_err());
    }
}
