//! End-to-end engine tests driven by a scripted backend — no network, no
//! server, but the real state machine, the real board, the real scoring.

use async_trait::async_trait;
use council_core::client::{ChatBackend, ChatChunk, ChatRequest, ChatResponse, LlmError};
use council_core::engine::{Engine, EventSink, PendingPrompt};
use council_core::events::{BallotKind, EngineEvent, NoticeTone};
use council_core::tools::RawToolCall;
use council_core::types::*;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Replies with a fixed script, in order. Each script is a list of turns for
/// one delegate, consumed as that delegate speaks.
struct Script {
    turns: Mutex<Vec<ChatResponse>>,
    seen: Mutex<Vec<ChatRequest>>,
}

impl Script {
    fn new(turns: Vec<ChatResponse>) -> Self {
        Self {
            turns: Mutex::new(turns),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn text(content: &str) -> ChatResponse {
        ChatResponse {
            content: content.into(),
            ..Default::default()
        }
    }

    /// A response that takes the no-native-tools path.
    fn inline(body: &str) -> ChatResponse {
        ChatResponse {
            content: body.into(),
            ..Default::default()
        }
    }

    /// A reply the server cut short — as distinct from a seat that chose to say
    /// nothing, which looks identical without a finish reason to go on.
    fn cut_off(content: &str) -> ChatResponse {
        ChatResponse {
            content: content.into(),
            finish_reason: Some("length".into()),
            ..Default::default()
        }
    }

    fn call(name: &str, args: serde_json::Value) -> ChatResponse {
        ChatResponse {
            tool_calls: vec![RawToolCall {
                id: "x".into(),
                name: name.into(),
                arguments: args,
            }],
            ..Default::default()
        }
    }

    /// A corrective re-prompt response: the model still doesn't act, so the
    /// turn is labelled as inaction.
    fn corrective() -> ChatResponse {
        ChatResponse {
            content: "Understood — I have nothing further to add.".into(),
            ..Default::default()
        }
    }

    fn thinking(reasoning: &str) -> ChatResponse {
        ChatResponse {
            reasoning: reasoning.into(),
            ..Default::default()
        }
    }
}

#[async_trait]
impl ChatBackend for Script {
    async fn chat(
        &self,
        req: &ChatRequest,
        on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
    ) -> Result<ChatResponse, LlmError> {
        self.seen.lock().expect("seen").push(ChatRequest {
            endpoint: req.endpoint.clone(),
            model: req.model.clone(),
            messages: req.messages.clone(),
            tools: req.tools.clone(),
            tool_choice: req.tool_choice.clone(),
            temperature: req.temperature,
            max_tokens: req.max_tokens,
        });
        let mut turns = self.turns.lock().expect("turns");
        let resp = if turns.is_empty() {
            // Running dry: the model says nothing and calls nothing, which is
            // exactly the failure mode the engine has to survive.
            ChatResponse::default()
        } else {
            turns.remove(0)
        };
        if !resp.content.is_empty() {
            on_chunk(ChatChunk::Content(&resp.content));
        }
        if !resp.reasoning.is_empty() {
            on_chunk(ChatChunk::Reasoning(&resp.reasoning));
        }
        Ok(resp)
    }
}

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<EngineEvent>>,
}

impl EventSink for Recorder {
    fn emit(&self, event: EngineEvent) {
        self.events.lock().expect("events").push(event);
    }
}

impl Recorder {
    fn kinds(&self) -> Vec<&'static str> {
        self.events
            .lock()
            .expect("events")
            .iter()
            .map(|e| match e {
                EngineEvent::DebateStarted { .. } => "started",
                EngineEvent::RoundStarted { .. } => "round",
                EngineEvent::TurnStarted { .. } => "turnStart",
                EngineEvent::YourTurn { .. } => "yourTurn",
                EngineEvent::BallotAsked { .. } => "ballotAsked",
                EngineEvent::TurnProgress { .. } => "progress",
                EngineEvent::TurnActivity { .. } => "activity",
                EngineEvent::TurnSkipped { .. } => "skipped",
                EngineEvent::Token { .. } => "token",
                EngineEvent::TurnUsage { .. } => "usage",
                EngineEvent::WebFetched { .. } => "webFetched",
                EngineEvent::Reasoning { .. } => "reasoning",
                EngineEvent::SpeechFinished { .. } => "speech",
                EngineEvent::ActionTaken { .. } => "action",
                EngineEvent::ActionRejected { .. } => "rejected",
                EngineEvent::PointUpdated { .. } => "point",
                EngineEvent::PollOpened { .. } => "pollOpen",
                EngineEvent::PollBallotCast { .. } => "ballot",
                EngineEvent::PollClosed { .. } => "pollClose",
                EngineEvent::ReviewOpened { .. } => "reviewOpen",
                EngineEvent::ReviewBallotCast { .. } => "reviewBallot",
                EngineEvent::ReviewClosed { .. } => "reviewClose",
                EngineEvent::AdjournMoved { .. } => "adjournMoved",
                EngineEvent::AdjournBallotCast { .. } => "adjournBallot",
                EngineEvent::AdjournDecided { .. } => "adjournDecided",
                EngineEvent::IntermissionStarted { .. } => "intermission",
                EngineEvent::ExtensionProposed { .. } => "extProposed",
                EngineEvent::ExtensionBallotCast { .. } => "extBallot",
                EngineEvent::ExtensionGranted { .. } => "extGranted",
                EngineEvent::ExtensionDeclined { .. } => "extDeclined",
                EngineEvent::RosterUpdated { .. } => "roster",
                EngineEvent::Notice { .. } => "notice",
                EngineEvent::DebateEnded { .. } => "ended",
                EngineEvent::Failed { .. } => "failed",
            })
            .collect()
    }

    fn verdicts(&self) -> Vec<String> {
        self.events
            .lock()
            .expect("events")
            .iter()
            .filter_map(|e| match e {
                EngineEvent::DebateEnded { verdict } => Some(verdict.summary.clone()),
                _ => None,
            })
            .collect()
    }

    fn failures(&self) -> Vec<String> {
        self.events
            .lock()
            .expect("events")
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Failed { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    /// Every finished speech on the record, delegate by delegate.
    fn speeches(&self) -> Vec<Speech> {
        self.events
            .lock()
            .expect("events")
            .iter()
            .filter_map(|e| match e {
                EngineEvent::SpeechFinished { speech, .. } => Some(speech.clone()),
                _ => None,
            })
            .collect()
    }

    fn final_board(&self) -> Vec<(u32, &'static str, i32)> {
        let events = self.events.lock().expect("events");
        let mut board: std::collections::BTreeMap<u32, (&'static str, i32)> = Default::default();
        for e in events.iter() {
            if let EngineEvent::PointUpdated { point, tally, .. } = e {
                let status = match tally.status {
                    PointStatus::Open => "open",
                    PointStatus::Carried => "carried",
                    PointStatus::Fell => "fell",
                    PointStatus::Stalemate => "stalemate",
                    PointStatus::Withdrawn => "withdrawn",
                };
                board.insert(point.id, (status, tally.score));
            }
        }
        board.into_iter().map(|(k, v)| (k, v.0, v.1)).collect()
    }
}

fn config() -> DebateConfig {
    DebateConfig {
        motion: "Roses outrank lilies as a gift flower".into(),
        endpoints: vec![EndpointConfig {
            id: "local".into(),
            name: "Local".into(),
            base_url: "http://127.0.0.1:8000/v1".into(),
            native_tools: true,
            ..Default::default()
        }],
        delegates: vec![
            DelegateConfig {
                id: "a".into(),
                label: "Advocate".into(),
                model: "m-a".into(),
                endpoint_id: "local".into(),
                color: "#ff5555".into(),
                seat_role: SeatRole::Proponent,
                ..Default::default()
            },
            DelegateConfig {
                id: "b".into(),
                label: "Critic".into(),
                model: "m-b".into(),
                endpoint_id: "local".into(),
                color: "#55aaff".into(),
                seat_role: SeatRole::Opponent,
                ..Default::default()
            },
            DelegateConfig {
                id: "c".into(),
                label: "Judge".into(),
                model: "m-c".into(),
                endpoint_id: "local".into(),
                color: "#55ffaa".into(),
                seat_role: SeatRole::Judge,
                ..Default::default()
            },
            DelegateConfig {
                id: "s".into(),
                label: "Summarizer".into(),
                model: "m-s".into(),
                endpoint_id: "local".into(),
                color: "#bffffa".into(),
                seat_role: SeatRole::Summarizer,
                ..Default::default()
            },
        ],
        max_rounds: 2,
        closing_statements: false,
        auto_adjourn: false,
        ..Default::default()
    }
}

async fn run(script: Vec<ChatResponse>, cfg: DebateConfig) -> (Arc<Recorder>, Arc<Script>) {
    let rec = Arc::new(Recorder::default());
    let backend = Arc::new(Script::new(script));
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Engine::with_backend(backend.clone(), sink);
    engine.run(cfg).await;
    (rec, backend)
}

#[tokio::test]
async fn a_point_carries_when_more_delegates_say_win() {
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses last longer than lilies"})),
            Script::call("vote", json!({"point": 1, "verdict": "win", "reason": "true"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose", "reason": "false"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
        ],
        config(),
    )
    .await;
    assert!(rec.failures().is_empty(), "{:?}", rec.failures());
    assert_eq!(rec.final_board(), vec![(1, "carried", 1)]);
    assert!(rec.verdicts()[0].contains("1 of 1 points carried"));
}

#[tokio::test]
async fn votes_that_cancel_out_are_a_stalemate() {
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Lilies are cheaper"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::call("vote", json!({"point": 1, "verdict": "stalemate"})),
        ],
        config(),
    )
    .await;
    // a's implicit win is cancelled by b's lose, and c will not go further:
    // the council deadlocked, so the point is a stalemate, not a coin-flip.
    assert_eq!(rec.final_board(), vec![(1, "stalemate", 0)]);
}

#[tokio::test]
async fn a_point_nobody_else_has_touched_is_not_settled() {
    // The proponent's implicit vote alone must not read as a carried point.
    let p = Point {
        id: 1,
        text: "Roses are cheaper".into(),
        raised_by: "a".into(),
        round: 1,
        withdrawn: false,
        withdrawn_reason: None,
        beats: vec![],
        votes: Default::default(),
        rebuttals: vec![],
        steelmen: vec![],
        evidence: vec![],
        conceded_by: Default::default(),
        review: None,
        ruling: None,
    };
    let roster = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let t = p.tally(&roster);
    assert_eq!(t.status, PointStatus::Carried);
    assert!(!t.contested);
    assert!(!t.settled(), "a point nobody has engaged with is still live");
}

#[tokio::test]
async fn conceding_is_recorded_as_a_loss_vote() {
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses cause allergies"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("concede", json!({"point": 1, "reason": "that is true"})),
            Script::text("no comment"),
        ],
        config(),
    )
    .await;
    // a's implicit win + b's win against c's concession -> carried by one.
    assert_eq!(rec.final_board(), vec![(1, "carried", 1)]);
    let actions = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ActionTaken { action, .. } => Some(action.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(actions.contains(&"concede".to_string()), "{actions:?}");
}

#[tokio::test]
async fn conceding_your_own_point_withdraws_it() {
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Lilies are toxic to cats"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::text("surely not"),
            Script::corrective(),
            // Consumed by the intermission between rounds.
            Script::text("round one: a point stands"),
            Script::call("concede", json!({"point": 1, "reason": "I made that up"})),
        ],
        config(),
    )
    .await;
    assert_eq!(rec.final_board(), vec![(1, "withdrawn", 0)]);
}

#[tokio::test]
async fn a_delegate_may_change_its_vote() {
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses are cheaper"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            // Consumed by the intermission between rounds.
            Script::text("round one: a point falls"),
            Script::call("vote", json!({"point": 1, "verdict": "lose", "reason": "rethought it"})),
        ],
        config(),
    )
    .await;
    // a gave up its own point, so everyone rates it lose and it falls.
    assert_eq!(rec.final_board(), vec![(1, "fell", -3)]);
    let log = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .any(|e| matches!(e, EngineEvent::ActionTaken { detail, .. } if detail.contains("re-voted")));
    assert!(log, "a changed vote should say so");
}

#[tokio::test]
async fn voting_on_a_point_that_does_not_exist_is_refused_not_fatal() {
    let (rec, _) = run(
        vec![
            Script::call("vote", json!({"point": 99, "verdict": "win"})),
            Script::text("ok"),
            Script::text("ok"),
            Script::text("ok"),
            Script::text("ok"),
        ],
        config(),
    )
    .await;
    assert!(rec.final_board().is_empty());
    let rejected = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .any(|e| matches!(e, EngineEvent::ActionRejected { reason, .. } if reason.contains("does not exist")));
    assert!(rejected);
    // The debate still ran to completion.
    assert!(rec.kinds().contains(&"ended"));
}

#[tokio::test]
async fn a_poll_is_put_to_every_other_delegate_and_resolves() {
    let (rec, _) = run(
        vec![
            Script::call(
                "poll",
                json!({"question": "Which is the better gift?", "options": ["roses", "lilies"]}),
            ),
            Script::call("poll_ballot", json!({"choice": "roses", "reason": "classic"})),
            Script::call("poll_ballot", json!({"choice": "roses"})),
            Script::text("that is a silly poll"),
            Script::text("agreed"),
            Script::text("agreed"),
        ],
        config(),
    )
    .await;
    let events = rec.events.lock().expect("events");
    let outcome = events.iter().find_map(|e| match e {
        EngineEvent::PollClosed { outcome, .. } => Some(outcome.clone()),
        _ => None,
    });
    assert_eq!(outcome, Some(Some("roses".to_string())));
    let ballots = events
        .iter()
        .filter(|e| matches!(e, EngineEvent::PollBallotCast { .. }))
        .count();
    assert_eq!(ballots, 2, "the opener does not vote in their own poll");
    drop(events);
    assert!(rec.final_board().is_empty(), "a poll is not a point");
}

#[tokio::test]
async fn a_tied_poll_is_reported_as_inconclusive() {
    let (rec, _) = run(
        vec![
            Script::call("poll", json!({"question": "Roses or lilies?", "options": ["roses", "lilies"]})),
            Script::call("poll_ballot", json!({"choice": "roses"})),
            Script::call("poll_ballot", json!({"choice": "lilies"})),
            Script::text("unclear"),
            Script::text("unclear"),
            Script::text("unclear"),
        ],
        config(),
    )
    .await;
    let events = rec.events.lock().expect("events");
    let outcome = events.iter().find_map(|e| match e {
        EngineEvent::PollClosed { outcome, .. } => Some(outcome.clone()),
        _ => None,
    });
    assert_eq!(outcome, Some(None), "a 1-1 split must not pick a winner");
}

#[tokio::test]
async fn adjourning_stops_the_debate_immediately_once_confirmed() {
    // One motion, two confirmations, all inside the first turn: the council
    // closes 3–0 without another seat ever speaking.
    let (rec, _) = run(
        vec![
            Script::call("adjourn", json!({"reason": "we are going in circles"})),
            Script::call("adjourn_ballot", json!({"confirm": true})),
            Script::call("adjourn_ballot", json!({"confirm": "yes"})),
            // These should never be requested.
            Script::call("make_point", json!({"text": "should never appear"})),
            Script::text("unreachable"),
        ],
        DebateConfig {
            max_rounds: 5,
            ..config()
        },
    )
    .await;
    assert!(rec.final_board().is_empty());
    let kinds = rec.kinds();
    // One speaking turn plus the summarizer's end result: closing still runs
    // nothing (disabled here), but the conclusion always gets its turn.
    assert_eq!(kinds.iter().filter(|k| **k == "turnStart").count(), 2);
    assert!(kinds.contains(&"ended"));
    let decided = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::AdjournDecided {
                passed,
                yes,
                no,
                ..
            } => Some((*passed, *yes, *no)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decided, vec![(true, 3, 0)]);
    let by = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => Some(verdict.adjourned_by.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(by, vec![Some("a".to_string())]);
    let how = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => Some(verdict.ended_how),
            _ => None,
        })
        .collect::<Vec<_>>();
    use council_core::types::EndedHow;
    assert_eq!(how, vec![EndedHow::Adjourned]);
}

#[tokio::test]
async fn pressing_a_delegate_forces_an_answer_on_their_next_turn() {
    let (_, backend) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses are toxic"})),
            Script::call("press", json!({"to": "Judge", "point": 1, "question": "Do you accept this?"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::text("I do not."),
        ],
        config(),
    )
    .await;
    let seen = backend.seen.lock().expect("seen");
    // Requests: a's turn, b's turn, c's turn, then the intermission, ...
    let judge_turn = seen
        .iter()
        .nth(2)
        .expect("third request is the Judge's turn");
    assert!(
        judge_turn
            .messages
            .iter()
            .any(|m| m.role == "user" && m.content.contains("You are being pressed")),
        "the Judge should have been asked to answer directly"
    );
    assert!(judge_turn
        .messages
        .iter()
        .any(|m| m.content.contains("Do you accept this?")));
}

#[tokio::test]
async fn every_delegate_is_shown_the_whole_board() {
    let (_, backend) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses outlast lilies"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::text("I will vote on it."),
        ],
        config(),
    )
    .await;
    let seen = backend.seen.lock().expect("seen");
    // The first request happens before any point exists; from the second on,
    // every speaking seat must be shown the whole board. The intermission and
    // the conclusion are not speaking turns and carry their own briefs.
    assert!(!seen[0]
        .messages
        .iter()
        .any(|m| m.content.contains("Roses outlast lilies")));
    let speaking: Vec<_> = seen
        .iter()
        .skip(1)
        .filter(|r| r.tools.iter().any(|t| t["function"]["name"] == "vote"))
        .collect();
    assert!(!speaking.is_empty(), "no speaking turns were requested");
    for (i, req) in speaking.iter().enumerate() {
        assert!(
            req.messages
                .iter()
                .any(|m| m.content.contains("Roses outlast lilies")),
            "speaking request {i} did not include the board"
        );
    }
}

#[tokio::test]
async fn the_engine_survives_a_model_that_does_nothing() {
    let (rec, _) = run(vec![], config()).await;
    assert!(rec.failures().is_empty());
    assert!(rec.kinds().contains(&"ended"));
    assert!(rec.verdicts()[0].contains("no points"));
}

#[tokio::test]
async fn the_toolbelt_offers_vote_concede_and_poll() {
    let tools = council_core::tools::speaking_tools();
    let names: Vec<String> = tools
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap_or_default().to_string())
        .collect();
    for expected in ["make_point", "vote", "concede", "poll"] {
        assert!(names.contains(&expected.to_string()), "missing {expected}");
    }
    let vote = tools
        .iter()
        .find(|t| t["function"]["name"] == "vote")
        .expect("vote tool");
    let verdicts = vote["function"]["parameters"]["properties"]["verdict"]["enum"]
        .as_array()
        .expect("verdict enum");
    let listed: Vec<&str> = verdicts.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(listed, vec!["win", "lose", "stalemate"]);
}

/// The four essentials plus `n` extra seats, which is the configuration the
/// setup panel builds and the one nothing tested.
///
/// Returns the new seats' ids as well: most of what needs checking is about a
/// seat nobody has a name for.
fn council_with_custom_seats(n: usize) -> (DebateConfig, Vec<String>) {
    let mut cfg = config();
    let ids: Vec<String> = (0..n)
        .map(|i| {
            let id = format!("x{i}");
            cfg.delegates.push(DelegateConfig {
                id: id.clone(),
                label: format!("Delegate {}", i + 5),
                model: format!("m-{id}"),
                endpoint_id: "local".into(),
                color: "#8888ff".into(),
                seat_role: SeatRole::Custom,
                ..Default::default()
            });
            id
        })
        .collect();
    (cfg, ids)
}

#[tokio::test]
async fn an_extra_seat_takes_its_turn_like_any_other() {
    let (mut cfg, _) = council_with_custom_seats(2);
    cfg.max_rounds = 1;
    // Speaking order is a, b, c, then the extras; the summarizer speaks last.
    let (rec, engine, backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::text("r"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    // They spoke.
    for id in ["x0", "x1"] {
        assert!(
            rec.events.lock().expect("events").iter().any(
                |e| matches!(e, EngineEvent::SpeechFinished { speech, .. } if speech.delegate == id)
            ),
            "the extra seat {id} never got a turn"
        );
    }

    // And their votes counted: 3 up against 2 down.
    let last = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::PointUpdated { point, tally, .. } if point.id == 1 => {
                Some((tally.wins, tally.losses, tally.score))
            }
            _ => None,
        })
        .next_back()
        .expect("a tally for P1");
    assert_eq!(
        last,
        (3, 2, 1),
        "two extra seats add two votes in each direction"
    );

    // And they are on the belt, but not on the Arbiter's instrument.
    for (id, model) in [("x0", "m-x0"), ("x1", "m-x1")] {
        let belts: Vec<Vec<String>> = backend
            .seen
            .lock()
            .expect("seen")
            .iter()
            .filter(|r| r.model == model)
            .map(|r| {
                r.tools
                    .iter()
                    .map(|t| t["function"]["name"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .collect();
        assert!(!belts.is_empty(), "{id} was never asked for anything");
        assert!(
            belts.iter().all(|b| b.contains(&"make_point".to_string())),
            "{id} should have the speaking belt"
        );
        assert!(
            belts.iter().all(|b| !b.contains(&"rule".to_string())),
            "{id} is not the Arbiter and must not be offered `rule`"
        );
    }
}

#[tokio::test]
async fn an_extra_seat_is_asked_about_more_rounds_and_may_read() {
    let (mut cfg, _) = council_with_custom_seats(3);
    cfg.max_rounds = 1;
    cfg.allow_web = true;
    cfg.max_extra_rounds = 1;
    // The extension is offered at the conclusion when the board is deadlocked,
    // not between rounds. Deadlock needs an exact tie, and the raiser's implicit
    // support means one seat has to vote its own point down to get there.
    // Split three-three, which is the only shape where nobody can be skipped:
    // the vote stops asking once the rest cannot change it, so a unanimous
    // "no" would settle after four of six and never reach the extras.
    let no = json!({"grant": false, "reason": "going in circles"});
    let yes = json!({"grant": true, "reason": "P1 deserves another pass"});
    let (rec, engine, _backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::call("vote", json!({"point": 1, "verdict": "stalemate"})),
            Script::call("vote", json!({"point": 1, "verdict": "stalemate"})),
            Script::call("vote", json!({"point": 1, "verdict": "stalemate"})),
            Script::call("vote", json!({"point": 1, "verdict": "stalemate"})),
            Script::text("round laid out"),
            Script::call(
                "propose_extension",
                json!({"rounds": 1, "reason": "P1 is deadlocked"}),
            ),
            Script::call("extension_ballot", no.clone()),
            Script::call("extension_ballot", no.clone()),
            Script::call("extension_ballot", no.clone()),
            Script::call("extension_ballot", yes.clone()),
            Script::call("extension_ballot", yes.clone()),
            Script::call("extension_ballot", yes.clone()),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let asked: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ExtensionBallotCast { delegate, .. } => Some(delegate.clone()),
            _ => None,
        })
        .collect();
    for id in ["x0", "x1", "x2"] {
        assert!(
            asked.contains(&id.to_string()),
            "the extra seat {id} was never asked about more rounds; asked {asked:?}"
        );
    }
    // And the essential seats were, so this is not a case of only the extras
    // being polled.
    assert_eq!(asked.len(), 6, "every voting seat answers: {asked:?}");
}

#[tokio::test]
async fn two_seats_may_not_share_a_name() {
    // `press` addresses a seat by name, so a shared one made the first match win:
    // a press aimed at the second landed on the first, or reported that you cannot
    // press yourself when you meant somebody else.
    let (mut cfg, _) = council_with_custom_seats(1);
    cfg.delegates[4].label = "Advocate".into();
    let (rec, backend) = run(vec![], cfg).await;
    assert!(
        rec.failures().iter().any(|f| f.contains("two seats are called Advocate")),
        "a shared name should be refused: {:?}",
        rec.failures()
    );
    assert!(backend.seen.lock().expect("seen").is_empty());
}

#[tokio::test]
async fn a_press_names_the_council_back_so_the_model_can_correct_itself() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, engine, _backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1"})),
            // Wrong name, and no name at all — both used to say the same thing,
            // which taught nothing and spent a turn.
            Script::call("press", json!({"to": "Arbiter", "question": "on what basis?"})),
            Script::call("press", json!({"question": "and you?"})),
            Script::text("r"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let said: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ActionRejected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect();
    assert!(
        said.iter().any(|r| r.contains("the seats are: Advocate")),
        "a wrong name should come back with the roster: {said:?}"
    );
    assert!(
        said.iter().any(|r| r.contains("press needs `to`")),
        "a missing name should say what is missing: {said:?}"
    );
}

#[tokio::test]
async fn a_bad_configuration_is_refused_before_any_request() {
    let (rec, backend) = run(vec![], DebateConfig { motion: "  ".into(), ..config() }).await;
    assert!(rec.failures().iter().any(|f| f.contains("no motion")));
    assert!(backend.seen.lock().expect("seen").is_empty());
}

#[tokio::test]
async fn a_single_delegate_is_not_a_council() {
    let mut cfg = config();
    cfg.delegates.truncate(1);
    let (rec, _) = run(vec![], cfg).await;
    assert!(rec.failures().iter().any(|f| f.contains("at least two")));
}

#[tokio::test]
async fn the_system_prompt_is_shared_but_the_persona_is_not() {
    let mut cfg = config();
    cfg.delegates[0].persona = Some("you are a botanist".into());
    let (_, backend) = run(
        vec![Script::text("ok"), Script::text("ok"), Script::text("ok"), Script::text("ok")],
        cfg,
    )
    .await;
    let seen = backend.seen.lock().expect("seen");
    let advocate = seen
        .iter()
        .find(|r| r.model == "m-a")
        .expect("advocate request");
    let critic = seen.iter().find(|r| r.model == "m-b").expect("critic request");
    let sys = |r: &ChatRequest| r.messages[0].content.clone();
    assert!(sys(advocate).contains("you are a botanist"));
    assert!(!sys(critic).contains("you are a botanist"));
    for r in seen
        .iter()
        .filter(|r| r.tools.iter().any(|t| t["function"]["name"] == "vote"))
    {
        assert!(sys(r).contains("stalemate"), "procedure must be present");
    }
}

#[tokio::test]
async fn inline_mode_works_without_native_tool_calling() {
    let mut cfg = config();
    cfg.endpoints[0].native_tools = false;
    let (rec, backend) = run(
        vec![
            ChatResponse {
                content: "Opening.\n```tool\n{\"tool\":\"make_point\",\"args\":{\"text\":\"Roses win\"}}\n```"
                    .into(),
                ..Default::default()
            },
            Script::text("```tool\n{\"tool\":\"vote\",\"args\":{\"point\":1,\"verdict\":\"win\"}}\n```"),
            Script::text("```tool\n{\"tool\":\"vote\",\"args\":{\"point\":1,\"verdict\":\"lose\"}}\n```"),
            Script::text("```tool\n{\"tool\":\"vote\",\"args\":{\"point\":1,\"verdict\":\"win\"}}\n```"),
        ],
        cfg,
    )
    .await;
    assert_eq!(rec.final_board(), vec![(1, "carried", 1)]);
    let seen = backend.seen.lock().expect("seen");
    assert!(seen[0].tools.is_empty(), "no tools should be advertised");
}

#[tokio::test]
async fn a_model_with_no_tool_support_can_still_be_sniffed_for_an_answer() {
    let mut cfg = config();
    cfg.endpoints[0].native_tools = false;
    let (rec, _) = run(
        vec![
            Script::inline(r#"```tool
{"tool":"poll","args":{"question":"Roses or lilies?","options":["roses","lilies"]}}
```"#),
            ChatResponse { content: "I would have to say lilies, unhelpfully.".into(), ..Default::default() },
            ChatResponse { content: "lilies".into(), ..Default::default() },
            Script::text("undecided"),
        ],
        cfg,
    )
    .await;
    let events = rec.events.lock().expect("events");
    let outcome = events.iter().find_map(|e| match e {
        EngineEvent::PollClosed { outcome, .. } => Some(outcome.clone()),
        _ => None,
    });
    assert_eq!(outcome, Some(Some("lilies".to_string())));
}

#[tokio::test]
async fn tokens_reach_the_ui() {
    let (rec, _) = run(
        vec![
            ChatResponse { content: "Hello ".into(), ..Default::default() },
            Script::text(""),
            Script::text(""),
            Script::text(""),
        ],
        config(),
    )
    .await;
    let streamed: String = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Token { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(streamed.contains("Hello"), "got {streamed:?}");
}

// ---------------------------------------------------------------------------
// The event stream is the contract with the webview. If a field is renamed in
// Rust without updating src/types.ts, the UI silently stops working, so the
// shape is asserted here rather than discovered at runtime.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn events_serialise_to_the_shape_the_webview_expects() {
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            Script::call("poll", json!({"question": "Roses or lilies?", "options": ["roses", "lilies"]})),
            Script::call("poll_ballot", json!({"choice": "roses"})),
            Script::call("poll_ballot", json!({"choice": "roses"})),
            Script::text("agreed"),
            Script::text("agreed"),
        ],
        config(),
    )
    .await;

    let events = rec.events.lock().expect("events");
    let as_json: Vec<serde_json::Value> = events
        .iter()
        .map(|e| serde_json::to_value(e).expect("events serialise"))
        .collect();

    let kind = |v: &serde_json::Value| v["type"].as_str().unwrap_or_default().to_string();
    let names: Vec<String> = as_json.iter().map(kind).collect();
    for expected in [
        "debateStarted",
        "roundStarted",
        "turnStarted",
        "token",
        "speechFinished",
        "actionTaken",
        "pointUpdated",
        "pollOpened",
        "pollBallotCast",
        "pollClosed",
        "debateEnded",
    ] {
        assert!(names.contains(&expected.to_string()), "no {expected} event: {names:?}");
    }

    // No snake_case may leak through: the TypeScript types are camelCase.
    for v in &as_json {
        let keys: Vec<&str> = v.as_object().expect("objects").keys().map(String::as_str).collect();
        for k in &keys {
            assert!(!k.contains('_'), "field {k} is not camelCase in {v}");
        }
    }

    // Spot-check the shapes the UI reads most.
    let ballot = as_json.iter().find(|v| kind(v) == "pollBallotCast").expect("ballot");
    assert!(ballot["pollId"].is_number(), "{ballot}");
    assert!(ballot["delegate"].is_string());
    assert!(ballot["choice"].is_string());
    assert!(ballot["result"].is_array());

    let point = as_json.iter().find(|v| kind(v) == "pointUpdated").expect("point");
    assert!(point["tally"]["status"].is_string());
    assert!(point["tally"]["ratings"].is_object(), "the UI reads ratings directly");
    assert!(point["tally"]["contested"].is_boolean());
    assert!(point["point"]["id"].is_number());
    assert!(point["point"]["concededBy"].is_object());

    let closed = as_json.iter().find(|v| kind(v) == "pollClosed").expect("closed");
    assert!(closed["outcome"].is_string(), "{closed}");
    let ballots = closed["poll"]["ballots"].as_array().expect("ballots");
    assert!(!ballots.is_empty(), "the scripted seats should have voted");
    assert!(
        ballots.iter().all(|b| b.get("confidence").is_some()),
        "confidence rides on every ballot, got {ballots:?}"
    );

    let ended = as_json.iter().find(|v| kind(v) == "debateEnded").expect("ended");
    assert!(ended["verdict"]["carried"].is_array());
    assert!(ended["verdict"]["motion"].is_string());
    assert!(ended["verdict"]["endedHow"].is_string(), "{ended}");
    assert!(ended["verdict"]["adjournedBy"].is_string() || ended["verdict"]["adjournedBy"].is_null());
}

#[tokio::test]
async fn a_withdrawn_point_reports_no_score() {
    let p = Point {
        id: 1,
        text: "x".into(),
        raised_by: "a".into(),
        round: 1,
        withdrawn: true,
        withdrawn_reason: Some("wrong".into()),
        beats: vec![],
        votes: Default::default(),
        rebuttals: vec![],
        steelmen: vec![],
        evidence: vec![],
        conceded_by: Default::default(),
        review: None,
        ruling: None,
    };
    let roster = vec!["a".to_string(), "b".to_string()];
    let t = p.tally(&roster);
    assert_eq!(t.status, PointStatus::Withdrawn);
    assert_eq!(t.score, 0);
    assert!(t.ratings.is_empty(), "a withdrawn point has no ratings");
}

// ---------------------------------------------------------------------------
// Diagnostics. A connection error that says only "could not reach X" is
// useless at 1am, so the real cause has to survive the trip to the UI.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_refused_connection_says_why() {
    // Port 1 is reserved and nothing will be listening on it.
    let endpoint = EndpointConfig {
        base_url: "http://127.0.0.1:1/v1".into(),
        native_tools: false,
        ..Default::default()
    };
    let err = council_core::list_models(&endpoint)
        .await
        .expect_err("nothing is listening on port 1");
    let msg = err.to_string();
    assert!(msg.contains("127.0.0.1:1"), "{msg}");
    assert!(
        msg.to_lowercase().contains("refused"),
        "the cause chain should reach the io error, got: {msg}"
    );
    assert!(
        !msg.contains("error sending request for url"),
        "reqwest's useless summary should be filtered out, got: {msg}"
    );
}

#[tokio::test]
async fn a_wrong_port_reports_the_port_it_tried() {
    let endpoint = EndpointConfig {
        base_url: "http://127.0.0.1:1".into(),
        ..Default::default()
    };
    let urls = endpoint.models_urls();
    assert_eq!(
        urls,
        vec!["http://127.0.0.1:1/v1/models", "http://127.0.0.1:1/models"]
    );
}

#[tokio::test]
async fn a_lan_address_is_left_alone() {
    // The localhost rewrite must not touch an address the user typed on purpose.
    let ep = EndpointConfig {
        base_url: "http://192.168.0.41:8000".into(),
        ..Default::default()
    };
    assert_eq!(ep.base(), "http://192.168.0.41:8000");
    assert_eq!(
        ep.chat_completions_url(),
        "http://192.168.0.41:8000/v1/chat/completions"
    );
    assert_eq!(
        EndpointConfig {
            base_url: "http://192.168.0.41:8000/v1".into(),
            ..Default::default()
        }
        .models_urls(),
        vec!["http://192.168.0.41:8000/v1/models"]
    );
}

#[tokio::test]
async fn skipping_a_stuck_turn_moves_on_without_killing_the_debate() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Hung {
        entered: Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl ChatBackend for Hung {
        async fn chat(
            &self,
            _req: &ChatRequest,
            _on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
        ) -> Result<ChatResponse, LlmError> {
            self.entered.store(true, Ordering::SeqCst);
            // A request that never answers, like a model being swapped in.
            std::future::pending().await
        }
    }
    let rec = Arc::new(Recorder::default());
    let backend = Arc::new(Hung {
        entered: Arc::new(AtomicBool::new(false)),
    });
    let entered = backend.entered.clone();
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(backend, sink));

    let driver = {
        let engine = engine.clone();
        let mut cfg = config();
        cfg.max_rounds = 1;
        cfg.closing_statements = false;
        tokio::spawn(async move { engine.run(cfg).await })
    };
    // Wait for the first turn to go in flight, then skip it.
    for _ in 0..200 {
        if entered.load(Ordering::SeqCst) || engine.turn_in_flight() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(engine.skip_turn(), "a turn should be in flight to skip");
    // The second delegate's turn hangs too (same backend); skip it as well.
    for _ in 0..400 {
        if rec.kinds().iter().filter(|k| **k == "skipped").count() >= 2 {
            break;
        }
        if engine.turn_in_flight() {
            engine.skip_turn();
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    driver.abort();

    let kinds = rec.kinds();
    assert!(
        kinds.contains(&"skipped"),
        "a TurnSkipped event should have been emitted, got {kinds:?}"
    );
    let skipped: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::TurnSkipped { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect();
    assert!(!skipped.is_empty(), "someone should have been skipped");
}

#[tokio::test]
async fn thinking_reaches_the_ui_as_reasoning_not_speech() {
    let (rec, _) = run(
        vec![
            Script::thinking("Roses have thorns, so the gift angle is weak…"),
            Script::text(""),
            Script::text(""),
            Script::text(""),
            Script::text(""),
            Script::text(""),
        ],
        config(),
    )
    .await;
    let thought: String = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Reasoning { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(thought.contains("thorns"), "got {thought:?}");
    // The thinking is kept on the finished speech for reading after the fact.
    let kept: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::SpeechFinished { speech, .. } => Some(speech.reasoning.clone()),
            _ => None,
        })
        .collect();
    assert!(
        kept.iter().any(|r| r.contains("thorns")),
        "speech should keep its thinking, got {kept:?}"
    );
}

#[tokio::test]
async fn tool_blocks_hiding_in_the_thinking_still_count() {
    // Reasoning-first models put their tool calls in the thinking, not the
    // answer. The engine reads both before giving up on a turn.
    let lone_vote = r#"Roses are overrated.
```tool
{"tool":"vote","args":{"point":1,"verdict":"lose"}}
```"#;
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            Script::thinking(lone_vote),
            Script::thinking(lone_vote),
            Script::text(""),
            Script::text(""),
            Script::text(""),
        ],
        config(),
    )
    .await;
    assert_eq!(
        rec.final_board(),
        vec![(1, "fell", -1)],
        "the votes from the thinking should have landed"
    );
}

#[tokio::test]
async fn stop_aborts_a_hung_request_instead_of_waiting_out_the_timeout() {
    struct Hung;
    #[async_trait::async_trait]
    impl ChatBackend for Hung {
        async fn chat(
            &self,
            _req: &ChatRequest,
            _on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
        ) -> Result<ChatResponse, LlmError> {
            std::future::pending().await
        }
    }
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(Arc::new(Hung), sink));
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.closing_statements = false;
    let driver = {
        let engine = engine.clone();
        tokio::spawn(async move { engine.run(cfg).await })
    };
    for _ in 0..200 {
        if engine.turn_in_flight() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(engine.turn_in_flight(), "a turn should be in flight");
    engine.stop();
    let done = tokio::time::timeout(std::time::Duration::from_secs(10), driver)
        .await
        .expect("stop should end a hung debate promptly")
        .expect("driver joins");
    let _ = done;
    let kinds = rec.kinds();
    assert!(kinds.contains(&"ended"), "stopping should adjourn, got {kinds:?}");
    assert!(!kinds.contains(&"failed"), "stopping is not a failure, got {kinds:?}");
    let how = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => Some(verdict.ended_how),
            _ => None,
        })
        .collect::<Vec<_>>();
    use council_core::types::EndedHow;
    assert_eq!(how, vec![EndedHow::Stopped]);
}

#[tokio::test]
async fn an_empty_turn_is_logged_not_silent() {
    // A model that just stops used to leave a hole in the transcript. The
    // debate survives, but the hole is now labelled.
    let (rec, _) = run(
        vec![
            Script::text(""),
            Script::text(""),
            Script::text(""),
            Script::text(""),
            Script::text(""),
            Script::text(""),
        ],
        config(),
    )
    .await;
    let notes: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        notes.iter().any(|n| n.contains("empty turn")),
        "the empty turn should be named, got {notes:?}"
    );
    let kinds = rec.kinds();
    assert!(kinds.contains(&"ended"), "the debate should survive, got {kinds:?}");
}

#[tokio::test]
async fn hitting_the_token_limit_says_so() {
    let (rec, _) = run(
        vec![
            ChatResponse {
                content: "Roses are".into(),
                finish_reason: Some("length".into()),
                ..Default::default()
            },
            Script::text(""),
            Script::text(""),
            Script::text(""),
            Script::text(""),
            Script::text(""),
        ],
        config(),
    )
    .await;
    let notes: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        notes.iter().any(|n| n.contains("token limit")),
        "the cutoff should be named, got {notes:?}"
    );
}

#[tokio::test]
async fn a_stuttered_vote_counts_once() {
    // A model emitting the same call twice in one turn means it once. The
    // engine folds exact duplicates instead of double-counting.
    let twice = ChatResponse {
        content: String::new(),
        tool_calls: vec![
            RawToolCall {
                id: "x".into(),
                name: "vote".into(),
                arguments: json!({"point": 1, "verdict": "lose"}),
            },
            RawToolCall {
                id: "y".into(),
                name: "vote".into(),
                arguments: json!({"point": 1, "verdict": "lose"}),
            },
        ],
        ..Default::default()
    };
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            twice,
            Script::text(""),
            Script::text(""),
            Script::text(""),
            Script::text(""),
        ],
        config(),
    )
    .await;
    let votes = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter(|e| matches!(e, EngineEvent::ActionTaken { action, .. } if action == "vote"))
        .count();
    assert_eq!(votes, 1, "the duplicate should have been folded");
    assert_eq!(rec.final_board(), vec![(1, "stalemate", 0)]);
}

#[tokio::test]
async fn a_decision_model_answers_ballots_without_burning_turns() {
    use council_core::decide::{ClassifyRequest, Decision, DecisionBackend};
    struct Fixed;
    #[async_trait::async_trait]
    impl DecisionBackend for Fixed {
        async fn classify(
            &self,
            req: &ClassifyRequest,
        ) -> Result<Decision, LlmError> {
            assert!(req.question.contains("Roses or lilies"));
            assert!(!req.context.is_empty(), "the decider should see the board");
            Ok(Decision {
                choice: "roses".into(),
                confidence: Some(0.9),
            })
        }
    }
    // No ballot scripts at all: if the seats had to answer, these would come
    // back dry and the poll would close empty.
    let rec = Arc::new(Recorder::default());
    let backend = Arc::new(Script::new(vec![
        Script::call("poll", json!({"question": "Roses or lilies?", "options": ["roses", "lilies"]})),
        Script::text(""),
        Script::text(""),
        Script::text(""),
        Script::text(""),
        Script::text(""),
    ]));
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Engine::with_backend(backend, sink);
    engine.set_decider(Some(Arc::new(Fixed)));
    engine.run(config()).await;
    let closed = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::PollClosed { poll, .. } => Some(poll.clone()),
            _ => None,
        })
        .last()
        .expect("a poll should have closed");
    assert_eq!(closed.ballots.len(), 2, "both other seats should have ballots");
    assert!(closed.ballots.iter().all(|b| b.choice == "roses"));
    assert!(
        closed.ballots.iter().all(|b| b.confidence == Some(0.9)),
        "confidence should ride along: {closed:?}"
    );
}

#[tokio::test]
async fn a_dead_decision_model_falls_back_to_the_seats() {
    let mut cfg = config();
    cfg.decision = DecisionConfig {
        provider: DecisionProvider::Jev,
        base_url: "http://127.0.0.1:1".into(),
        model: "jev-1".into(),
        timeout_secs: 5,
        ..Default::default()
    };
    // Port 1 refuses immediately, so every ballot falls back to the scripted
    // seats below.
    let (rec, _) = run(
        vec![
            Script::call("poll", json!({"question": "Roses or lilies?", "options": ["roses", "lilies"]})),
            Script::call("poll_ballot", json!({"choice": "roses"})),
            Script::call("poll_ballot", json!({"choice": "lilies"})),
            Script::text(""),
            Script::text(""),
            Script::text(""),
        ],
        cfg,
    )
    .await;
    let notes: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        notes.iter().any(|n| n.contains("decision model failed")),
        "the fallback should be announced, got {notes:?}"
    );
    let kinds = rec.kinds();
    assert!(kinds.contains(&"pollClose"), "the poll should still close, got {kinds:?}");
}

#[tokio::test]
async fn a_decision_model_without_an_address_is_refused_up_front() {
    let mut cfg = config();
    cfg.decision = DecisionConfig {
        provider: DecisionProvider::Jev,
        ..Default::default()
    };
    let (rec, _) = run(vec![], cfg).await;
    assert!(
        rec.failures().iter().any(|f| f.contains("no URL")),
        "got {:?}",
        rec.failures()
    );
}

#[tokio::test]
async fn adjourn_over_a_stalemate_is_refused_until_reviewed() {
    // The mandate: a 1-1 deadlock cannot be buried by adjournment. The third
    // seat tries, is refused with the point named, and the debate runs out
    // its rounds instead.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::call("adjourn", json!({"reason": "we are done here"})),
        ],
        cfg,
    )
    .await;
    let logs: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::SpeechFinished { speech, .. } => Some(speech.actions.join("\n")),
            _ => None,
        })
        .collect();
    assert!(
        logs.iter()
            .any(|l| l.contains("(rejected adjourn") && l.contains("P1") && l.contains("review")),
        "the refusal should name the point and the remedy, got {logs:?}"
    );
    let adjourned = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => Some(verdict.adjourned_by.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(adjourned, vec![None], "refused adjournment must not stick");
}

#[tokio::test]
async fn a_review_rules_the_deadlock_and_counts_changed_minds() {
    // b voted lose, deadlocking P1 1-1. c opens a review; a rules it
    // decidable, b rules it decidable *and* flips to win in the same breath.
    // The review closes with a ruling and the point carries.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::call("review", json!({"point": 1})),
            Script::call(
                "stalemate_ballot",
                json!({"reading": "decidable", "reason": "the record settles it"}),
            ),
            Script::call(
                "stalemate_ballot",
                json!({"reading": "decidable", "reason": "on reflection it holds", "vote": "win"}),
            ),
        ],
        cfg,
    )
    .await;
    let kinds = rec.kinds();
    for k in ["reviewOpen", "reviewBallot", "reviewClose"] {
        assert!(kinds.contains(&k), "missing {k} in {kinds:?}");
    }
    let outcome = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ReviewClosed { outcome, .. } => Some(*outcome),
            _ => None,
        })
        .collect::<Vec<_>>();
    use council_core::types::StalemateReading;
    assert_eq!(outcome, vec![Some(StalemateReading::Decidable)]);
    assert_eq!(rec.final_board(), vec![(1, "carried", 2)]);
    let ruling = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => Some(
                verdict
                    .carried
                    .iter()
                    .map(|p| p.ruling)
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ruling, vec![vec![Some(StalemateReading::Decidable)]]);
}

#[tokio::test]
async fn review_events_serialise_to_the_shape_the_webview_expects() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::call("review", json!({"point": 1})),
            Script::call("stalemate_ballot", json!({"reading": "unsolvable"})),
            Script::call("stalemate_ballot", json!({"reading": "too_dense"})),
        ],
        cfg,
    )
    .await;
    let events = rec.events.lock().expect("events");
    let as_json: Vec<serde_json::Value> = events
        .iter()
        .map(|e| serde_json::to_value(e).expect("events serialise"))
        .collect();
    let kind = |v: &serde_json::Value| v["type"].as_str().unwrap_or_default().to_string();
    for expected in ["reviewOpened", "reviewBallotCast", "reviewClosed"] {
        assert!(
            as_json.iter().any(|v| kind(v) == expected),
            "no {expected} event"
        );
    }
    for v in as_json
        .iter()
        .filter(|v| kind(v).starts_with("review"))
    {
        let keys: Vec<&str> = v
            .as_object()
            .expect("objects")
            .keys()
            .map(String::as_str)
            .collect();
        for k in &keys {
            assert!(!k.contains('_'), "field {k} is not camelCase in {v}");
        }
    }
    let closed = as_json
        .iter()
        .find(|v| kind(v) == "reviewClosed")
        .expect("closed");
    // 1-1 split: no majority, honestly inconclusive.
    assert!(closed["outcome"].is_null(), "{closed}");
    let ballot = as_json
        .iter()
        .find(|v| kind(v) == "reviewBallotCast")
        .expect("ballot");
    assert!(ballot["result"].is_array());
    assert!(ballot["reading"].is_string());
}

#[tokio::test]
async fn each_round_closes_with_a_summary_from_the_summarizer() {
    // The intermission belongs to the summarizer seat alone: after every
    // completed round it lays the round out, and the arguing seats never do.
    let mut cfg = config();
    cfg.max_rounds = 2;
    let (rec, _) = run(
        vec![
            Script::text("a speaks"),
            Script::text("b speaks"),
            Script::text("c speaks"),
            Script::text("round one: nothing settled"),
            Script::text("a again"),
            Script::text("b again"),
            Script::text("c again"),
            Script::text("round two: still nothing settled"),
            Script::text("the end result"),
        ],
        cfg,
    )
    .await;
    let summaries: Vec<(String, u32, String)> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::SpeechFinished { speech, .. }
                if speech.actions.iter().any(|a| a.contains("summary")) =>
            {
                Some((
                    speech.delegate.clone(),
                    speech.round,
                    speech.actions.join(","),
                ))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        summaries,
        vec![
            ("s".to_string(), 1, "round 1 summary".to_string()),
            ("s".to_string(), 2, "round 2 summary".to_string()),
        ],
        "every round should close with the summarizer's summary"
    );
    assert!(
        rec.kinds().iter().filter(|k| ***k == *"intermission").count() == 2,
        "one intermission per completed round"
    );
}

#[tokio::test]
async fn a_poll_interrupts_the_turn_it_is_called_in() {
    // The ballot round trip runs inside the opener's turn: opened, answered,
    // closed, and only then is the opener's speech finished.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::call("poll", json!({"question": "Roses or lilies?", "options": ["roses", "lilies"]})),
            Script::call("poll_ballot", json!({"choice": "roses"})),
            Script::call("poll_ballot", json!({"choice": "lilies"})),
            Script::text(""),
            Script::text(""),
        ],
        cfg,
    )
    .await;
    let events = rec.events.lock().expect("events");
    let pos = |pred: &dyn Fn(&EngineEvent) -> bool| {
        events.iter().position(pred).expect("event should exist")
    };
    let opened = pos(&|e| matches!(e, EngineEvent::PollOpened { .. }));
    let ballot = pos(&|e| matches!(e, EngineEvent::PollBallotCast { .. }));
    let closed = pos(&|e| matches!(e, EngineEvent::PollClosed { .. }));
    let speech = pos(&|e| {
        matches!(e, EngineEvent::SpeechFinished { turn: 1, .. })
    });
    assert!(
        opened < ballot && ballot < closed && closed < speech,
        "poll must open, fill, close, and only then finish the opener's turn"
    );
}

#[tokio::test]
async fn a_voted_down_adjournment_keeps_the_debate_open() {
    // The motion fails 1–2, the debate runs out its round, and nobody is
    // recorded as having adjourned anything.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("adjourn", json!({"reason": "nothing left to say"})),
            Script::call("adjourn_ballot", json!({"confirm": false})),
            Script::call("adjourn_ballot", json!({"confirm": "no"})),
        ],
        cfg,
    )
    .await;
    let kinds = rec.kinds();
    assert!(kinds.contains(&"adjournDecided"), "got {kinds:?}");
    let notes: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        notes.iter().any(|n| n.contains("voted down")),
        "the failure should be announced, got {notes:?}"
    );
    let by = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => Some(verdict.adjourned_by.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(by, vec![None]);
}

#[tokio::test]
async fn a_failed_motion_may_be_moved_again_next_round() {
    // b's round-1 motion fails; c tries to re-move in the same round and is
    // refused; a moves again in round 2 and it carries unanimously.
    let mut cfg = config();
    cfg.max_rounds = 2;
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            Script::call("adjourn", json!({"reason": "short debate"})),
            Script::call("adjourn_ballot", json!({"confirm": false})),
            Script::call("adjourn_ballot", json!({"confirm": false})),
            Script::call("adjourn", json!({"reason": "me too"})),
            // Consumed by the intermission between rounds.
            Script::text("round one: no adjournment"),
            Script::call("adjourn", json!({"reason": "round two, truly done"})),
            Script::call("adjourn_ballot", json!({"confirm": true})),
            Script::call("adjourn_ballot", json!({"confirm": true})),
        ],
        cfg,
    )
    .await;
    let logs: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::SpeechFinished { speech, .. } => Some(speech.actions.join("\n")),
            _ => None,
        })
        .collect();
    assert!(
        logs.iter()
            .any(|l| l.contains("(rejected adjourn") && l.contains("voted down")),
        "the same-round re-motion should name the cooldown, got {logs:?}"
    );
    let by = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => Some(verdict.adjourned_by.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(by, vec![Some("a".to_string())]);
    let how = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => Some(verdict.ended_how),
            _ => None,
        })
        .collect::<Vec<_>>();
    use council_core::types::EndedHow;
    assert_eq!(how, vec![EndedHow::Adjourned]);
}

#[tokio::test]
async fn adjourn_events_serialise_to_the_shape_the_webview_expects() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::call("adjourn", json!({"reason": "done"})),
            Script::call("adjourn_ballot", json!({"confirm": true})),
            Script::call("adjourn_ballot", json!({"confirm": true})),
        ],
        cfg,
    )
    .await;
    let events = rec.events.lock().expect("events");
    let as_json: Vec<serde_json::Value> = events
        .iter()
        .map(|e| serde_json::to_value(e).expect("events serialise"))
        .collect();
    let kind = |v: &serde_json::Value| v["type"].as_str().unwrap_or_default().to_string();
    for expected in ["adjournMoved", "adjournBallotCast", "adjournDecided"] {
        assert!(as_json.iter().any(|v| kind(v) == expected), "no {expected}");
    }
    for v in as_json.iter().filter(|v| kind(v).starts_with("adjourn")) {
        let keys: Vec<&str> = v
            .as_object()
            .expect("objects")
            .keys()
            .map(String::as_str)
            .collect();
        for k in &keys {
            assert!(!k.contains('_'), "field {k} is not camelCase in {v}");
        }
    }
    let decided = as_json
        .iter()
        .find(|v| kind(v) == "adjournDecided")
        .expect("decided");
    assert_eq!(decided["passed"], serde_json::json!(true));
    assert_eq!(decided["yes"], serde_json::json!(3));
}

#[tokio::test]
async fn auto_adjourn_settles_early_and_says_so() {
    // Every point decided in round 1 of 3: the debate ends early and names
    // the settlement instead of masquerading as ten full rounds.
    let mut cfg = config();
    cfg.max_rounds = 3;
    cfg.auto_adjourn = true;
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
        ],
        cfg,
    )
    .await;
    let verdict = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => Some(verdict.clone()),
            _ => None,
        })
        .last()
        .expect("a verdict");
    use council_core::types::EndedHow;
    assert_eq!(verdict.rounds, 1);
    assert_eq!(verdict.ended_how, EndedHow::Settled);
    // The board settles on the second vote, but the round still runs to its
    // end: cutting the third seat off mid-round left the transcript with no
    // account of what had happened, and no summary of the round either.
    assert_eq!(
        rec.final_board(),
        vec![(1, "carried", 3)],
        "every seat should have been heard before the early stop"
    );
    let events = rec.events.lock().expect("events");
    assert!(
        events.iter().any(|e| matches!(e, EngineEvent::IntermissionStarted { round: 1 })),
        "the round the debate ended on must still be laid out"
    );
}

#[tokio::test]
async fn a_turn_of_announced_intentions_is_labelled_inaction() {
    // Prose promising future deeds, zero tool calls: the transcript should
    // say so rather than showing a bare speech.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::text("I will now steelman everything before weighing in."),
            Script::text(""),
            Script::text(""),
        ],
        cfg,
    )
    .await;
    let logs: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::SpeechFinished { speech, .. } => Some(speech.actions.join("\n")),
            _ => None,
        })
        .collect();
    assert!(
        logs.iter().any(|l| l.contains("(no actions taken this turn)")),
        "the idle turn should be labelled, got {logs:?}"
    );
}

#[tokio::test]
async fn a_refused_tool_call_is_not_reported_as_inaction() {
    // A delegate that reached for a tool and was refused has acted, badly. It
    // must not be labelled as having sat silent, which is what happened: the
    // parser-refusal path emitted its notice but wrote no line to the action
    // log, and an empty log is exactly what the caller reads as "did nothing".
    // So the turn showed "acting on the board", explained the refusal, and then
    // claimed no action was taken.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            // No such tool, and arguments that cannot be parsed into an action.
            Script::call("not_a_real_tool", json!({"nonsense": true})),
            Script::text(""),
            Script::text(""),
        ],
        cfg,
    )
    .await;
    let logs: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::SpeechFinished { speech, .. } => Some(speech.actions.join("\n")),
            _ => None,
        })
        .collect();
    assert!(
        !logs.iter().any(|l| l.contains("(no actions taken this turn)")),
        "a refused call is not inaction, yet it was labelled as such: {logs:?}"
    );
    assert!(
        logs.iter().any(|l| l.contains("(refused")),
        "the refusal should be on the record: {logs:?}"
    );
}

#[tokio::test]
async fn a_repeated_call_is_not_reported_as_inaction() {
    // Same tool, same arguments, twice. The second is folded — a stuttering
    // model means it once — but the delegate did act, so folding must not empty
    // the log and get the turn labelled as inaction.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::call(
                "make_point",
                json!({"text": "Roses outrank lilies as a gift flower"}),
            ),
            Script::call(
                "make_point",
                json!({"text": "Roses outrank lilies as a gift flower"}),
            ),
            Script::text(""),
            Script::text(""),
        ],
        cfg,
    )
    .await;
    let logs: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::SpeechFinished { speech, .. } => Some(speech.actions.join("\n")),
            _ => None,
        })
        .collect();
    assert!(
        !logs.iter().any(|l| l.contains("(no actions taken this turn)")),
        "a repeated call still means the delegate acted: {logs:?}"
    );
}

#[tokio::test]
async fn convening_without_the_essentials_is_refused() {
    // No summarizer at all: the debate must not start.
    let mut cfg = config();
    cfg.delegates.retain(|d| d.seat_role != SeatRole::Summarizer);
    let (rec, backend) = run(vec![], cfg).await;
    assert!(
        rec.failures().iter().any(|f| f.contains("summarizer")),
        "missing summarizer should be named, got {:?}",
        rec.failures()
    );
    assert!(backend.seen.lock().expect("seen").is_empty());

    // Two proponents: exactly one of each essential may sit.
    let mut cfg = config();
    cfg.delegates[3].seat_role = SeatRole::Proponent;
    let (rec, _) = run(vec![], cfg).await;
    assert!(
        rec.failures().iter().any(|f| f.contains("only one")),
        "duplicate essential should be refused, got {:?}",
        rec.failures()
    );
}

#[tokio::test]
async fn seating_two_rationales_is_refused() {
    // The Rationale is optional, but permanent furniture means at most one.
    let mut cfg = config();
    for (id, label) in [("r1", "Rationale"), ("r2", "Second Opinion")] {
        cfg.delegates.push(DelegateConfig {
            id: id.into(),
            label: label.into(),
            model: "m-r".into(),
            endpoint_id: "local".into(),
            color: "#c9b8f2".into(),
            seat_role: SeatRole::Rationale,
            ..Default::default()
        });
    }
    let (rec, backend) = run(vec![], cfg).await;
    assert!(
        rec.failures().iter().any(|f| f.contains("only one rationale")),
        "duplicate rationale should be refused, got {:?}",
        rec.failures()
    );
    assert!(backend.seen.lock().expect("seen").is_empty());
}

#[tokio::test]
async fn the_rationale_argues_and_votes_after_the_arbiter() {
    // The fifth seat: speaks in seat order after the judge, votes like any
    // arguing seat, and is never offered the Arbiter's instrument.
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.delegates.insert(
        3,
        DelegateConfig {
            id: "r".into(),
            label: "Rationale".into(),
            model: "m-r".into(),
            endpoint_id: "local".into(),
            color: "#c9b8f2".into(),
            seat_role: SeatRole::Rationale,
            ..Default::default()
        },
    );
    let (rec, backend) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses last longer than lilies"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    assert!(rec.failures().is_empty(), "{:?}", rec.failures());
    let order: Vec<String> = {
        let events = rec.events.lock().expect("events");
        events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::TurnStarted { delegate, .. } => Some(delegate.clone()),
                _ => None,
            })
            .collect()
    };
    // The arguing seats speak first; the summarizer then takes two
    // non-arguing turns, the round intermission and the conclusion.
    assert_eq!(order, vec!["a", "b", "c", "r", "s", "s"], "got {order:?}");
    // The Rationale's vote counted: a's implicit win, b win, c lose, r win.
    assert_eq!(rec.final_board(), vec![(1, "carried", 2)]);
    let belts: Vec<Vec<String>> = backend
        .seen
        .lock()
        .expect("seen")
        .iter()
        .filter(|r| r.model == "m-r")
        .map(|r| {
            r.tools
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect();
    assert!(!belts.is_empty(), "the rationale was never asked for anything");
    assert!(
        belts.iter().all(|b| b.contains(&"vote".to_string())),
        "the rationale must be able to vote"
    );
    assert!(
        belts.iter().all(|b| !b.contains(&"rule".to_string())),
        "the rationale must not be offered `rule`"
    );
}

#[tokio::test]
async fn attached_images_reach_the_opening_request_but_not_the_record_bytes() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.images = vec![ImageAttachment {
        id: "im1".into(),
        name: "chart.png".into(),
        media_type: "image/png".into(),
        data_url: "data:image/png;base64,AAA".into(),
    }];
    let (rec, backend) = run(
        vec![
            Script::call("make_point", json!({"text": "The chart trends up"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    assert!(rec.failures().is_empty(), "{:?}", rec.failures());
    // The UI learns what was attached, without the bytes.
    let started = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .find_map(|e| match e {
            EngineEvent::DebateStarted { images, .. } => Some(images.clone()),
            _ => None,
        })
        .expect("debate started");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].name, "chart.png");
    // The opening request carries the image as a content part, named in prose.
    let seen = backend.seen.lock().expect("seen");
    let first = seen.iter().find(|r| r.model == "m-a").expect("advocate asked");
    let opening = first
        .messages
        .iter()
        .find(|m| m.role == "user" && m.content.contains("MOTION"))
        .expect("opening message");
    assert_eq!(
        opening.images,
        vec!["data:image/png;base64,AAA".to_string()]
    );
    assert!(
        opening.content.contains("Image 1 (chart.png)"),
        "the seat must be told what it is looking at: {}",
        opening.content
    );
}

/// A backend that 400s any request asking for usage, like a server that does
/// not know `stream_options`. Records whether each request asked.
#[derive(Default)]
struct RejectUsage {
    seen: Mutex<Vec<bool>>,
}

#[async_trait]
impl ChatBackend for RejectUsage {
    async fn chat(
        &self,
        req: &ChatRequest,
        _on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
    ) -> Result<ChatResponse, LlmError> {
        self.seen.lock().expect("seen").push(req.endpoint.include_usage);
        if req.endpoint.include_usage {
            return Err(LlmError::Status {
                url: "http://127.0.0.1:8000/v1/chat/completions".into(),
                status: 400,
                body: "unrecognized field".into(),
            });
        }
        Ok(ChatResponse {
            content: "fine".into(),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn a_usage_rejection_is_remembered_for_the_session() {
    // The retry saves the turn, but without memory every turn on such a
    // server would pay a 400 plus a retry — and a notice — forever.
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.endpoints[0].stream = true;
    cfg.endpoints[0].include_usage = true;
    let rec = Arc::new(Recorder::default());
    let backend = Arc::new(RejectUsage::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Engine::with_backend(backend.clone(), sink);
    engine.run(cfg).await;
    assert!(rec.failures().is_empty(), "{:?}", rec.failures());
    let seen = backend.seen.lock().expect("seen");
    assert!(!seen.is_empty(), "nothing was ever asked");
    assert!(seen[0], "the first request should ask for usage");
    assert!(
        seen.iter().skip(1).all(|asked| !asked),
        "after one refusal nothing should ask again, got {seen:?}"
    );
}

#[tokio::test]
async fn the_summarizer_takes_no_round_turns_and_casts_no_ballots() {
    // The summarizer speaks only between rounds and at the end. Its requests
    // carry its one instrument (or none), never the arguing toolbelt.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, backend) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            Script::call(
                "poll",
                json!({"question": "Roses or lilies?", "options": ["roses", "lilies"]}),
            ),
            Script::call("poll_ballot", json!({"choice": "roses"})),
            Script::call("poll_ballot", json!({"choice": "roses"})),
            Script::text("round one laid out"),
            Script::text("the end result"),
        ],
        cfg,
    )
    .await;
    assert!(rec.failures().is_empty(), "{:?}", rec.failures());
    let seen = backend.seen.lock().expect("seen");
    for r in seen.iter().filter(|r| r.model == "m-s") {
        assert!(
            !r.tools.iter().any(|t| t["function"]["name"] == "vote"),
            "the summarizer must never be offered the arguing toolbelt"
        );
    }
    let events = rec.events.lock().expect("events");
    // Two poll ballots: the summarizer never answered the poll.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, EngineEvent::PollBallotCast { .. }))
            .count(),
        2
    );
    // Every summarizer speech is a summary or the final result.
    for s in events.iter().filter_map(|e| match e {
        EngineEvent::SpeechFinished { speech, .. } if speech.delegate == "s" => Some(speech),
        _ => None,
    }) {
        assert!(
            s.actions.iter().any(|a| a.contains("summary") || a == "final result"),
            "summarizer spoke out of role: {s:?}"
        );
    }
}

#[tokio::test]
async fn a_speaker_cannot_propose_an_extension_mid_turn() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 2;
    let (rec, _) = run(
        vec![
            Script::call("propose_extension", json!({"rounds": 2, "reason": "more time"})),
            Script::text("b speaks"),
            Script::text("c speaks"),
            Script::text("round one laid out"),
            Script::text("the end result"),
        ],
        cfg,
    )
    .await;
    let events = rec.events.lock().expect("events");
    let rejected = events.iter().any(
        |e| matches!(e, EngineEvent::ActionRejected { tool, reason, .. }
            if tool == "propose_extension" && reason.contains("Summarizer")),
    );
    assert!(rejected, "a mid-turn proposal should be refused");
    // kinds() re-locks the event log: release the guard first.
    drop(events);
    assert!(
        !rec.kinds().contains(&"extProposed"),
        "no proposal should ever open mid-turn"
    );
}

/// Run the engine in the background so the test can answer an extension
/// proposal mid-debate, the way the UI will through `resolve_extension`.
async fn run_background(
    script: Vec<ChatResponse>,
    cfg: DebateConfig,
) -> (
    Arc<Recorder>,
    Arc<Engine<Script>>,
    Arc<Script>,
    tokio::task::JoinHandle<()>,
) {
    let rec = Arc::new(Recorder::default());
    let backend = Arc::new(Script::new(script));
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(backend.clone(), sink));
    let handle = tokio::spawn({
        let engine = engine.clone();
        async move {
            engine.run(cfg).await;
        }
    });
    (rec, engine, backend, handle)
}

/// A backend that takes its time, so an operator has a real window to answer.
///
/// The scripted backend replies instantly, which is right for every other test
/// and useless here: the council now votes on a request for more rounds, and
/// against an instant reply there is no moment in which a human could have
/// spoken. This is what makes the veto testable at all.
struct Slow {
    inner: Arc<Script>,
    delay_ms: u64,
}

#[async_trait]
impl ChatBackend for Slow {
    async fn chat(
        &self,
        req: &ChatRequest,
        on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
    ) -> Result<ChatResponse, LlmError> {
        tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        self.inner.chat(req, on_chunk).await
    }
}

/// As `run_background`, but every reply takes `delay_ms`.
async fn run_background_slow(
    script: Vec<ChatResponse>,
    cfg: DebateConfig,
    delay_ms: u64,
) -> (
    Arc<Recorder>,
    Arc<Engine<Slow>>,
    Arc<Script>,
    tokio::task::JoinHandle<()>,
) {
    let rec = Arc::new(Recorder::default());
    let inner = Arc::new(Script::new(script));
    let backend = Arc::new(Slow {
        inner: inner.clone(),
        delay_ms,
    });
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(backend, sink));
    let handle = tokio::spawn({
        let engine = engine.clone();
        async move {
            engine.run(cfg).await;
        }
    });
    (rec, engine, inner, handle)
}

async fn wait_for(rec: &Recorder, kind: &str) {
    for _ in 0..500 {
        if rec.kinds().contains(&kind) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {kind}");
}

fn intermission_proposal(rounds: u32) -> ChatResponse {
    ChatResponse {
        content: "Round one is laid out above.".into(),
        tool_calls: vec![RawToolCall {
            id: "x".into(),
            name: "propose_extension".into(),
            arguments: json!({"rounds": rounds, "reason": "the crux is undecided"}),
        }],
        ..Default::default()
    }
}

#[tokio::test]
async fn an_approved_extension_grows_the_debate() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 3;
    let (rec, engine, _script, handle) = run_background_slow(
        vec![
            Script::text("a speaks"),
            Script::corrective(),
            Script::text("b speaks"),
            Script::corrective(),
            Script::text("c speaks"),
            Script::corrective(),
            intermission_proposal(2),
            // Enough room for the operator to answer before the room does.
            Script::call("extension_ballot", json!({"grant": false, "reason": "we have said it"})),
            Script::call("extension_ballot", json!({"grant": false, "reason": "we have said it"})),
            Script::call("extension_ballot", json!({"grant": false, "reason": "we have said it"})),
        ],
        cfg,
        400,
    )
    .await;
    wait_for(&rec, "extProposed").await;
    assert!(
        engine.resolve_extension(true, 2).is_ok(),
        "an operator should be able to settle an open proposal"
    );
    handle.await.expect("engine task");
    let kinds = rec.kinds();
    assert!(kinds.contains(&"extGranted"), "grant should be announced");
    let totals: Vec<(u32, u32, u32)> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ExtensionGranted {
                added,
                total,
                budget_left,
            } => Some((*added, *total, *budget_left)),
            _ => None,
        })
        .collect();
    assert_eq!(totals, vec![(2, 3, 1)]);
    let rounds: Vec<u32> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => Some(verdict.rounds),
            _ => None,
        })
        .collect();
    assert_eq!(rounds, vec![3], "1 planned + 2 granted rounds are spoken");
}

#[tokio::test]
async fn a_council_that_declines_the_extension_summarizes_as_usual() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 3;
    let (rec, _engine, _script, handle) = run_background(
        vec![
            Script::text("a speaks"),
            Script::corrective(),
            Script::text("b speaks"),
            Script::corrective(),
            Script::text("c speaks"),
            Script::corrective(),
            intermission_proposal(2),
            // Every seat refuses: the argument is going in circles.
            Script::call("extension_ballot", json!({"grant": false, "reason": "repeating"})),
            Script::call("extension_ballot", json!({"grant": false, "reason": "repeating"})),
            Script::call("extension_ballot", json!({"grant": false, "reason": "repeating"})),
            Script::text("the end result"),
        ],
        cfg,
    )
    .await;
    wait_for(&rec, "extProposed").await;
    handle.await.expect("engine task");
    let kinds = rec.kinds();
    assert!(kinds.contains(&"extDeclined"));
    assert!(!kinds.contains(&"extGranted"));
    let events = rec.events.lock().expect("events");
    // The round was still laid out despite the refusal.
    assert!(events.iter().any(|e| matches!(e,
        EngineEvent::SpeechFinished { speech, .. }
        if speech.delegate == "s" && speech.actions.iter().any(|a| a == "round 1 summary"))));
    let rounds: Vec<u32> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => Some(verdict.rounds),
            _ => None,
        })
        .collect();
    assert_eq!(rounds, vec![1]);
}

#[tokio::test]
async fn an_exhausted_extension_budget_refuses_the_proposal() {
    // The default test config budgets zero extra rounds: the proposal is
    // rejected and the debate continues as planned.
    let mut cfg = config();
    cfg.max_rounds = 1;
    assert_eq!(cfg.max_extra_rounds, 0);
    let (rec, _) = run(
        vec![
            Script::text("a speaks"),
            Script::corrective(),
            Script::text("b speaks"),
            Script::corrective(),
            Script::text("c speaks"),
            Script::corrective(),
            intermission_proposal(2),
            Script::text("the end result"),
        ],
        cfg,
    )
    .await;
    let events = rec.events.lock().expect("events");
    assert!(events.iter().any(|e| matches!(e,
        EngineEvent::ActionRejected { tool, reason, .. }
        if tool == "propose_extension" && reason.contains("exhausted"))));
    drop(events);
    assert!(!rec.kinds().contains(&"extProposed"));
}

#[tokio::test]
async fn a_second_proposal_in_one_intermission_is_rejected() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 3;
    let (rec, _engine, _script, handle) = run_background(
        vec![
            Script::text("a speaks"),
            Script::corrective(),
            Script::text("b speaks"),
            Script::corrective(),
            Script::text("c speaks"),
            Script::corrective(),
            ChatResponse {
                content: "Round one is laid out above.".into(),
                tool_calls: vec![
                    RawToolCall {
                        id: "x".into(),
                        name: "propose_extension".into(),
                        arguments: json!({"rounds": 1, "reason": "first"}),
                    },
                    RawToolCall {
                        id: "y".into(),
                        name: "propose_extension".into(),
                        arguments: json!({"rounds": 1, "reason": "second"}),
                    },
                ],
                ..Default::default()
            },
            Script::call("extension_ballot", json!({"grant": false, "reason": "one is enough"})),
            Script::call("extension_ballot", json!({"grant": false, "reason": "one is enough"})),
            Script::text("the end result"),
        ],
        cfg,
    )
    .await;
    wait_for(&rec, "extProposed").await;
    handle.await.expect("engine task");
    let events = rec.events.lock().expect("events");
    assert!(
        events.iter().any(|e| matches!(e,
            EngineEvent::ActionRejected { tool, reason, .. }
            if tool == "propose_extension" && reason.contains("one extension proposal"))),
        "the second proposal should be refused"
    );
    // One proposal opened, one declined — the debate ran its single round.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, EngineEvent::ExtensionProposed { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn extension_rounds_are_clamped_to_the_budget() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 1;
    let (rec, engine, _script, handle) = run_background_slow(
        vec![
            Script::text("a speaks"),
            Script::corrective(),
            Script::text("b speaks"),
            Script::corrective(),
            Script::text("c speaks"),
            Script::corrective(),
            intermission_proposal(5),
        ],
        cfg,
        400,
    )
    .await;
    wait_for(&rec, "extProposed").await;
    let proposed: Vec<u32> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ExtensionProposed { rounds, .. } => Some(*rounds),
            _ => None,
        })
        .collect();
    assert_eq!(proposed, vec![1], "asking 5 of a budget of 1 clamps to 1");
    assert!(engine.resolve_extension(true, 1).is_ok());
    handle.await.expect("engine task");
    let totals: Vec<(u32, u32, u32)> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ExtensionGranted {
                added,
                total,
                budget_left,
            } => Some((*added, *total, *budget_left)),
            _ => None,
        })
        .collect();
    assert_eq!(totals, vec![(1, 2, 0)]);
}

#[tokio::test]
async fn resolving_with_nothing_waiting_is_an_error() {
    let engine = Engine::with_backend(
        Arc::new(Script::new(vec![])),
        Arc::new(Recorder::default()),
    );
    assert!(
        engine
            .resolve_extension(true, 1)
            .unwrap_err()
            .contains("no extension proposal"),
        "answering thin air should fail loudly"
    );
}

#[tokio::test]
async fn the_conclusion_lands_on_the_verdict() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::text("a speaks"),
            Script::corrective(),
            Script::text("b speaks"),
            Script::corrective(),
            Script::text("c speaks"),
            Script::corrective(),
            Script::text("round one laid out"),
            Script::text("Roses carry the day on longevity."),
        ],
        cfg,
    )
    .await;
    let finals: Vec<Option<String>> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => Some(verdict.final_summary.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        finals,
        vec![Some("Roses carry the day on longevity.".to_string())]
    );
}

#[tokio::test]
async fn a_turn_of_announced_intentions_gets_one_corrective_reprompt() {
    // The model says it will act but calls nothing. The engine re-prompts
    // once; this time the model acts, and the action lands on the board.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::text("I will raise a point about roses."),
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            Script::text("b speaks"),
            Script::corrective(),
            Script::text("c speaks"),
            Script::corrective(),
            Script::text("round one laid out"),
            Script::text("the end result"),
        ],
        cfg,
    )
    .await;
    assert!(rec.failures().is_empty(), "{:?}", rec.failures());
    // The corrective re-prompt produced the point. The proponent implicitly
    // rates it win, so it carries with a score of 1.
    assert_eq!(rec.final_board(), vec![(1, "carried", 1)]);
    let actions = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ActionTaken { action, .. } => Some(action.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(actions.contains(&"make_point".to_string()), "{actions:?}");
}

#[tokio::test]
async fn a_corrective_reprompt_that_still_does_nothing_is_labelled() {
    // The model announces intent, gets corrected, and still does not act.
    // The turn is labelled as inaction rather than silently passing.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::text("I will vote on the point."),
            Script::corrective(),
            Script::text("b speaks"),
            Script::corrective(),
            Script::text("c speaks"),
            Script::corrective(),
            Script::text("round one laid out"),
            Script::text("the end result"),
        ],
        cfg,
    )
    .await;
    let logs: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::SpeechFinished { speech, .. } => Some(speech.actions.join("\n")),
            _ => None,
        })
        .collect();
    assert!(
        logs.iter().any(|l| l.contains("(no actions taken this turn)")),
        "the twice-idle turn should be labelled, got {logs:?}"
    );
}

#[tokio::test]
async fn the_same_claim_is_not_raised_twice() {
    // A model that re-raises a claim next round used to put it on the board
    // twice, and the tally counted it twice. Punctuation and case carry no
    // meaning on the board, so neither does a rephrasing of one claim.
    let mut cfg = config();
    cfg.max_rounds = 2;
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            Script::text("b"),
            Script::text("c"),
            Script::text("round one laid out"),
            Script::call("make_point", json!({"text": "Roses outrank lilies"})),
            Script::text("d"),
            Script::text("e"),
            Script::text("round two laid out"),
            Script::text("the end result"),
        ],
        cfg,
    )
    .await;
    assert_eq!(
        rec.final_board().len(),
        1,
        "the repeat should be refused, not raised as P2"
    );
    let rejected = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .any(|e| matches!(e, EngineEvent::ActionRejected { tool, reason, .. }
            if tool == "make_point" && reason.contains("already raised")));
    assert!(rejected, "the refusal should say why: {rejected}");
}

#[tokio::test]
async fn one_reply_cannot_put_the_same_point_on_the_board_twice() {
    // The same call written twice in one reply — once fenced, once bare, and
    // once repeated in the thinking that a reasoning-first model prepends.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            ChatResponse {
                reasoning: "I should call {\"tool\":\"make_point\",\"args\":{\"text\":\"Roses win\"}}"
                    .into(),
                content: "```tool\n{\"tool\":\"make_point\",\"args\":{\"text\":\"Roses win\"}}\n```\n\
                          {\"tool\":\"make_point\",\"args\":{\"text\":\"Roses win\"}}".into(),
                ..Default::default()
            },
            Script::text("b"),
            Script::text("c"),
            Script::text("round one laid out"),
            Script::text("the end result"),
        ],
        cfg,
    )
    .await;
    assert_eq!(
        rec.final_board().len(),
        1,
        "three copies of one claim must yield one point"
    );
}

#[tokio::test]
async fn a_truncated_end_result_says_so_rather_than_looking_conclusive() {
    // The end result is read as finished prose in the sidebar, so a server cut
    // mid-sentence must announce itself instead of reading as a conclusion.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::text("a speaks"),
            Script::corrective(),
            Script::text("b speaks"),
            Script::corrective(),
            Script::text("c speaks"),
            Script::corrective(),
            Script::text("round one laid out"),
            ChatResponse {
                content: "The council carried P2 and then P4 (operational integrity — is the".into(),
                finish_reason: Some("length".into()),
                ..Default::default()
            },
        ],
        cfg,
    )
    .await;
    let warned = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .any(|e| matches!(e, EngineEvent::Notice { text, tone: NoticeTone::Warn, .. }
            if text.contains("end result") && text.contains("token limit")));
    assert!(
        warned,
        "a cut-off end result must warn: it would otherwise read as a conclusion"
    );
}

#[tokio::test]
async fn a_full_adjournment_round_trip_records_every_ballot() {
    // The failure this pins: a reasoning model given a tight token budget emits
    // its ballot truncated mid-string, it stops parsing, and the seat is
    // counted as having abstained — quietly moving the majority.
    let mut cfg = config();
    cfg.max_rounds = 3;
    let (rec, backend) = run(
        vec![
            Script::call("adjourn", json!({"reason": "the record is settled"})),
            // The mover counts as yes; these two are every other voting seat.
            // The second answers "1" as a string, the shape a real model emits.
            Script::call("adjourn_ballot", json!({"confirm": true, "reason": "settled"})),
            Script::call("adjourn_ballot", json!({"confirm": "1", "reason": "nothing live"})),
            Script::text("unreachable"),
        ],
        cfg,
    )
    .await;
    let decided = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::AdjournDecided { passed, yes, no, .. } => Some((*passed, *yes, *no)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decided, vec![(true, 3, 0)], "every seat's ballot should land");
    // The ballots must also be uncapped, or a reasoning model loses them.
    for req in backend.seen.lock().expect("seen").iter() {
        if req.tools.iter().any(|t| t["function"]["name"] == "adjourn_ballot") {
            assert_eq!(
                req.max_tokens, None,
                "ballots must not be capped: a cut reply stops parsing"
            );
        }
    }
}

/// A settled board: one point, raised and voted through, so the council did
/// reach an answer.
fn settled_config() -> DebateConfig {
    config()
}

fn conclude_call(agreed: bool, answer: &str, lean: Option<&str>) -> ChatResponse {
    let mut args = json!({ "agreed": agreed, "answer": answer });
    if let Some(l) = lean {
        args["lean"] = json!(l);
    }
    ChatResponse {
        content: String::new(),
        tool_calls: vec![RawToolCall {
            id: "c".into(),
            name: "conclude".into(),
            arguments: args,
        }],
        ..Default::default()
    }
}

#[tokio::test]
async fn the_answer_section_reports_an_agreement_the_board_reached() {
    let mut cfg = settled_config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "A hotdog is a sandwich"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::text("round one laid out"),
            conclude_call(true, "Yes — structurally, a hotdog is a sandwich.", None),
        ],
        cfg,
    )
    .await;
    let c = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => verdict.conclusion.clone(),
            _ => None,
        })
        .next()
        .expect("a conclusion");
    assert!(c.agreed, "the board carried P1 unanimously enough: {c:?}");
    assert_eq!(c.answer, "Yes — structurally, a hotdog is a sandwich.");
    assert!(c.tally.contains("1 carried"), "{}", c.tally);
}

#[tokio::test]
async fn a_stalemate_reports_no_agreement_but_still_shows_the_lean() {
    // The council deadlocked, so there is no answer to give — but the lean is
    // still worth showing, which is the whole point of the section.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "A hotdog is not a sandwich"})),
            // One win (the proponent's implicit) against one loss: a tie, and a
            // contested one, so the board is deadlocked rather than settled.
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::text("the Judge declines to rate it"),
            // Consumed by that turn's corrective re-prompt.
            Script::corrective(),
            Script::text("round one laid out"),
            conclude_call(false, "", Some("toward calling it a sandwich")),
        ],
        cfg,
    )
    .await;
    let c = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => verdict.conclusion.clone(),
            _ => None,
        })
        .next()
        .expect("a conclusion");
    assert!(!c.agreed);
    assert!(c.answer.is_empty(), "a deadlock has no answer to give");
    assert_eq!(c.lean.as_deref(), Some("toward calling it a sandwich"));
    assert!(c.tally.contains("deadlocked"), "{}", c.tally);
}

#[tokio::test]
async fn a_seat_cannot_declare_an_agreement_the_board_never_reached() {
    // The summarizer claims consensus while the board is deadlocked. The
    // engine keeps its own verdict: the answer is dropped and the claim
    // reported, because prose is not what settles a debate here.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "A hotdog is not a sandwich"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::text("the Judge declines to rate it"),
            // Consumed by that turn's corrective re-prompt.
            Script::corrective(),
            Script::text("round one laid out"),
            conclude_call(true, "The council agreed it is a sandwich.", None),
        ],
        cfg,
    )
    .await;
    let events = rec.events.lock().expect("events");
    let c = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => verdict.conclusion.clone(),
            _ => None,
        })
        .next()
        .expect("a conclusion");
    assert!(!c.agreed, "the board deadlocked, so this cannot be agreement");
    assert!(
        c.answer.is_empty(),
        "a refused answer must not be shown as the result"
    );
    assert!(events.iter().any(|e| matches!(e, EngineEvent::Notice { text, .. }
        if text.contains("claimed agreement"))), "the override must be reported");
}

#[tokio::test]
async fn a_summarizer_that_never_answers_still_leaves_an_honest_section() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _) = run(
        vec![
            Script::call("make_point", json!({"text": "A hotdog is a sandwich"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::text("round one laid out"),
            Script::text("In summary, the debate covered the motion at length."),
        ],
        cfg,
    )
    .await;
    let c = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => verdict.conclusion.clone(),
            _ => None,
        })
        .next()
        .expect("a conclusion");
    assert!(!c.agreed);
    assert!(c.answer.is_empty());
    assert!(c.lean.is_none());
}

#[tokio::test]
async fn the_summarizer_keeps_both_instruments_in_their_own_phase() {
    // Two phases, two toolbelts: propose_extension between rounds, conclude
    // once at the end. Neither may leak into the other, and neither may be
    // lost — the extension is what lets the summarizer ask for more time.
    let tool_names = |tools: &[serde_json::Value]| -> Vec<String> {
        tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap_or_default().to_string())
            .collect()
    };
    assert_eq!(
        tool_names(&council_core::tools::summarizer_tools()),
        // Between rounds the Summarizer can end the debate or ask to extend it,
        // and nothing else — `conclude` belongs to the end, not the intermission.
        vec!["settle".to_string(), "propose_extension".to_string()],
        "the intermission belt is the seat's own two instruments"
    );
    assert_eq!(
        tool_names(&council_core::tools::conclude_tool()),
        vec!["conclude".to_string()],
        "the closing belt is the answer, and only that"
    );

    // And in the engine, end to end: the proposal is offered on the
    // intermission request, and the answer is not.
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 2;
    let (rec, engine, backend, handle) = run_background_slow(
        vec![
            Script::text("a speaks"),
            Script::corrective(),
            Script::text("b speaks"),
            Script::corrective(),
            Script::text("c speaks"),
            Script::corrective(),
            intermission_proposal(1),
            Script::text("the end result"),
        ],
        cfg,
        400,
    )
    .await;
    wait_for(&rec, "extProposed").await;
    assert!(engine.resolve_extension(false, 0).is_ok());
    handle.await.expect("engine task");

    assert!(
        rec.events.lock().expect("events").iter().any(
            |e| matches!(e, EngineEvent::ExtensionProposed { rounds: 1, .. })
        ),
        "the summarizer should still be able to ask for another round"
    );
    let offered_conclude_mid_debate = {
        let seen = backend.seen.lock().expect("seen");
        seen.iter().any(|r| {
            r.tools.iter().any(|t| t["function"]["name"] == "propose_extension")
                && r.tools.iter().any(|t| t["function"]["name"] == "conclude")
        })
    };
    assert!(
        !offered_conclude_mid_debate,
        "the answer must not be offered mid-debate"
    );
}

/// A board that deadlocked: one implicit proponent win against one loss, and a
/// third seat that never rated it.
/// `n` seats answering a request for more rounds.
fn extension_ballots(grant: bool, n: usize, reason: &str) -> Vec<ChatResponse> {
    (0..n)
        .map(|_| Script::call("extension_ballot", json!({"grant": grant, "reason": reason})))
        .collect()
}

fn deadlocked_script(extra: Vec<ChatResponse>) -> Vec<ChatResponse> {
    let mut v = vec![
        Script::call("make_point", json!({"text": "A hotdog is not a sandwich"})),
        Script::call("vote", json!({"point": 1, "verdict": "lose"})),
        Script::text("the Judge declines to rate it"),
        Script::corrective(),
        Script::text("round one laid out: deadlocked"),
    ];
    v.extend(extra);
    v
}

#[tokio::test]
async fn the_conclusion_offers_no_extension_when_the_board_was_not_stalemated() {
    // The instrument is the one thing that must not appear on a settled board:
    // offering a tool that is guaranteed to be refused only invites the model
    // to spend a turn arguing for it.
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 3;
    let (rec, backend) = run(
        vec![
            Script::call("make_point", json!({"text": "A hotdog is a sandwich"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::text("round one laid out"),
            conclude_call(true, "Yes.", None),
        ],
        cfg,
    )
    .await;
    let final_req = backend
        .seen
        .lock()
        .expect("seen")
        .last()
        .cloned()
        .expect("a final request");
    assert!(
        !final_req
            .tools
            .iter()
            .any(|t| t["function"]["name"] == "propose_extension"),
        "no deadlock, so no extension offered: {:?}",
        final_req.tools
    );
    assert!(!rec.kinds().contains(&"extProposed"));
}

#[tokio::test]
async fn a_deadlocked_board_lets_the_summarizer_ask_and_the_council_grant_it() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 3;
    let (rec, _engine, _script, handle) = run_background(
        {
            let mut v = vec![ChatResponse {
                content: String::new(),
                tool_calls: vec![RawToolCall {
                    id: "c".into(),
                    name: "propose_extension".into(),
                    arguments: json!({"rounds": 2, "reason": "nobody engaged the structural claim"}),
                }],
                ..Default::default()
            }];
            // Two of three is a majority, so the request settles without the
            // operator touching anything.
            v.extend(extension_ballots(true, 2, "the structural claim is untouched"));
            // Two granted rounds have to be argued, or the loop runs out of
            // script before the board has actually debated anything.
            // Three seat turns and an intermission per granted round: a, b
            // and c in seat order, the two who say nothing taking a corrective,
            // then the Summarizer laying the round out.
            for turn in 0..2 {
                v.push(Script::call("vote", json!({"point": 1, "verdict": "lose"})));
                v.push(Script::text("nothing new"));
                v.push(Script::corrective());
                v.push(Script::text("nothing new"));
                v.push(Script::corrective());
                v.push(Script::text(&format!("round {} laid out", 2 + turn)));
            }
            deadlocked_script(v)
        },
        cfg,
    )
    .await;
    wait_for(&rec, "extProposed").await;
    handle.await.expect("engine task");
    assert!(rec.kinds().contains(&"extGranted"));

    // The granted rounds are actually spoken, and the debate concludes again.
    let events = rec.events.lock().expect("events");
    let rounds: Vec<u32> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::RoundStarted { round } => Some(*round),
            _ => None,
        })
        .collect();
    assert!(
        rounds.len() >= 2,
        "a grant of two rounds must produce more debate, got {rounds:?}"
    );
}

#[tokio::test]
async fn a_declined_end_of_debate_request_concludes_with_no_agreement() {
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 3;
    let (rec, _engine, _script, handle) = run_background(
        deadlocked_script(vec![
            ChatResponse {
                content: String::new(),
                tool_calls: vec![RawToolCall {
                    id: "c".into(),
                    name: "propose_extension".into(),
                    arguments: json!({"rounds": 2, "reason": "worth another pass"}),
                }],
                ..Default::default()
            },
            Script::call("extension_ballot", json!({"grant": false, "reason": "we have said it"})),
            Script::call("extension_ballot", json!({"grant": false, "reason": "we have said it"})),
            Script::text("the end result"),
        ]),
        cfg,
    )
    .await;
    wait_for(&rec, "extProposed").await;
    handle.await.expect("engine task");

    let rounds: Vec<u32> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::RoundStarted { round } => Some(*round),
            _ => None,
        })
        .collect();
    assert_eq!(rounds, vec![1], "a refusal ends it there");
    let c = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => verdict.conclusion.clone(),
            _ => None,
        })
        .next()
        .expect("a conclusion");
    assert!(!c.agreed, "the board never agreed");
}

#[tokio::test]
async fn the_end_of_debate_extension_is_offered_only_once() {
    // Otherwise a summarizer that keeps finding new reasons could reopen the
    // debate until the whole budget is gone.
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 9;
    let proposal = || ChatResponse {
        content: String::new(),
        tool_calls: vec![RawToolCall {
            id: "c".into(),
            name: "propose_extension".into(),
            arguments: json!({"rounds": 2, "reason": "the crux is untouched"}),
        }],
        ..Default::default()
    };
    // Silent seats keep the script short: an empty reply is labelled and does
    // not spend a corrective re-prompt, so one entry is one seat's turn.
    let silent_round = || vec![Script::text(""), Script::text(""), Script::text(""), Script::text("")];

    let mut script = deadlocked_script(vec![proposal()]);
    // The room answers the first request; two of three is a majority.
    script.extend(extension_ballots(true, 2, "the crux is untouched"));
    script.extend(silent_round()); // round 2, granted
    script.extend(silent_round()); // round 3
    script.push(proposal()); // the second conclusion, asking again
    script.push(Script::text("the end result"));

    let (rec, engine, _script, handle) = run_background_slow(script, cfg, 400).await;
    wait_for(&rec, "extProposed").await;
    assert!(engine.resolve_extension(true, 2).is_ok());
    handle.await.expect("engine task");

    let asked = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter(|e| matches!(e, EngineEvent::ExtensionProposed { .. }))
        .count();
    assert_eq!(
        asked, 1,
        "the post-closing request is a single chance per debate"
    );
    // And it did resume, so the guard is not simply blocking it.
    let rounds: Vec<u32> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::RoundStarted { round } => Some(*round),
            _ => None,
        })
        .collect();
    assert_eq!(rounds, vec![1, 2, 3], "the grant bought real rounds");
}

#[tokio::test]
async fn an_inline_request_is_acted_on_without_being_displayed() {
    // What the operator saw: the request granted, and the raw JSON sitting in
    // the transcript under the summary. The call and the speech have to be
    // separated — the engine does the deed and shows only the sentence.
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 2;
    let inline = "The board is stuck on the crux, so I am asking for more time.\n\
                  ```tool\n{\"tool\":\"propose_extension\",\"args\":{\"rounds\":2,\"reason\":\"P3 unengaged\"}}\n```";
    let (rec, engine, _script, handle) = run_background_slow(
        vec![
            Script::call("make_point", json!({"text": "P3 unengaged"})),
            Script::text("no comment"),
            Script::text("no comment"),
            Script::corrective(),
            Script::text("round one laid out"),
            ChatResponse {
                content: inline.into(),
                ..Default::default()
            },
        ],
        cfg,
        400,
    )
    .await;
    wait_for(&rec, "extProposed").await;
    assert!(engine.resolve_extension(true, 2).is_ok());
    handle.await.expect("engine task");

    let speeches: Vec<(u32, String)> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::SpeechFinished { speech, .. } if speech.delegate == "s" => {
                Some((speech.round, speech.text.clone()))
            }
            _ => None,
        })
        .collect();
    let said = speeches
        .iter()
        .find(|(r, _)| *r == 1)
        .map(|(_, t)| t.clone())
        .expect("the intermission speech");
    assert!(
        said.contains("asking for more time"),
        "the prose should survive: {said}"
    );
    assert!(
        !said.contains("propose_extension") && !said.contains("```") && !said.contains("args"),
        "the request JSON must not be rendered as speech: {said}"
    );
}

/// The payload that failed to parse in a real run. It is perfectly good JSON,
/// and `parse_action` accepts it in isolation — the failure was that it was
/// not the *first* tool call in the reply.
const REPORTED_ADJOURN_BALLOT: &str = r#"{"tool":"adjourn_ballot","args":{"confirm":true,"reason":"The board has settled the motion on its own terms: P1 (pro-Socrates efficiency claim) is defeated by P4, which carries at +2; P2 fell; P3 confirms falsifiability but doesn't alter the outcome. The verdict — Plato was better than Socrates on the metric of producing sustained positive doctrine versus elenchus yielding only aporia — follows from the record. No live substantive disagreement remains within the framework the council actually built."}}"#;

#[tokio::test]
async fn a_ballot_is_found_even_when_a_vote_comes_first() {
    // The seat voted, then answered. Reading only the first call threw the
    // ballot away and recorded the seat as not having answered — which on an
    // adjournment quietly moves the majority, so this is not a cosmetic miss.
    let mut cfg = config();
    cfg.max_rounds = 3;
    let reply = format!(
        "Two positions before my answer.\n\
         {{\"tool\":\"vote\",\"args\":{{\"point\":1,\"verdict\":\"win\",\"reason\":\"holds\"}}}}\n\
         {REPORTED_ADJOURN_BALLOT}"
    );
    let (rec, _backend) = run(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("adjourn", json!({"reason": "the record is settled"})),
            Script::inline(&reply),
            Script::call("adjourn_ballot", json!({"confirm": true, "reason": "nothing live"})),
            Script::text("unreachable"),
        ],
        cfg,
    )
    .await;
    let decided: Vec<(bool, u32, u32)> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::AdjournDecided { passed, yes, no, .. } => Some((*passed, *yes, *no)),
            _ => None,
        })
        .collect();
    assert_eq!(
        decided,
        vec![(true, 3, 0)],
        "the ballot was dropped because a vote preceded it"
    );
    // And the vote the seat bundled with it is still a vote.
    let votes: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ActionTaken { delegate, detail, .. } if detail.contains(" voted ") && detail.contains("P1") => {
                Some(format!("{delegate}: {detail}"))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        votes,
        vec!["a: Advocate voted win on P1 — holds".to_string()],
        "a vote cast alongside a ballot must not be lost"
    );
}

#[tokio::test]
async fn the_arbiter_rates_every_point_it_engages_but_only_once_each() {
    // A one-vote-per-turn cap looked like restraint and was not: an Arbiter
    // engaging four points has to rate four points, and refusing the rest pushed
    // those judgements off the record rather than making them count for less.
    // What needed limiting was rating the same point twice in one breath.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let two_votes_and_a_ruling =
        "Both points are settled.\n\
         {\"tool\":\"vote\",\"args\":{\"point\":1,\"verdict\":\"win\"}}\n\
         {\"tool\":\"vote\",\"args\":{\"point\":2,\"verdict\":\"lose\",\"reason\":\"refuted\"}}\n\
         {\"tool\":\"vote\",\"args\":{\"point\":2,\"verdict\":\"win\",\"reason\":\"on reflection\"}}\n\
         {\"tool\":\"rule\",\"args\":{\"point\":2,\"ruling\":\"falls\",\"reason\":\"the argument is refuted in the record\"}}";
    let (rec, _engine, backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("make_point", json!({"text": "P2 claim"})),
            Script::inline(two_votes_and_a_ruling),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");

    let events = rec.events.lock().expect("events");
    let votes: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ActionTaken { delegate, detail, .. } if detail.contains(" voted ") => {
                Some(format!("{delegate}: {detail}"))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        votes.len(),
        2,
        "the Arbiter should rate both points it engaged: {votes:?}"
    );
    assert!(votes[0].contains("P1"), "{votes:?}");
    assert!(votes[1].contains("P2"), "{votes:?}");

    let refused = events
        .iter()
        .any(|e| matches!(e, EngineEvent::ActionRejected { tool, reason, .. }
            if tool == "vote" && reason.contains("already rated P2 this turn")
                && reason.contains("`rule`")));
    assert!(
        refused,
        "rating the same point twice in one turn should be refused, pointing at `rule`"
    );

    // And the ruling it reached instead settles the point outright.
    let ruled = events.iter().any(|e| matches!(e,
        EngineEvent::ActionTaken { delegate, detail, .. }
        if delegate == "c" && detail.contains("ruled P2 falls")));
    assert!(ruled, "the Arbiter should have ruled P2 down");

    let p2 = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::PointUpdated { point, tally, .. } if point.id == 2 => {
                Some(tally.status.clone())
            }
            _ => None,
        })
        .last()
        .expect("a tally for P2");
    assert_eq!(
        p2,
        PointStatus::Fell,
        "the ruling settles the point whatever the tally says"
    );

    // And only the Arbiter is offered the instrument.
    let belts: Vec<Vec<String>> = backend
        .seen
        .lock()
        .expect("seen")
        .iter()
        .map(|r| {
            r.tools
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect();
    let with_rule = belts.iter().filter(|b| b.contains(&"rule".to_string())).count();
    assert!(
        with_rule > 0,
        "the Arbiter must be offered `rule`"
    );
    assert_eq!(
        with_rule,
        1,
        "only one seat may hold a tiebreak: {belts:?}"
    );
}

#[tokio::test]
async fn a_ruling_overturns_a_tally_that_says_otherwise() {
    // The point the Arbiter is for: the counting has settled something the
    // record cannot. The tally is left alone, so the board shows exactly what
    // happened — the Arbiter overrode the count rather than rewriting it.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _engine, _s, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            // Two seats against, one for: the point has fallen on the count.
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::inline(
                "The count is not the answer here.\n\
                 {\"tool\":\"rule\",\"args\":{\"point\":1,\"ruling\":\"carries\",\"reason\":\"the objection was answered\"}}",
            ),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    let tally = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::PointUpdated { tally, .. } => Some(tally.clone()),
            _ => None,
        })
        .last()
        .expect("a tally");
    // One seat against, the raiser's implicit win for: a deadlocked point.
    // The tally must still read that way — a ruling that rewrote the count
    // would hide the fact that it overrode it.
    assert_eq!(
        tally.score, 0,
        "the tally must still read as it counted"
    );
    assert!(
        tally.wins + tally.losses + tally.stalemates + tally.abstentions > 0,
        "somebody rated it"
    );
    assert_eq!(
        tally.status,
        PointStatus::Carried,
        "and yet the point stands, because the Arbiter ruled it does"
    );
}


// ---------------------------------------------------------------------------
// the reference shelf
// ---------------------------------------------------------------------------

/// A shelf that answers from a script, so no test ever touches a network.
///
/// It answers in real MediaWiki JSON rather than in already-parsed text, so the
/// parsers are exercised rather than bypassed: a change that broke
/// `parse_extract` would break these tests.
struct StubWeb {
    pages: std::sync::Mutex<std::collections::HashMap<String, String>>,
    hits: std::sync::Mutex<Vec<(String, String)>>,
    asked: std::sync::Mutex<Vec<String>>,
    fail: std::sync::Mutex<Option<String>>,
}

impl StubWeb {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            pages: std::sync::Mutex::new(Default::default()),
            hits: std::sync::Mutex::new(Vec::new()),
            asked: std::sync::Mutex::new(Vec::new()),
            fail: std::sync::Mutex::new(None),
        })
    }
    /// Give the shelf an article.
    fn page(self: &Arc<Self>, title: &str, text: &str) -> Arc<Self> {
        self.pages
            .lock()
            .expect("pages")
            .insert(title.to_ascii_lowercase(), text.to_string());
        self.clone()
    }
    /// Give the shelf some search hits.
    fn search(self: &Arc<Self>, hits: &[(&str, &str)]) -> Arc<Self> {
        *self.hits.lock().expect("hits") = hits
            .iter()
            .map(|(t, sn)| (t.to_string(), sn.to_string()))
            .collect();
        self.clone()
    }
    /// Make every request fail, to stand in for a shelf that is not there.
    fn failing(self: &Arc<Self>, needle: &str) -> Arc<Self> {
        *self.fail.lock().expect("fail") = Some(needle.to_string());
        self.clone()
    }
    fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("asked").clone()
    }
}

#[async_trait]
impl council_core::web::WebBackend for StubWeb {
    async fn get(&self, url: &str) -> Result<String, council_core::web::WebError> {
        self.asked.lock().expect("asked").push(url.to_string());
        let failure = self.fail.lock().expect("fail").clone();
        if let Some(f) = failure {
            if url.contains(f.as_str()) {
                return Err(council_core::web::WebError::Fetch(format!(
                    "{f} is unreachable"
                )));
            }
        }
        let page = |title: &str, extract: &str| {
            json!({"batchcomplete": true, "query": {"pages": [
                {"pageid": 1, "ns": 0, "title": title, "extract": extract}
            ]}}).to_string()
        };
        if url.contains("list=search") {
            let hits: Vec<_> = self
                .hits
                .lock()
                .expect("hits")
                .iter()
                .map(|(t, s)| json!({"title": t, "snippet": s}))
                .collect();
            return Ok(json!({"query": {"search": hits}}).to_string());
        }
        if url.contains("duckduckgo") {
            return Ok(json!({"Definition": "", "AbstractText": "a definition"}).to_string());
        }
        if url.contains("prop=extracts") {
            let raw = url.split("titles=").nth(1).unwrap_or_default();
            let title = percent_decode(raw);
            let key = title.to_ascii_lowercase();
            let found = self.pages.lock().expect("pages").get(&key).cloned();
            if let Some(text) = found {
                return Ok(page(&title, &text));
            }
            return Ok(json!({"query": {"pages": [
                {"ns": 0, "title": title, "missing": true}
            ]}}).to_string());
        }
        Err(council_core::web::WebError::Fetch(format!("no stub for {url}")))
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"),
                16,
            ) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A debate with a shelf attached, so nothing reaches a network.
async fn run_with_web(
    script: Vec<ChatResponse>,
    cfg: DebateConfig,
    web: Arc<StubWeb>,
) -> (Arc<Recorder>, Arc<Script>, Arc<StubWeb>) {
    let rec = Arc::new(Recorder::default());
    let backend = Arc::new(Script::new(script));
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Engine::with_backend(backend.clone(), sink);
    engine.set_web(web.clone());
    engine.run(cfg).await;
    (rec, backend, web)
}

fn web_config() -> DebateConfig {
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.allow_web = true;
    cfg
}

#[tokio::test]
async fn a_seat_reads_a_page_and_argues_from_it_in_the_same_turn() {
    let web = StubWeb::new().page(
        "tautology (logic)",
        "In mathematical logic, a tautology is a formula true regardless of the \
         interpretation of its component terms.",
    );
    let (rec, backend, _web) = run_with_web(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("web_fetch", json!({"title": "Tautology (logic)"})),
            // What the seat says once it has read.
            ChatResponse {
                content: "The definition settles it.".into(),
                tool_calls: vec![RawToolCall {
                    id: "x".into(),
                    name: "vote".into(),
                    arguments: json!({"point": 1, "verdict": "win"}),
                }],
                ..Default::default()
            },
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::text("round laid out"),
        ],
        web_config(),
        web,
    )
    .await;

    // The page reached the model, fenced as data.
    let saw_fence = backend.seen.lock().expect("seen").iter().any(|r| {
        r.messages
            .iter()
            .any(|m| m.content.contains("<untrusted_data"))
    });
    assert!(saw_fence, "the seat was never shown what it read");

    // And the seat acted on it rather than only fetching.
    let acted = rec.events.lock().expect("events").iter().any(|e| {
        matches!(e, EngineEvent::ActionTaken { delegate, .. } if delegate == "b")
    });
    assert!(acted, "a read must be followed by an action");

    // The operator can see the same page.
    let got = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .find_map(|e| match e {
            EngineEvent::WebFetched { title, text, source, .. } => {
                Some((title.clone(), text.clone(), source.clone()))
            }
            _ => None,
        })
        .expect("a page read event");
    assert_eq!(got.0, "Tautology (logic)");
    assert_eq!(got.2, "wikipedia");
    assert!(got.1.contains("true regardless"), "{}", got.1);
}

#[tokio::test]
async fn a_seat_reads_what_a_search_found_and_acts_on_it() {
    let web = StubWeb::new()
        .search(&[("Epistemic injustice", "unfair distinctions in authority")])
        .page(
            "epistemic injustice",
            "Epistemic injustice is unfairness in epistemic authority.",
        );
    let (rec, backend, web) = run_with_web(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("web_search", json!({"query": "epistemic injustice"})),
            ChatResponse {
                content: "I have what I need.".into(),
                tool_calls: vec![RawToolCall {
                    id: "x".into(),
                    name: "vote".into(),
                    arguments: json!({"point": 1, "verdict": "win"}),
                }],
                ..Default::default()
            },
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::text("round laid out"),
        ],
        web_config(),
        web.clone(),
    )
    .await;

    assert!(
        web.asked().iter().any(|u| u.contains("list=search")),
        "the search never reached the shelf"
    );
    let saw_search = backend.seen.lock().expect("seen").iter().any(|r| {
        r.messages
            .iter()
            .any(|m| m.content.contains("Epistemic injustice"))
    });
    assert!(saw_search, "the search results never reached the model");
    let _ = rec;
}

#[tokio::test]
async fn the_turn_reads_only_as_many_pages_as_its_budget_allows() {
    let web = StubWeb::new()
        .page("alpha", "alpha text")
        .page("beta", "beta text")
        .page("gamma", "gamma text")
        .page("delta", "delta text")
        .page("epsilon", "epsilon text");
    let five: Vec<ChatResponse> = ["alpha", "beta", "gamma", "delta", "epsilon"]
        .iter()
        .map(|t| Script::call("web_fetch", json!({"title": t})))
        .collect();
    let mut script = vec![Script::call("make_point", json!({"text": "P1 claim"}))];
    script.extend(five);
    script.push(Script::text("over budget"));
    script.push(Script::call("vote", json!({"point": 1, "verdict": "lose"})));
    script.push(Script::text("round laid out"));

    let mut cfg = web_config();
    cfg.max_fetches_per_turn = 2;
    let (_rec, _backend, web) = run_with_web(script, cfg, web.clone()).await;

    let reads = web.asked().len();
    assert_eq!(reads, 2, "the turn read {reads} pages, not its budget of 2");
}

#[tokio::test]
async fn a_page_that_is_not_there_is_reported_rather_than_silently_dropped() {
    let web = StubWeb::new();
    let (rec, backend, _web) = run_with_web(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("web_fetch", json!({"title": "Nothing Called This"})),
            ChatResponse {
                content: "No such page, so I argue from the record.".into(),
                tool_calls: vec![RawToolCall {
                    id: "x".into(),
                    name: "vote".into(),
                    arguments: json!({"point": 1, "verdict": "win"}),
                }],
                ..Default::default()
            },
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::text("round laid out"),
        ],
        web_config(),
        web,
    )
    .await;
    let told = backend.seen.lock().expect("seen").iter().any(|r| {
        r.messages
            .iter()
            .any(|m| m.content.contains("NOT FOUND"))
    });
    assert!(told, "a failed read must be reported to the seat");
    let acted = rec.events.lock().expect("events").iter().any(|e| {
        matches!(e, EngineEvent::ActionTaken { delegate, .. } if delegate == "b")
    });
    assert!(acted, "a failed read must not end the turn");
}

#[tokio::test]
async fn only_the_seats_that_argue_may_read() {
    // The arbiter is handed the tool by a model that invents it anyway.
    let web = StubWeb::new().page("alpha", "alpha text");
    let mut cfg = web_config();
    cfg.auto_adjourn = false;
    let (rec, _backend, web) = run_with_web(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("web_fetch", json!({"title": "alpha"})),
            Script::text("nobody objects"),
            Script::text("round laid out"),
        ],
        cfg,
        web.clone(),
    )
    .await;
    assert!(
        web.asked().is_empty(),
        "a seat that may not read must not reach the shelf"
    );
    let refused = rec.events.lock().expect("events").iter().any(|e| {
        matches!(e, EngineEvent::ActionRejected { tool, .. } if tool == "web_fetch")
    });
    assert!(refused, "an invented reading tool should be refused by name");
}

#[tokio::test]
async fn web_access_is_off_until_it_is_switched_on() {
    let web = StubWeb::new().page("alpha", "alpha text");
    let mut cfg = web_config();
    cfg.allow_web = false;
    let (rec, _backend, web) = run_with_web(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("web_fetch", json!({"title": "alpha"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::text("round laid out"),
        ],
        cfg,
        web.clone(),
    )
    .await;
    assert!(web.asked().is_empty(), "nothing should be fetched");
    assert!(
        !rec.events.lock().expect("events").iter().any(|e| matches!(
            e,
            EngineEvent::WebFetched { .. }
        )),
        "no page should be reported"
    );
}

#[tokio::test]
async fn a_shelf_that_is_not_there_does_not_sink_the_turn() {
    // No TLS, no network, DNS down — a debate must survive all three.
    let web = StubWeb::new().failing("wikimedia");
    let (rec, backend, _web) = run_with_web(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("web_fetch", json!({"title": "Tautology"})),
            ChatResponse {
                content: "No reference work, so I argue from the record.".into(),
                tool_calls: vec![RawToolCall {
                    id: "x".into(),
                    name: "vote".into(),
                    arguments: json!({"point": 1, "verdict": "win"}),
                }],
                ..Default::default()
            },
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::text("round laid out"),
        ],
        web_config(),
        web,
    )
    .await;
    let told = backend.seen.lock().expect("seen").iter().any(|r| {
        r.messages
            .iter()
            .any(|m| m.content.contains("NOT FOUND"))
    });
    assert!(told, "the seat must be told the lookup failed");
    let acted = rec.events.lock().expect("events").iter().any(|e| {
        matches!(e, EngineEvent::ActionTaken { delegate, .. } if delegate == "b")
    });
    assert!(acted, "the seat should still have spoken and voted");
}

#[tokio::test]
async fn a_closing_address_is_prose_even_when_the_model_calls_a_tool() {
    // A closing is the last speech of the debate, and it is not a turn on the
    // board: no tool is advertised and the prompt says not to use one. Models
    // answer with `adjourn_ballot` anyway. Rendering that JSON as the closing
    // address put a line of code, with a scrollbar, where the council's last
    // words should have been.
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.closing_statements = true;
    let inline = "The record is settled on its own terms.\n\
                  ```tool\n{\"tool\":\"adjourn_ballot\",\"args\":{\"confirm\":true,\"reason\":\"P1 fell and P2 carries, so nothing is live.\"}}\n```\n\
                  So I record my agreement with that.";
    let (rec, _backend) = run(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::text("round laid out"),
            Script::inline(inline),
            Script::inline(inline),
            Script::inline(inline),
            Script::text("the answer is that we cannot tell"),
            Script::call("conclude", json!({"agreed": false, "answer": "undetermined", "lean": "neither"})),
        ],
        cfg,
    )
    .await;

    let closings: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::SpeechFinished { speech, .. }
                if speech.actions.iter().any(|a| a == "closing address") =>
            {
                Some(speech.text.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(closings.len(), 3, "every seat should have closed: {closings:?}");
    for text in &closings {
        assert!(
            !text.contains("adjourn_ballot") && !text.contains("```") && !text.contains("args"),
            "a tool call was rendered as the closing address: {text}"
        );
        assert!(
            text.contains("record is settled") && text.contains("agreement with that"),
            "the prose around the call should have survived: {text}"
        );
    }
    // And the refusal is on the record rather than being a silent drop.
    let told = rec.events.lock().expect("events").iter().any(|e| {
        matches!(e, EngineEvent::Notice { text, .. } if text.contains("closing address is prose"))
    });
    assert!(told, "a seat calling a tool in a closing should be reported");
}

#[tokio::test]
async fn the_reading_tools_are_offered_only_where_they_work() {
    // The belt is what a model can see. Advertising `web_fetch` to the arbiter
    // or with the shelf switched off produces calls that are refused every
    // single time, which is worse than not offering it.
    let names = |tools: Vec<serde_json::Value>| -> Vec<String> {
        tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap_or_default().to_string())
            .collect()
    };

    let bare = names(council_core::tools::speaking_tools());
    assert!(
        !bare.contains(&"web_fetch".to_string()) && !bare.contains(&"web_search".to_string()),
        "the base belt must not carry the reading tools: {bare:?}"
    );

    let with_shelf = names(council_core::tools::web_tools());
    assert_eq!(with_shelf, vec!["web_search".to_string(), "web_fetch".to_string()]);

    // And they describe themselves usefully, since a local model reads only this.
    let fetch = council_core::tools::web_tools()
        .into_iter()
        .find(|t| t["function"]["name"] == "web_fetch")
        .expect("web_fetch");
    let params = &fetch["function"]["parameters"];
    assert_eq!(params["properties"]["source"]["enum"][0], "auto");
    assert!(
        params["properties"]["title"]["description"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains("wiktionary"),
        "the tool must say a dictionary term works as well as an article"
    );
}

#[tokio::test]
async fn a_seat_that_may_read_is_told_about_the_reading_tools() {
    // Which seats are offered the tool is enforced in one place; what they are
    // *told* is enforced in another, and the two can drift.
    let rec = Arc::new(Recorder::default());
    let mut cfg = config();
    cfg.allow_web = true;
    let backend = Arc::new(Script::new(vec![
        Script::call("make_point", json!({"text": "P1 claim"})),
        Script::call("vote", json!({"point": 1, "verdict": "win"})),
        Script::call("vote", json!({"point": 1, "verdict": "lose"})),
        Script::text("round laid out"),
    ]));
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Engine::with_backend(backend.clone(), sink);
    engine.run(cfg).await;

    let offered: Vec<Vec<String>> = backend
        .seen
        .lock()
        .expect("seen")
        .iter()
        .map(|r| {
            r.tools
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect();
    let advocate = offered
        .iter()
        .find(|belt| belt.contains(&"make_point".to_string()))
        .expect("the advocate's speaking turn");
    assert!(
        advocate.contains(&"web_fetch".to_string()),
        "the advocate argues and should be offered the shelf: {advocate:?}"
    );
    // The prompt and the belt have to agree, or a model reads about a tool it
    // was not given and calls it anyway.
    let system = backend
        .seen
        .lock()
        .expect("seen")
        .iter()
        .flat_map(|r| r.messages.iter())
        .find(|m| m.role == "system")
        .map(|m| m.content.clone())
        .expect("a system prompt");
    assert!(
        system.contains("web_fetch") && system.contains("untrusted_data"),
        "a seat with the tool should be told how to treat what it reads"
    );
}

#[tokio::test]
async fn a_seat_without_the_shelf_is_not_taught_about_it() {
    let rec = Arc::new(Recorder::default());
    let mut cfg = config();
    cfg.allow_web = false;
    let backend = Arc::new(Script::new(vec![
        Script::call("make_point", json!({"text": "P1 claim"})),
        Script::call("vote", json!({"point": 1, "verdict": "win"})),
        Script::call("vote", json!({"point": 1, "verdict": "lose"})),
        Script::text("round laid out"),
    ]));
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Engine::with_backend(backend.clone(), sink);
    engine.run(cfg).await;

    let prompts: Vec<String> = backend
        .seen
        .lock()
        .expect("seen")
        .iter()
        .flat_map(|r| r.messages.iter())
        .filter(|m| m.role == "system")
        .map(|m| m.content.clone())
        .collect();
    for p in &prompts {
        assert!(
            !p.contains("web_fetch") && !p.contains("web_search"),
            "a prompt described tools the seat was not given: {p}"
        );
    }
    let belts: Vec<Vec<String>> = backend
        .seen
        .lock()
        .expect("seen")
        .iter()
        .map(|r| {
            r.tools
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect();
    for belt in &belts {
        assert!(
            !belt.contains(&"web_fetch".to_string()),
            "the shelf is off but the tool was advertised: {belt:?}"
        );
    }
}

#[tokio::test]
async fn what_a_search_returned_is_shown_too() {
    // A seat is handed the hits as readily as a page. If only pages are
    // inspectable, the operator can check half of what the model was given.
    let web = StubWeb::new()
        .search(&[("Epistemic injustice", "unfair distinctions in authority")])
        .page("epistemic injustice", "Epistemic injustice is unfair epistemic authority.");
    let (rec, _backend, _web) = run_with_web(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("web_search", json!({"query": "epistemic injustice"})),
            ChatResponse {
                content: "I have what I need.".into(),
                tool_calls: vec![RawToolCall {
                    id: "x".into(),
                    name: "vote".into(),
                    arguments: json!({"point": 1, "verdict": "win"}),
                }],
                ..Default::default()
            },
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::text("round laid out"),
        ],
        web_config(),
        web,
    )
    .await;
    let shown = rec.events.lock().expect("events").iter().any(|e| {
        matches!(e, EngineEvent::WebFetched { source, text, .. }
            if source == "web_search" && text.contains("Epistemic injustice"))
    });
    assert!(shown, "the hits a seat read should be on the record");
}

#[tokio::test]
async fn a_seat_may_search_and_then_fetch_what_it_found() {
    // The natural sequence, and one the continuation message used to forbid
    // outright: a seat looks up a subject, picks a title, reads that, and then
    // argues. Two round trips inside one turn.
    let web = StubWeb::new()
        .search(&[("Epistemic injustice", "unfair distinctions in authority")])
        .page(
            "epistemic injustice",
            "Epistemic injustice is unfairness in epistemic authority.",
        );
    let (rec, backend, web) = run_with_web(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("web_search", json!({"query": "epistemic injustice"})),
            // The seat picks a title out of its own results.
            Script::call("web_fetch", json!({"title": "Epistemic injustice"})),
            ChatResponse {
                content: "That is the definition I wanted.".into(),
                tool_calls: vec![RawToolCall {
                    id: "x".into(),
                    name: "vote".into(),
                    arguments: json!({"point": 1, "verdict": "win"}),
                }],
                ..Default::default()
            },
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::text("round laid out"),
        ],
        web_config(),
        web.clone(),
    )
    .await;
    let asked = web.asked();
    assert!(
        asked.iter().any(|u| u.contains("list=search")),
        "the search should have happened: {asked:?}"
    );
    assert!(
        asked.iter().any(|u| u.contains("prop=extracts")),
        "the article it found should have been fetched: {asked:?}"
    );
    let saw_page = backend.seen.lock().expect("seen").iter().any(|r| {
        r.messages
            .iter()
            .any(|m| m.content.contains("unfairness in epistemic authority"))
    });
    assert!(saw_page, "the article text should have reached the model");
    let acted = rec.events.lock().expect("events").iter().any(|e| {
        matches!(e, EngineEvent::ActionTaken { delegate, .. } if delegate == "b")
    });
    assert!(acted, "the seat should still have acted in the end");
}

#[tokio::test]
async fn the_council_settles_a_request_for_more_rounds_without_the_operator() {
    // The point of the vote: the debate no longer parks waiting for a human. Two
    // of three is a majority, and the request is granted on that alone.
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 3;
    let (rec, _engine, _s, handle) = run_background(
        vec![
            Script::text("a speaks"),
            Script::corrective(),
            Script::text("b speaks"),
            Script::corrective(),
            Script::text("c speaks"),
            Script::corrective(),
            intermission_proposal(2),
            Script::call("extension_ballot", json!({"grant": true, "reason": "P3 is untouched"})),
            Script::call("extension_ballot", json!({"grant": true, "reason": "P3 is untouched"})),
            // Never asked: the majority was already reached.
            Script::text("the end result"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    let kinds = rec.kinds();
    assert!(kinds.contains(&"extGranted"), "{kinds:?}");
    assert!(
        !kinds.contains(&"extDeclined"),
        "the council granted it, so nothing was declined"
    );
    let asked: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ExtensionBallotCast { delegate, grant, .. } => {
                Some(format!("{delegate}:{grant}"))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        asked.len(),
        2,
        "the third seat should never have been asked: {asked:?}"
    );
    let notice = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .any(|e| matches!(e, EngineEvent::Notice { text, .. } if text.contains("the council")));
    assert!(notice, "the record should say who decided");
}

#[tokio::test]
async fn the_operator_can_still_veto_a_request_the_room_wanted() {
    // The window is real but it is a window: the operator may settle it while
    // the room is still counting, and their answer is the answer.
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.max_extra_rounds = 3;
    let (rec, engine, _s, handle) = run_background_slow(
        vec![
            Script::text("a speaks"),
            Script::corrective(),
            Script::text("b speaks"),
            Script::corrective(),
            Script::text("c speaks"),
            Script::corrective(),
            intermission_proposal(2),
            Script::call("extension_ballot", json!({"grant": true, "reason": "worth it"})),
            Script::call("extension_ballot", json!({"grant": true, "reason": "worth it"})),
            Script::call("extension_ballot", json!({"grant": true, "reason": "worth it"})),
            Script::text("the end result"),
        ],
        cfg,
        400,
    )
    .await;
    wait_for(&rec, "extProposed").await;
    assert!(engine.resolve_extension(false, 0).is_ok(), "a veto must land");
    handle.await.expect("engine task");
    let kinds = rec.kinds();
    assert!(kinds.contains(&"extDeclined"), "{kinds:?}");
    assert!(
        !kinds.contains(&"extGranted"),
        "three yeses must not overrule the operator: {kinds:?}"
    );
}

#[tokio::test]
async fn only_the_arbiter_is_offered_a_ruling() {
    // A tiebreak every model can reach is not a tiebreak; it is a second vote
    // with better manners. The instrument belongs to one seat.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _engine, backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            // The Critic reaching for an instrument that is not on its belt.
            Script::inline(
                "The count is wrong.\n\
                 {\"tool\":\"rule\",\"args\":{\"point\":1,\"ruling\":\"carries\",\"reason\":\"it stands\"}}",
            ),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    let refused = rec.events.lock().expect("events").iter().any(|e| {
        matches!(e, EngineEvent::ActionRejected { delegate, reason, .. }
            if delegate == "b" && reason.contains("only the Arbiter"))
    });
    assert!(refused, "the Critic's ruling should be refused by name");

    let belts: Vec<Vec<String>> = backend
        .seen
        .lock()
        .expect("seen")
        .iter()
        .map(|r| {
            r.tools
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect();
    // Exactly one belt carries it. The third speaking turn is the Arbiter's, so
    // this is both the count and the identity in one assertion.
    let holders: Vec<usize> = belts
        .iter()
        .enumerate()
        .filter(|(_, b)| b.contains(&"rule".to_string()))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        holders,
        vec![2],
        "only the Arbiter's third turn may carry `rule`: {belts:?}"
    );
}

#[tokio::test]
async fn a_ruling_needs_a_reason_the_record_can_stand_behind() {
    // A ruling with no stated ground is an opinion with a badge on it, and it
    // would settle a point outright, so it is refused outright.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _engine, _s, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::inline(
                "It carries.\n\
                 {\"tool\":\"rule\",\"args\":{\"point\":1,\"ruling\":\"carries\"}}",
            ),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    let rejections: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ActionRejected { delegate, reason, .. } => {
                Some(format!("{delegate}: {reason}"))
            }
            _ => None,
        })
        .collect();
    assert!(
        rejections.iter().any(|r| r.contains("needs a `reason`")),
        "a ruling without a reason should be refused, got {rejections:?}"
    );
}

/// A scripted backend where one model fails its first `fails` requests and then
/// behaves normally.
///
/// A failed turn is the interesting case for everything the engine rolls back:
/// the request has already drained the press queue and advanced the seat's
/// reading mark by the time the model is called, so a turn that never returns
/// is exactly when those restores have to be real.
///
/// The error is a parameter because the two kinds are not handled alike — one is
/// retried, the other is not — and a test harness that cannot express that
/// difference cannot check it.
struct Flaky {
    scripts: Arc<Script>,
    /// Model that misbehaves, and how many of its calls should.
    stuck_model: String,
    fails: std::sync::atomic::AtomicUsize,
    error: fn() -> LlmError,
}

#[async_trait::async_trait]
impl ChatBackend for Flaky {
    async fn chat(
        &self,
        req: &ChatRequest,
        on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
    ) -> Result<ChatResponse, LlmError> {
        if req.model == self.stuck_model {
            let left = self
                .fails
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |n| Some(n.saturating_sub(1)),
                )
                .unwrap_or(0);
            if left > 0 {
                return Err((self.error)());
            }
        }
        self.scripts.chat(req, on_chunk).await
    }
}

/// A malformed request. The server will say the same thing every time, so it
/// must not be retried.
fn bad_request() -> LlmError {
    LlmError::Status {
        url: "http://127.0.0.1:8000/v1/chat/completions".into(),
        status: 400,
        body: "the model name is wrong".into(),
    }
}

/// A read timeout. The network having a moment, so the same request may well
/// succeed this time.
fn timed_out() -> LlmError {
    LlmError::Stream("timed out waiting for the model".into())
}

/// Every user message a given seat was ever shown, joined — the only place the
/// text the engine actually wrote can be inspected.
fn prompts_for(backend: &Script, model: &str) -> String {
    backend
        .seen
        .lock()
        .expect("seen")
        .iter()
        .filter(|r| r.model == model)
        .flat_map(|r| r.messages.iter())
        .filter(|m| m.role == "user")
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n———\n")
}

/// The system prompt a seat was given — where its belt, and only its belt, is
/// described.
fn system_for(backend: &Script, model: &str) -> String {
    backend
        .seen
        .lock()
        .expect("seen")
        .iter()
        .filter(|r| r.model == model)
        .filter_map(|r| r.messages.iter().find(|m| m.role == "system"))
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n———\n")
}

#[tokio::test]
async fn the_arbiter_rules_once_a_turn_though_the_prompts_say_so() {
    // Both the standing rules and the tool's own description promise one ruling
    // a turn. Nothing enforced it, so an Arbiter could rule every point in one
    // reply and the promise was decoration.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, engine, backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::inline(concat!(
                "Two points need settling.\n",
                r#"{"tool":"rule","args":{"point":1,"ruling":"carries","reason":"answered"}}"#,
                "\n",
                r#"{"tool":"rule","args":{"point":1,"ruling":"falls","reason":"refuted"}}"#
            )),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let events = rec.events.lock().expect("events");
    let ruled: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ActionTaken { action, detail, .. } if action == "rule" => {
                Some(detail.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(ruled.len(), 1, "exactly one ruling should have been applied");
    assert!(
        ruled[0].contains("carries"),
        "the first ruling should stand, got {ruled:?}"
    );

    let refused = events.iter().any(|e| {
        matches!(e, EngineEvent::ActionRejected { tool, reason, .. }
            if tool == "rule" && reason.contains("one ruling a turn"))
    });
    assert!(refused, "the second ruling should be refused for what it is");
    drop(backend);
}

#[tokio::test]
async fn a_turn_that_never_returns_does_not_take_the_seats_questions_with_it() {
    // The press queue is drained before the model is called, so a skipped or
    // failed turn used to destroy every question pressed on that seat. There was
    // no second chance: the questions were gone for the rest of the debate.
    let rec = Arc::new(Recorder::default());
    let scripts = Script::new(vec![
        Script::call(
            "press",
            json!({"to": "Judge", "question": "does the count include implicit votes?"}),
        ),
        // Round 1: the Judge's turn errors out and its rollback has to put the
        // question back.
        Script::text("unused"),
        // Round 2: the Judge answers, and must find the question waiting.
        Script::call("vote", json!({"point": 1, "verdict": "stalemate"})),
        Script::text("nothing open"),
        Script::text("round laid out"),
    ]);
    let backend = Arc::new(Flaky {
        scripts: Arc::new(scripts),
        stuck_model: "m-c".into(),
        fails: std::sync::atomic::AtomicUsize::new(1),
        error: bad_request,
    });
    let inner = backend.scripts.clone();
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(backend, sink));
    let handle = {
        let engine = engine.clone();
        tokio::spawn(async move {
            let mut cfg = config();
            cfg.max_rounds = 2;
            engine.run(cfg).await
        })
    };
    handle.await.expect("engine task");

    assert!(
        rec.events.lock().expect("events").iter().any(|e| matches!(
            e,
            EngineEvent::Failed { message, .. } if message.contains("Judge")
        )),
        "the Judge's turn should have been recorded as failed"
    );
    let seen = prompts_for(&inner, "m-c");
    assert!(
        seen.contains("does the count include implicit votes?"),
        "the pressed question should still have reached the seat: {seen}"
    );
    // Restored once and consumed once: the seat is pressed once, not re-pressed
    // every turn by a question that survived a turn nobody answered.
    assert_eq!(
        seen.matches("You are being pressed").count(),
        1,
        "the question should be restored once and then consumed: {seen}"
    );
}

#[tokio::test]
async fn a_seat_that_fell_behind_is_told_it_fell_behind() {
    // Forty entries is the most a turn can carry. Past that the head is
    // dropped, and the seat used to be told nothing: it went on believing it
    // had read the record, and argued against a board three rounds of rebuttal
    // had moved since it last looked.
    //
    let rec = Arc::new(Recorder::default());
    // The Judge's own turns fail, so its reading mark never moves while the two
    // arguing seats fill the record behind it. That is what a seat that was
    // skipped or that fell back to a slow model actually looks like.
    fn a_turn_full_of_claims(round: usize, seat: usize) -> ChatResponse {
        let mut body = String::from("Twelve claims this turn.\n");
        for i in 0..12 {
            // Distinct text every time: the board refuses a restated claim as a
            // re-raise, so a repeated filler would never reach the record.
            body.push_str(&format!(
                "\n```tool\n{{\"tool\":\"make_point\",\"args\":{{\"text\":\"r{round} s{seat} c{i}\"}}}}\n```"
            ));
        }
        Script::inline(&body)
    }

    let mut script = Vec::new();
    for round in 0..3 {
        script.push(a_turn_full_of_claims(round, 0)); // Advocate
        script.push(a_turn_full_of_claims(round, 1)); // Critic
        script.push(Script::text("round laid out")); // Summarizer, after the Judge failed
    }
    script.push(Script::call("vote", json!({"point": 1, "verdict": "win"})));
    let scripts = Script::new(script);
    let backend = Arc::new(Flaky {
        scripts: Arc::new(scripts),
        stuck_model: "m-c".into(),
        // Rounds 1 and 2. Round 3 is the turn this test is reading.
        fails: std::sync::atomic::AtomicUsize::new(2),
        error: bad_request,
    });
    let inner = backend.scripts.clone();
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(backend, sink));
    let handle = {
        let engine = engine.clone();
        tokio::spawn(async move {
            let mut cfg = config();
            cfg.max_rounds = 3;
            engine.run(cfg).await
        })
    };
    handle.await.expect("engine task");

    let seen = prompts_for(&inner, "m-c");
    assert!(
        seen.contains("earlier record"),
        "the seat should be told its feed could not carry everything: {seen}"
    );
    // The number must be real: the mark advances over what it dropped, and no
    // further, so the count reported is the count actually lost.
    let at = seen.find("earlier record").expect("already asserted");
    let digits: String = seen[..at]
        .trim_end()
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect();
    let claimed: usize = digits.chars().rev().collect::<String>().parse().unwrap_or(0);
    assert!(
        claimed > 0,
        "the number of dropped records should be stated, not left vague: {:?}",
        &seen[at.saturating_sub(60)..(at + 60).min(seen.len())]
    );
}

#[tokio::test]
async fn a_server_without_tool_calling_is_actually_told_the_syntax() {
    // The inline path has a scraper, a parser and a dozen unit tests, and none
    // of them check the one thing that matters: whether the model was ever
    // shown what a tool call looks like. It was not — the protocol was a
    // constant nothing referenced — so every one of those tests passed against
    // calls the test harness had formatted for it.
    let rec = Arc::new(Recorder::default());
    let scripts = Script::new(vec![
        Script::inline("I am raising a claim.\n```tool\n{\"tool\":\"make_point\",\"args\":{\"text\":\"a claim\"}}\n```"),
        Script::inline("I am voting.\n```tool\n{\"tool\":\"vote\",\"args\":{\"point\":1,\"verdict\":\"win\"}}\n```"),
        Script::text("round laid out"),
    ]);
    let backend = Arc::new(scripts);
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(backend.clone(), sink));
    let handle = {
        let engine = engine.clone();
        tokio::spawn(async move {
            let mut cfg = config();
            cfg.max_rounds = 1;
            cfg.endpoints[0].native_tools = false;
            engine.run(cfg).await
        })
    };
    handle.await.expect("engine task");

    // No tools are advertised, so the instruction has to live in the prose.
    // The summarizer never acts on the board and has its own prompt, so it is
    // not an arguing seat and is not asked to write calls by hand.
    for req in backend
        .seen
        .lock()
        .expect("seen")
        .iter()
        .filter(|r| r.model != "m-s")
    {
        assert!(
            req.tools.is_empty(),
            "a non-native endpoint must not be offered tool schemas"
        );
        let system = req
            .messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        assert!(
            system.contains("```tool"),
            "{} was asked to call tools by hand but never shown how: {system}",
            req.model
        );
        assert!(
            system.contains(r#"{"tool":"vote","args":{"point":1"#),
            "the shapes have to include concrete examples, not just the envelope"
        );
    }

    // And the seat obeyed, which is the part that actually matters.
    assert!(
        rec.events.lock().expect("events").iter().any(
            |e| matches!(e, EngineEvent::ActionTaken { action, .. } if action == "make_point")
        ),
        "a seat shown the syntax should manage to use it"
    );
}

#[tokio::test]
async fn a_native_server_is_never_asked_to_write_calls_by_hand() {
    // The opposite failure: contradicting the server's own interface invites
    // JSON pasted into the middle of a speech, which the scraper then has to
    // unpick from the argument.
    let rec = Arc::new(Recorder::default());
    let scripts = Script::new(vec![
        Script::call("make_point", json!({"text": "a claim"})),
        Script::call("vote", json!({"point": 1, "verdict": "win"})),
        Script::text("round laid out"),
    ]);
    let backend = Arc::new(scripts);
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(backend.clone(), sink));
    let handle = {
        let engine = engine.clone();
        tokio::spawn(async move {
            let mut cfg = config();
            cfg.max_rounds = 1;
            engine.run(cfg).await
        })
    };
    handle.await.expect("engine task");

    for req in backend.seen.lock().expect("seen").iter() {
        let system = req
            .messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        assert!(
            !system.contains("```tool"),
            "{} has native tools and should not also be told to write them",
            req.model
        );
    }
}

#[tokio::test]
async fn only_the_arbiter_is_told_it_may_rule() {
    // Two thirds of this test is the engine refusing `rule` from the wrong seat.
    // The other third is the part that makes that refusal fair: a seat that was
    // never offered the tool should not have been promised it either. Telling
    // every model about an instrument only one of them has guarantees a stream
    // of invented calls, each refused, each teaching nothing.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, engine, backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::inline(
                "I would settle this myself.\n\
                 {\"tool\":\"rule\",\"args\":{\"point\":1,\"ruling\":\"carries\",\"reason\":\"mine\"}}",
            ),
            Script::inline(
                "The count is wrong.\n\
                 {\"tool\":\"rule\",\"args\":{\"point\":1,\"ruling\":\"carries\",\"reason\":\"answered\"}}",
            ),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let critic = system_for(&backend, "m-b");
    assert!(
        !critic.contains("`rule`"),
        "the Critic was promised a tool it cannot reach: {critic}"
    );
    let arbiter = system_for(&backend, "m-c");
    assert!(
        arbiter.contains("`rule`"),
        "the Arbiter must be told about its own instrument"
    );
    drop(rec);
}

#[tokio::test]
async fn a_seats_own_ballot_answer_stays_in_its_own_history() {
    // Four of the five request paths pushed a ballot prompt into the delegate's
    // persistent history and never recorded what it replied. The seat's history
    // therefore ended on a question it had not answered, and the next speaking
    // turn appended a second one — two user messages in a row, which strict
    // proxies reject, on top of the seat losing sight of its own reasoning.
    // The script is consumed in global speaking order, so a ballot cast inside
    // another's turn takes its slot at that point: Advocate, then Critic opens
    // the poll, which asks Advocate and Arbiter, then the Arbiter's own turn.
    let mut cfg = config();
    cfg.max_rounds = 2;
    let (rec, engine, backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("poll", json!({
                "question": "which reading is right?",
                "options": ["the narrow one", "the broad one"],
                "reason": "the record is split"
            })),
            Script::call(
                "poll_ballot",
                json!({"choice": "the narrow one", "reason": "it survives the objection"}),
            ),
            Script::call(
                "poll_ballot",
                json!({"choice": "the broad one", "reason": "it explains more"}),
            ),
            Script::call("rebut", json!({"point": 1, "argument": "the broad reading fails"})),
            Script::text("round laid out"),
            Script::call("cite", json!({"source": "the record", "quote": "one line"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("steelman", json!({"point": 1, "restatement": "the strongest form"})),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);
    drop(rec);

    for model in ["m-a", "m-b", "m-c"] {
        for req in backend.seen.lock().expect("seen").iter().filter(|r| r.model == model) {
            let roles: Vec<&str> = req.messages.iter().map(|m| m.role.as_str()).collect();
            // The rule that matters: a question the engine asked is always
            // followed by that seat's own reply. A ballot prompt with no answer
            // after it is a question the model can see was never answered, and
            // it also leaves two user turns touching.
            for (i, m) in req.messages.iter().enumerate() {
                if m.role != "user" {
                    continue;
                }
                let asked = if m.content.contains("COUNCIL POLL") {
                    Some("the poll it was asked")
                } else if m.content.contains("DEADLOCK REVIEW") {
                    Some("the review it was asked")
                } else if m.content.contains("moves to adjourn") {
                    Some("the adjournment it was asked")
                } else if m.content.contains("asks for") && m.content.contains("more round") {
                    Some("the extension it was asked")
                } else {
                    None
                };
                let Some(what) = asked else { continue };
                // This request *is* the ballot: it was recorded the moment the
                // question went out, before any answer could exist. Only a
                // prompt followed by more of the conversation can be judged.
                if i + 1 == req.messages.len() {
                    continue;
                }
                assert_eq!(
                    roles.get(i + 1).copied(),
                    Some("assistant"),
                    "{model} was asked {what} and left it unanswered: {roles:?}"
                );
            }
        }
    }

    // The seats that answered the poll can still see what they said. Losing it
    // is the quieter half of the same bug: the model builds on its reasoning,
    // not just on its last parsed choice.
    let said_by_advocate: Vec<String> = {
        let seen = backend.seen.lock().expect("seen");
        seen.iter()
            .filter(|r| r.model == "m-a")
            .flat_map(|r| r.messages.iter())
            .filter(|m| m.role == "assistant")
            .map(|m| m.content.clone())
            .collect()
    };
    assert!(
        said_by_advocate
            .iter()
            .any(|m| m.contains("it survives the objection")),
        "the seat should still be able to see its own ballot reasoning, found {said_by_advocate:?}"
    );
}

#[tokio::test]
async fn a_reply_the_server_cut_short_is_not_turned_into_a_vote() {
    // The poll's prose fallback used to substring-match the options, so a
    // truncated reply became a vote and `unreadable_ballot` — the whole reason
    // it exists, to tell a lost vote from a deliberate abstention — never got
    // to say anything.
    let mut cfg = config();
    cfg.endpoints[0].native_tools = false;
    cfg.max_rounds = 1;
    // The Advocate opens it, so the Critic and Arbiter are asked. Every reply is
    // inline: an endpoint with no tool calling ignores a reply that only
    // carried `tool_calls`, and a turn that ends up empty draws a corrective
    // re-prompt which silently eats the next script entry.
    let (rec, engine, backend, handle) = run_background(
        vec![
            Script::inline(
                "The record is split.\n\
                 {\"tool\":\"poll\",\"args\":{\"question\":\"settled?\",\"options\":[\"yes\",\"no\"],\"reason\":\"split\"}}",
            ),
            // Cut short, with the answer fully present in the text. A cut-off
            // reply is a lost vote whatever it happens to contain: the rest of
            // the sentence is exactly what is missing, and it may have reversed.
            Script::cut_off("I think the answer is yes, although on reflection I"),
            Script::inline("It is yes, plainly."),
            Script::inline("nothing further from me"),
            Script::inline("nothing further from me"),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let closed = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .find_map(|e| match e {
            EngineEvent::PollClosed { poll, .. } => Some(poll.clone()),
            _ => None,
        })
        .expect("the poll should have closed");
    assert!(
        !closed.ballots.iter().any(|v| v.delegate == "b"),
        "a reply cut off mid-word is not an answer: {closed:?}"
    );
    let c = closed
        .ballots
        .iter()
        .find(|v| v.delegate == "c")
        .expect("the Arbiter answered in prose");
    assert_eq!(
        c.choice, "yes",
        "the whole-word match should still rescue a seat with no tool support"
    );

    // And the cut-off seat is recorded as not answering, in those words.
    let said: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        said.iter().any(|n| n.contains("cut the reply short")),
        "the truncation should have been named, not guessed at: {said:?}"
    );
    drop(backend);
}

#[tokio::test]
async fn naming_two_options_in_one_reply_counts_as_no_answer() {
    let mut cfg = config();
    cfg.endpoints[0].native_tools = false;
    cfg.max_rounds = 1;
    let (rec, engine, backend, handle) = run_background(
        vec![
            // Inline, because this endpoint has no tool calling: a reply that only
            // carried `tool_calls` would be ignored on purpose.
            Script::inline(
                "The record is split.\n\
                 {\"tool\":\"poll\",\"args\":{\"question\":\"settled?\",\"options\":[\"yes\",\"no\"],\"reason\":\"split\"}}",
            ),
            Script::inline("It is yes, although no would also do."),
            Script::inline("It is no."),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let closed = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .find_map(|e| match e {
            EngineEvent::PollClosed { poll, .. } => Some(poll.clone()),
            _ => None,
        })
        .expect("the poll should have closed");
    assert!(
        !closed.ballots.iter().any(|v| v.delegate == "a"),
        "a reply that named both options is not an answer to either: {closed:?}"
    );
    assert!(
        closed.ballots.iter().any(|v| v.delegate == "c" && v.choice == "no"),
        "a reply that named one option plainly should still count: {closed:?}"
    );
    drop(backend);
}

#[tokio::test]
async fn a_citation_of_a_point_that_does_not_exist_is_refused() {
    // It used to succeed, unattached, and be written to the record as evidence
    // the board had collected and then quietly lost.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, engine, _backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call(
                "cite",
                json!({"source": "a book", "quote": "a line", "point": 9, "stance": "supports"}),
            ),
            Script::call("cite", json!({"source": "a book", "quote": "a line"})),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let refused = rec.events.lock().expect("events").iter().any(|e| {
        matches!(e, EngineEvent::ActionRejected { tool, reason, .. }
            if tool == "cite" && reason.contains("P9 does not exist"))
    });
    assert!(refused, "a citation aimed at a missing point should be refused");

    // A citation with no point at all is a note, and still allowed.
    let taken = rec.events.lock().expect("events").iter().any(|e| {
        matches!(e, EngineEvent::ActionTaken { action, .. } if action == "cite")
    });
    assert!(taken, "a citation with no point is still worth recording");
}

#[tokio::test]
async fn a_seat_that_answers_the_poll_with_a_jumbled_call_is_still_counted() {
    // The ballot is found wherever the seat put it; everything else in the same
    // reply is a board action and belongs on the board.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, _engine, _backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            // Inline, because this endpoint has no tool calling: a reply that only
            // carried `tool_calls` would be ignored on purpose.
            Script::inline(
                "The record is split.\n\
                 {\"tool\":\"poll\",\"args\":{\"question\":\"settled?\",\"options\":[\"yes\",\"no\"],\"reason\":\"split\"}}",
            ),
            Script::inline(concat!(
                "On the question, yes.\n",
                "```tool\n{\"tool\":\"poll_ballot\",\"args\":{\"choice\":\"yes\",\"reason\":\"clear\"}}\n```\n",
                "And on the point:\n",
                "```tool\n{\"tool\":\"vote\",\"args\":{\"point\":1,\"verdict\":\"win\"}}\n```\n",
                "```tool\n{\"tool\":\"nonexistent_tool\",\"args\":{}}\n```"
            )),
            Script::call(
                "poll_ballot",
                json!({"choice": "yes", "reason": "clear"}),
            ),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");

    let events = rec.events.lock().expect("events");
    assert!(
        events.iter().any(|e| matches!(e, EngineEvent::ActionTaken { action, .. } if action == "vote")),
        "the vote bundled with the ballot should reach the board"
    );
    // The invented tool is named, not swallowed: silence teaches nothing.
    assert!(
        events.iter().any(|e| matches!(e, EngineEvent::ActionRejected { tool, .. } if tool == "nonexistent_tool")),
        "a malformed call alongside a ballot should still be refused by name"
    );
}

#[tokio::test]
async fn two_seats_against_a_point_is_what_it_takes_to_strike_it() {
    // Reported as missing from a real debate. Not evidence of a bug on its own,
    // so this pins the actual arithmetic: a proponent implicitly rates their own
    // point a win, so a point starts 1-0 and needs two `lose` votes to fall.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, engine, _backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "Embryos have moral status"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose", "reason": "no"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose", "reason": "not yet"})),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let last = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::PointUpdated { point, tally, .. } if point.id == 1 => {
                Some((tally.status, tally.score, tally.wins, tally.losses))
            }
            _ => None,
        })
        .next_back()
        .expect("the point should have been reported");
    assert_eq!(
        last,
        (PointStatus::Fell, -1, 1, 2),
        "two seats against an implicit proponent win should strike the point"
    );
}

#[tokio::test]
async fn a_point_raised_at_one_against_one_still_carries_on_three() {
    // The other half of the arithmetic, and the reason a debate can look like it
    // has no opposition: the proponent's implicit win is a real vote, so a point
    // hit by one seat and backed by another reads as +1 — carried — rather than
    // as the split it is. The count is right; the shorthand is what hides it.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, engine, _backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "A claim"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose", "reason": "no"})),
            Script::call("vote", json!({"point": 1, "verdict": "win", "reason": "yes"})),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let last = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::PointUpdated { point, tally, .. } if point.id == 1 => {
                Some((tally.status, tally.score))
            }
            _ => None,
        })
        .next_back()
        .expect("the point should have been reported");
    assert_eq!(
        last,
        (PointStatus::Carried, 1),
        "2 up to 1 down is a carry — the dissent is real and the score does not show it"
    );
}

#[tokio::test]
async fn a_transient_failure_costs_a_moment_and_not_the_turn() {
    // A gateway that takes thirty seconds to decide used to end a seat's turn.
    // It sometimes recovered on its own and sometimes did not, which made it look
    // like the engine was losing seats at random — the difference being whether
    // the reply happened to land inside the timeout.
    let rec = Arc::new(Recorder::default());
    let backend = Arc::new(Flaky {
        scripts: Arc::new(Script::new(vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose", "reason": "no"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose", "reason": "no"})),
            Script::text("round laid out"),
        ])),
        stuck_model: "m-b".into(),
        fails: std::sync::atomic::AtomicUsize::new(1),
        error: timed_out,
    });
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(backend, sink));
    let handle = {
        let engine = engine.clone();
        tokio::spawn(async move {
            let mut cfg = config();
            cfg.max_rounds = 1;
            engine.run(cfg).await
        })
    };
    handle.await.expect("engine task");

    assert!(
        !rec.events.lock().expect("events").iter().any(
            |e| matches!(e, EngineEvent::Failed { message, .. } if message.contains("Critic"))
        ),
        "a timeout should not have ended the seat's turn"
    );
    assert!(
        rec.events.lock().expect("events").iter().any(
            |e| matches!(e, EngineEvent::ActionTaken { delegate, .. } if delegate == "b")
        ),
        "the retry should have landed the vote that was almost lost"
    );
    // Said out loud, because a turn that quietly took two requests is a turn
    // somebody is waiting on.
    let said = rec.events.lock().expect("events").iter().any(|e| {
        matches!(e, EngineEvent::Notice { text, tone: NoticeTone::Warn, .. }
            if text.contains("retrying"))
    });
    assert!(said, "the retry should have been visible to the operator");
}

#[tokio::test]
async fn a_rejected_request_is_not_retried_because_it_would_only_be_refused_again() {
    // The other half of the retry rule: five identical 400s are five lost turns
    // and a slower, less useful error than the first one.
    let rec = Arc::new(Recorder::default());
    let backend = Arc::new(Flaky {
        scripts: Arc::new(Script::new(vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::text("unavailable"),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::text("round laid out"),
        ])),
        stuck_model: "m-b".into(),
        // Every call this seat makes fails, so a retry would be unmistakable.
        fails: std::sync::atomic::AtomicUsize::new(usize::MAX),
        error: bad_request,
    });
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(backend, sink));
    let handle = {
        let engine = engine.clone();
        tokio::spawn(async move {
            let mut cfg = config();
            cfg.max_rounds = 1;
            engine.run(cfg).await
        })
    };
    handle.await.expect("engine task");

    // The refusal is reported rather than retried into the ground.
    assert!(
        rec.events.lock().expect("events").iter().any(
            |e| matches!(e, EngineEvent::Failed { message, .. } if message.contains("Critic"))
        ),
        "a malformed request should be reported to the operator"
    );
    let retried = rec.events.lock().expect("events").iter().any(
        |e| matches!(e, EngineEvent::Notice { text, .. } if text.contains("retrying")),
    );
    assert!(!retried, "a request that will be refused again must not be retried");
}

#[tokio::test]
async fn the_room_says_what_it_is_doing_while_a_turn_is_open() {
    // A seat that has reached for a tool, and a council asking every other seat
    // a question, both look exactly like a hung model from outside: the words
    // stop and nothing resolves. A poll in particular holds the opener's turn
    // open for a round trip per seat, with nothing on the transcript to say so.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, engine, _backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1 claim"})),
            Script::call(
                "poll",
                json!({"question": "settled?", "options": ["yes", "no"], "reason": "split"}),
            ),
            Script::call("poll_ballot", json!({"choice": "yes", "reason": "clear"})),
            Script::call("poll_ballot", json!({"choice": "yes", "reason": "clear"})),
            Script::call("rebut", json!({"point": 1, "argument": "not so"})),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let said: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::TurnActivity { what, .. } => Some(what.clone()),
            _ => None,
        })
        .collect();
    assert!(
        said.iter().any(|w| w.contains("asking 2 other seats")),
        "the opener should be told the council is being asked: {said:?}"
    );
    assert!(
        said.iter().any(|w| w.contains("acting on the board")),
        "and that the turn is being written up: {said:?}"
    );
}

#[tokio::test]
async fn a_point_raised_by_one_seat_counts_that_seats_implicit_support() {
    // The count a reader is shown has to include the proponent's implicit win,
    // or the split on the card disagrees with the pills underneath it. Pinned
    // here because the number is what the board is read from.
    let mut cfg = config();
    cfg.max_rounds = 2;
    let (rec, engine, _backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "A claim"})),
            Script::call("vote", json!({"point": 1, "verdict": "win", "reason": "yes"})),
            // The Arbiter rates its own point: two wins so far plus its own.
            Script::call("vote", json!({"point": 1, "verdict": "win", "reason": "yes"})),
            Script::text("round laid out"),
            Script::text("nothing to add"),
            Script::text("nothing to add"),
            Script::text("nothing to add"),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let last = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::PointUpdated { point, tally, .. } if point.id == 1 => {
                Some((tally.wins, tally.losses, tally.stalemates, tally.score, tally.status))
            }
            _ => None,
        })
        .next_back()
        .expect("a tally for P1");
    assert_eq!(
        last,
        (3, 0, 0, 3, PointStatus::Carried),
        "the Advocate raises it and counts as supporting it, the Arbiter votes \
         for it, and the Critic stays silent: three up, and the score agrees \
         with the three ▲ the board shows"
    );
}

#[tokio::test]
async fn a_point_the_arbiter_raised_counts_its_own_support() {
    // The board showed `2▲ +2` on a card where the Advocate, the Critic and the
    // Arbiter all sat at ▲. If the raiser's implicit support is missing from the
    // tally, the count disagrees with the pills printed directly beneath it.
    let mut cfg = config();
    cfg.max_rounds = 2;
    // The Arbiter speaks last in a round, so it raises the point in round one
    // and the other two back it in round two.
    let (rec, engine, _backend, handle) = run_background(
        vec![
            // Every seat acts every turn: a turn with no tool call draws a
            // corrective re-prompt, which quietly eats the next script entry and
            // hands this seat's point to somebody else.
            Script::call("press", json!({"to": "Critic", "question": "what is your basis?"})),
            Script::call("press", json!({"to": "Advocate", "question": "and yours?"})),
            Script::inline(concat!(
                "I raise it.\n",
                r#"{"tool":"make_point","args":{"text":"A claim"}}"#
            )),
            Script::text("round laid out"),
            Script::call("vote", json!({"point": 1, "verdict": "win", "reason": "yes"})),
            Script::call("vote", json!({"point": 1, "verdict": "win", "reason": "yes"})),
            Script::call("vote", json!({"point": 1, "verdict": "win", "reason": "mine, stated"})),
            Script::text("round laid out"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let final_tally = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::PointUpdated { point, tally, .. } if point.id == 1 => {
                Some((point.raised_by.clone(), tally.wins, tally.score, tally.status))
            }
            _ => None,
        })
        .next_back()
        .expect("a tally for P1");
    assert_eq!(
        final_tally,
        ("c".to_string(), 3, 3, PointStatus::Carried),
        "three seats at ▲ should be three up and +3, not two"
    );
}

#[tokio::test]
async fn a_seat_is_never_sent_two_copies_of_the_board() {
    // Each turn's brief is a full rendering of the whole board, and the board is
    // regenerated every turn. Keeping the older copies meant every prompt ended
    // up carrying three or four of them, all but the newest superseded — a third
    // of the payload, and growing, because each snapshot is bigger than the
    // last.
    let mut cfg = config();
    cfg.max_rounds = 3;
    let (_rec, engine, backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call(
                "poll",
                json!({"question": "q", "options": ["a", "b"], "reason": "r"}),
            ),
            Script::call("poll_ballot", json!({"choice": "a", "reason": "x"})),
            Script::call("poll_ballot", json!({"choice": "a", "reason": "x"})),
            Script::text("r"),
            Script::call("make_point", json!({"text": "P2"})),
            Script::call("vote", json!({"point": 2, "verdict": "win"})),
            Script::call(
                "poll",
                json!({"question": "q2", "options": ["a", "b"], "reason": "r"}),
            ),
            Script::call("poll_ballot", json!({"choice": "b", "reason": "y"})),
            Script::call("poll_ballot", json!({"choice": "b", "reason": "y"})),
            Script::text("r"),
            Script::call("make_point", json!({"text": "P3"})),
            Script::call("vote", json!({"point": 3, "verdict": "win"})),
            Script::call("rebut", json!({"point": 1, "argument": "no"})),
            Script::text("r"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let seen = backend.seen.lock().expect("seen");
    for model in ["m-a", "m-b", "m-c"] {
        for req in seen.iter().filter(|r| r.model == model) {
            let briefs = req
                .messages
                .iter()
                .filter(|m| m.role == "user" && m.content.starts_with("THE BOARD"))
                .count();
            assert!(
                briefs <= 1,
                "{model} was sent {briefs} copies of the board in one request"
            );
            // And what is left is the seat's own side of the conversation: the
            // standing head survives every prune.
            assert_eq!(
                req.messages.first().map(|m| m.role.as_str()),
                Some("system"),
                "the system prompt must never be pruned away"
            );
            assert!(
                req.messages
                    .iter()
                    .any(|m| m.role == "user" && m.content.contains("The motion is before")),
                "{model} lost the opening message: {:?}",
                req.messages.iter().map(|m| (m.role.as_str(), m.content.chars().take(40).collect::<String>())).collect::<Vec<_>>()
            );
        }
    }
}

#[tokio::test]
async fn a_long_turn_does_not_come_back_whole_in_the_next_one() {
    // A seat that argues at length was having its entire speech put back into its
    // own context, eight of them deep. This is not what stalled the request in
    // the logs — the reported CONTEXT there was the model's context *cap*, not
    // the prompt — but the growth is real and unbounded, and a long debate of
    // long turns will get there.
    let long = |seed: &str| -> String {
        format!(
            "{seed} {}",
            (0..1400)
                .map(|i| format!("w{i}"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let say = |body: String| ChatResponse {
        content: body,
        ..Default::default()
    };
    let script = vec![
        say(long("opening")),
        say(long("second")),
        say(long("third")),
        say(long("fourth")),
        say(long("fifth")),
        say(long("sixth")),
        say(long("seventh")),
        say(long("eighth")),
        say(long("ninth")),
        say(long("tenth")),
        say("round laid out".to_string()),
    ];
    let mut cfg = config();
    cfg.max_rounds = 10;
    let (_rec, engine, backend, handle) = run_background(script, cfg).await;
    handle.await.expect("engine task");
    drop(engine);

    let seen = backend.seen.lock().expect("seen");
    for model in ["m-a", "m-b", "m-c"] {
        let peak = seen
            .iter()
            .filter(|r| r.model == model)
            .map(|r| r.messages.iter().map(|m| m.content.len()).sum::<usize>())
            .max()
            .unwrap_or(0);
        // ~11k chars of speech; clipped, ten of them fit in a prompt that is not
        // mostly the seat reading itself back.
        assert!(
            peak < 40_000,
            "{model} was sent {peak} chars of context — the history is carrying \
             whole speeches"
        );
    }

    // And the clip keeps the shape of the argument: the opening and the landing.
    // The last request is the one carrying ten turns of history.
    let last = seen
        .iter()
        .filter(|r| r.model == "m-a")
        .next_back()
        .expect("a request");
    let kept: Vec<&str> = last
        .messages
        .iter()
        .filter(|m| m.role == "assistant" && m.content.contains("characters of this turn"))
        .map(|m| m.content.as_str())
        .collect();
    assert!(
        !kept.is_empty(),
        "long turns should be clipped, and the clip should say so"
    );
    assert!(
        kept[0].starts_with("opening ") && kept[0].contains("w1399"),
        "the opening and the end of the turn should both survive: {:?}",
        &kept[0][..kept[0].len().min(120)]
    );
}

#[tokio::test]
async fn a_seat_may_revise_its_own_earlier_vote() {
    // Seats were markedly reluctant to re-rate a point, including one they had
    // rated themselves. The record has to carry the revision as a first-class
    // event rather than as a quiet overwrite, or the timeline shows a single
    // verdict and the change of mind disappears.
    let mut cfg = config();
    cfg.max_rounds = 2;
    let (rec, engine, _backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose", "reason": "not shown"})),
            Script::call("vote", json!({"point": 1, "verdict": "win", "reason": "held"})),
            Script::text("r"),
            // The Advocate rates its own point, then changes its mind about it.
            Script::call("vote", json!({"point": 1, "verdict": "lose", "reason": "I no longer hold it"})),
            Script::call("vote", json!({"point": 1, "verdict": "win", "reason": "answered"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose", "reason": "answered twice"})),
            Script::text("r"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let revotes: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ActionTaken { delegate, detail, .. } if detail.contains("re-voted") => {
                Some(format!("{delegate}: {detail}"))
            }
            _ => None,
        })
        .collect();
    assert!(
        !revotes.is_empty(),
        "a seat changing its own mind should be recorded as re-voting"
    );
    assert!(
        revotes.iter().any(|r| r.starts_with("a:")),
        "the Advocate revised its own point: {revotes:?}"
    );

    // And the last word stands: it lost, because two seats said so.
    let last = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::PointUpdated { point, tally, .. } if point.id == 1 => {
                Some((tally.wins, tally.losses, tally.status.clone()))
            }
            _ => None,
        })
        .next_back()
        .expect("a tally for P1");
    // a=lose, b=win, c=lose. The Advocate revising its own point cost it the
    // implicit support it started with, which is the whole difference between a
    // revision and a second vote.
    assert_eq!(
        last,
        (1, 2, PointStatus::Fell),
        "a revision replaces the earlier rating rather than adding to it"
    );
}

#[tokio::test]
async fn only_the_arbiter_is_told_to_reach_for_a_ruling() {
    // The refusal for rating a point twice in one turn pointed every seat at
    // `rule`. Only the Arbiter carries one, so the other two were being told to
    // call an instrument they cannot reach — the same mistake as listing it in
    // everyone's toolbelt.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, engine, _backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1"})),
            Script::inline(concat!(
                "On reflection, twice.\n",
                r#"{"tool":"vote","args":{"point":1,"verdict":"win"}}"#,
                "\n",
                r#"{"tool":"vote","args":{"point":1,"verdict":"lose"}}"#
            )),
            Script::inline(concat!(
                "Same again, and then I rule.\n",
                r#"{"tool":"vote","args":{"point":1,"verdict":"win"}}"#,
                "\n",
                r#"{"tool":"vote","args":{"point":1,"verdict":"lose"}}"#,
                "\n",
                r#"{"tool":"rule","args":{"point":1,"ruling":"carries","reason":"answered in the record"}}"#
            )),
            Script::text("r"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let refusals: Vec<(String, String)> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ActionRejected { delegate, reason, .. } => {
                Some((delegate.clone(), reason.clone()))
            }
            _ => None,
        })
        .collect();
    let critic = refusals
        .iter()
        .find(|(d, _)| d == "b")
        .expect("the Critic was refused a second rating");
    assert!(
        !critic.1.contains("`rule`"),
        "the Critic was pointed at a tool it does not have: {:?}",
        critic.1
    );
    assert!(
        critic.1.contains("later turn"),
        "and should be pointed at the move it does have: {:?}",
        critic.1
    );

    let arbiter = refusals
        .iter()
        .find(|(d, _)| d == "c")
        .expect("the Arbiter was refused a second rating");
    assert!(
        arbiter.1.contains("`rule`"),
        "the Arbiter is the one seat that should hear about `rule` here: {:?}",
        arbiter.1
    );
}

#[tokio::test]
async fn the_ruling_stays_the_arbiter_alone_and_outweighs_a_vote() {
    // Two things at once, because they pull against each other. Re-voting is now
    // actively encouraged for everyone, so the thing that stops a point moving on
    // the count alone has to be the Arbiter's instrument — and it has to remain
    // the only seat's, or every model grows a tiebreak and the board stops being
    // a count.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let (rec, engine, backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose", "reason": "no"})),
            // The Arbiter rates the point down *and* rules it up in one reply: the
            // two are independent instruments, and this is the case that matters —
            // two seats against, and the record says otherwise.
            Script::inline(concat!(
                "Two seats say it falls. I am not weighing it again; I am ruling on it.\n",
                r#"{"tool":"vote","args":{"point":1,"verdict":"lose","reason":"unanswered"}}"#,
                "\n",
                r#"{"tool":"rule","args":{"point":1,"ruling":"carries","reason":"the objection was answered in the record"}}"#
            )),
            Script::text("r"),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    // The ruling overrode two votes against, and the tally still shows them.
    let last = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::PointUpdated { point, tally, .. } if point.id == 1 => {
                Some((tally.status.clone(), tally.score, tally.losses, point.ruling.is_some()))
            }
            _ => None,
        })
        .next_back()
        .expect("a tally for P1");
    assert_eq!(
        last,
        (PointStatus::Carried, -1, 2, true),
        "the point carries on a ruling while the count still reads minus one"
    );

    // And exactly one seat was ever offered the instrument.
    let holders: Vec<String> = {
        let seen = backend.seen.lock().expect("seen");
        seen.iter()
            .filter(|r| r.tools.iter().any(|t| t["function"]["name"] == "rule"))
            .map(|r| r.model.clone())
            .collect()
    };
    assert_eq!(
        holders,
        vec!["m-c".to_string()],
        "only the Arbiter may rule; holders were {holders:?}"
    );
}

#[tokio::test]
async fn an_operator_note_reaches_every_seat_that_has_not_read_it_yet() {
    // Interjection, end to end: the operator types, and the note lands on the
    // board's record where the next seat's "since your last turn" feed picks it
    // up. Nothing about it is a special channel — it is a record entry like any
    // other, which is why it survives a seat that was mid-turn when it was sent.
    //
    // The Arbiter's turn is stalled so the note is sent at a known point in the
    // debate rather than in a race with a scripted engine.
    use std::sync::atomic::{AtomicBool, Ordering};
    struct HoldsOneSeat {
        scripts: Arc<Script>,
        entered: Arc<AtomicBool>,
        /// One stall only: a backend that hangs every time never reaches round
        /// two, and the point of the test is what round two is handed.
        stalled: Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl ChatBackend for HoldsOneSeat {
        async fn chat(
            &self,
            req: &ChatRequest,
            on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
        ) -> Result<ChatResponse, LlmError> {
            if req.model == "m-c" && !self.stalled.swap(true, Ordering::SeqCst) {
                self.entered.store(true, Ordering::SeqCst);
                std::future::pending().await
            }
            self.scripts.chat(req, on_chunk).await
        }
    }

    let rec = Arc::new(Recorder::default());
    let backend = Arc::new(HoldsOneSeat {
        scripts: Arc::new(Script::new(vec![
            Script::call("make_point", json!({"text": "P1"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::text("unused by a stalled seat"),
            Script::text("r"),
            // Round two: the Advocate speaks again, and the note is new to them.
            Script::call("steelman", json!({"point": 1, "restatement": "strongest form"})),
            Script::call("rebut", json!({"point": 1, "argument": "and yet"})),
            Script::text("r"),
        ])),
        entered: Arc::new(AtomicBool::new(false)),
        stalled: Arc::new(AtomicBool::new(false)),
    });
    let entered = backend.entered.clone();
    let scripts = backend.scripts.clone();
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(backend, sink));

    let driver = {
        let engine = engine.clone();
        tokio::spawn(async move {
            let mut cfg = config();
            cfg.max_rounds = 2;
            engine.run(cfg).await
        })
    };
    for _ in 0..400 {
        if entered.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // Mid-debate: the Advocate and Critic have spoken, the Arbiter has not.
    engine.inject("stop theorising and settle whether P1 stands");
    assert!(engine.skip_turn(), "the stalled turn should be skippable");
    driver.await.expect("engine task");
    drop(engine);

    let seen = prompts_for(&scripts, "m-a");
    assert!(
        seen.contains("stop theorising and settle whether P1 stands"),
        "a seat that speaks again should be handed what it missed: {seen}"
    );
    // A note is a record entry like any other, so it reads as one in the feed
    // rather than arriving as an instruction the model is told to obey.
    assert!(
        seen.contains("operator:"),
        "and it should be attributed rather than anonymous: {seen}"
    );
    // The summarizer reads the record too, so it is handed the same thing.
    let summary_seen = prompts_for(&scripts, "m-s");
    assert!(
        summary_seen.contains("stop theorising"),
        "the Summarizer is a seat as well: {summary_seen}"
    );
}

#[tokio::test]
async fn the_summarizer_can_call_the_board_settled_and_the_ending_says_so() {
    // The engine stops a debate on its own only when every point is decided, and
    // that is a count: it cannot weigh five carried against one deadlocked. The
    // seat that read every round can, and when it does the ending carries its
    // reason rather than "every point decided".
    let mut cfg = config();
    cfg.max_rounds = 4;
    cfg.max_extra_rounds = 0;
    let (rec, engine, _backend, handle) = run_background(
        vec![
            // One entry, two claims: P1 gets weighed, P2 does not, so the
            // engine's own count would keep the debate going for three more
            // rounds and the only thing that can end it is a reading.
            Script::inline(concat!(
                "Two claims.\n",
                r#"{"tool":"make_point","args":{"text":"P1"}}"#,
                "\n",
                r#"{"tool":"make_point","args":{"text":"P2 nobody has reached"}}"#
            )),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            // The Summarizer speaks fourth, so this is the intermission.
            Script::inline(concat!(
                "P1 is settled and P2 is a side issue nobody has reached. Converged.\n",
                r#"{"tool":"settle","args":{"reason":"the motion is settled; only the scope of P2 is open"}}"#
            )),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let settled = rec.events.lock().expect("events").iter().any(|e| {
        matches!(e, EngineEvent::ActionTaken { action, detail, .. }
            if action == "settle" && detail.contains("the motion is settled"))
    });
    assert!(settled, "the Summarizer's own reason should be on the record");

    let verdict = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .find_map(|e| match e {
            EngineEvent::DebateEnded { verdict, .. } => Some(verdict.clone()),
            _ => None,
        })
        .expect("the debate should end with a verdict");
    assert_eq!(verdict.ended_how, EndedHow::CalledSettled);
    assert_eq!(
        verdict.settled_reason.as_deref(),
        Some("the motion is settled; only the scope of P2 is open"),
        "and the ending quotes it rather than describing a count"
    );
    assert!(
        verdict.rounds < 4,
        "the remaining rounds should not have been spent; used {}",
        verdict.rounds
    );
}

#[tokio::test]
async fn one_deadlocked_point_does_not_deny_a_result_the_board_reached() {
    // Five carried, one deadlocked, one withdrawn — and the answer came back
    // "the council could not agree", with the answer itself blanked. Any single
    // undecided point was enough to throw the whole result away, which is not a
    // council failing to agree: it is a live dispute about a side issue beside a
    // result the board reached.
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.closing_statements = false;
    let (rec, engine, _backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1"})),
            Script::call("vote", json!({"point": 1, "verdict": "win"})),
            Script::call("vote", json!({"point": 1, "verdict": "lose"})),
            Script::call("vote", json!({"point": 1, "verdict": "stalemate"})),
            Script::call(
                "conclude",
                json!({
                    "agreed": true,
                    "answer": "circumstances matter; not always bad",
                    "lean": "against a blanket rule"
                }),
            ),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let conclusion = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .find_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => verdict.conclusion.clone(),
            _ => None,
        })
        .expect("a conclusion");
    assert!(
        conclusion.agreed,
        "a point that carried and a point that fell is a result: {conclusion:?}"
    );
    assert_eq!(
        conclusion.answer, "circumstances matter; not always bad",
        "and the answer must survive; it was being blanked"
    );
    assert!(
        !conclusion.tally.contains("withdrawn") || conclusion.tally.contains("carried"),
        "the tally should describe what happened: {}",
        conclusion.tally
    );
}

#[tokio::test]
async fn a_claim_of_consensus_on_a_board_that_decided_nothing_is_still_refused() {
    // The other side of the line. Relaxing the engine's guard must not let the
    // Summarizer narrate agreement out of an untouched board.
    let mut cfg = config();
    cfg.max_rounds = 1;
    cfg.closing_statements = false;
    let (rec, engine, _backend, handle) = run_background(
        vec![
            Script::call("make_point", json!({"text": "P1"})),
            // Nobody votes, so nothing is decided at all.
            Script::text("no"),
            Script::text("no"),
            Script::call(
                "conclude",
                json!({"agreed": true, "answer": "all in favour", "lean": "for"}),
            ),
        ],
        cfg,
    )
    .await;
    handle.await.expect("engine task");
    drop(engine);

    let conclusion = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .find_map(|e| match e {
            EngineEvent::DebateEnded { verdict } => verdict.conclusion.clone(),
            _ => None,
        })
        .expect("a conclusion");
    assert!(
        !conclusion.agreed,
        "nothing was decided, so there is no consensus to report: {conclusion:?}"
    );
    assert!(
        conclusion.answer.is_empty(),
        "and the answer must not stand either: {conclusion:?}"
    );
}

/// A backend that refuses a delegate's first request for exceeding its context
/// window, the way a real provider refuses one, and accepts whatever comes next.
///
/// Refusing by request index rather than anything over a size threshold keeps
/// the test independent of how long the prompts happen to be: the sizes are an
/// implementation detail that shifts whenever the prompt is reworded, and a
/// test asserting on a hard-coded character count would break for that reason
/// rather than because compaction stopped working. What matters is the
/// *relationship*: a refusal arrives, and the next request for the same
/// delegate is smaller than it.
///
/// The refusal has to come late, not first. A fresh delegate holds only the
/// system message, the opening and the current brief, and compaction
/// deliberately trades none of those — refusing the very first request can
/// only ever exercise the nothing-left-to-trade path, which loses the turn by
/// design. Refusing once the delegate has argued for a few rounds gives the
/// resend something real to shorten.
#[derive(Default)]
struct OverflowOnce {
    /// Requests seen per delegate, in order:
    /// `(total chars, chars of the delegate's own assistant replies)`.
    seen: Mutex<std::collections::BTreeMap<String, Vec<(usize, usize)>>>,
    /// Set to have every request refused, for the "still does not fit" case.
    always: bool,
    /// Refuse the k-th request per delegate (0-based), then accept again.
    /// `None` never refuses on its own.
    refuse_at: Option<usize>,
}

impl OverflowOnce {
    fn sizes(&self, who: &str) -> Vec<(usize, usize)> {
        self.seen
            .lock()
            .expect("seen")
            .get(who)
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl ChatBackend for OverflowOnce {
    async fn chat(
        &self,
        req: &ChatRequest,
        _on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
    ) -> Result<ChatResponse, LlmError> {
        let chars: usize = req
            .messages
            .iter()
            .map(|m| m.content.chars().count())
            .sum();
        // The model is the delegate's own: one id per seat in `config()`.
        let who = req.model.clone();
        let mut seen = self.seen.lock().expect("seen");
        let n = seen.entry(who).or_default().len();
        // The second number is what compaction actually promises to shrink: the
        // delegate's own argument. Total size is not it, because compaction trades
        // speeches for round summaries and a summary can be longer than the speech
        // it stands in for.
        let own = req
            .messages
            .iter()
            .filter(|m| m.role == "assistant")
            .map(|m| m.content.chars().count())
            .sum();
        seen.get_mut(&req.model)
            .expect("just inserted")
            .push((chars, own));
        let refuse = self.always || self.refuse_at.is_some_and(|k| n == k);
        drop(seen);
        if refuse {
            return Err(LlmError::Status {
                url: "https://example.invalid/v1/chat/completions".into(),
                status: 400,
                body: format!(
                    "{{\"error\":{{\"message\":\"This model's maximum context length is \
                     {chars} tokens. However, you requested {chars} tokens. Please reduce \
                     the length of the messages.\",\"code\":\"context_length_exceeded\"}}}}"
                ),
            });
        }
        // The arguing seats vote on a point that never exists, so the vote is
        // refused and the turn still counts as acted: exactly one backend call
        // per turn, which is what makes the request counts below exact. A bare
        // sentence would trip the inaction re-prompt and double every count.
        // The summarizer only ever lays rounds out, so it speaks plainly.
        if req.model == "m-s" {
            return Ok(ChatResponse {
                content: "The round is laid out: nothing moved, nothing is buried.".into(),
                ..Default::default()
            });
        }
        Ok(ChatResponse {
            content: "Noted.".into(),
            tool_calls: vec![RawToolCall {
                id: "x".into(),
                name: "vote".into(),
                arguments: json!({"point": 1, "verdict": "win", "reason": "the record stands"}),
            }],
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn an_over_long_prompt_is_compacted_and_resent_rather_than_lost() {
    // The whole point: a 400 for being too long is permanent by is_transient,
    // so the retry loop never fired and the seat used to lose its turn outright.
    // Now the history is shortened and the turn taken again.
    //
    // Five rounds, refused on the fifth: by then each seat holds the system
    // message, the opening, four briefs and four replies, so compaction has
    // older speeches to trade away. Refusing earlier would only prove the
    // nothing-left path, which the next test covers.
    let mut cfg = config();
    cfg.max_rounds = 5;
    let backend = Arc::new(OverflowOnce {
        refuse_at: Some(4),
        ..Default::default()
    });
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    Engine::with_backend(backend.clone(), sink).run(cfg).await;

    // Every arguing seat was refused once and resent having given up some of its
    // own earlier argument.
    //
    // The invariant asserted is the delegate's *own* characters, not the prompt
    // total. Compaction trades speeches for round summaries, so total size can
    // legitimately rise — this test used to assert "the resend is smaller" only
    // because the old code clipped the system prompt and opening, which clawed
    // back more bytes than the summaries cost. That clip was itself the bug: it
    // silently deleted most of the seat's rules on the turn where it was already
    // under pressure. What compaction promises is that the delegate stops carrying
    // its oldest speeches, and that is what is pinned here.
    for model in ["m-a", "m-b", "m-c"] {
        let sizes = backend.sizes(model);
        assert!(
            sizes.len() >= 2,
            "{model} was refused and never retried: {sizes:?}"
        );
        assert!(
            sizes.windows(2).any(|w| w[1].1 < w[0].1),
            "{model}: no resend gave up any of the delegate's own speeches: {sizes:?}"
        );
    }
    // And it is announced rather than suffered quietly.
    let notices: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .any(|n| n.contains("context window") || n.contains("shortened")),
        "compaction must be visible to the operator: {notices:?}"
    );
}

#[tokio::test]
async fn a_prompt_that_still_does_not_fit_loses_the_turn_and_says_so() {
    // Compaction is last-resort, not a loop. When even the shortened prompt is
    // refused, the turn is lost — but the operator is told the context window was
    // the reason rather than being left to infer it from a bare 400.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let backend = Arc::new(OverflowOnce {
        always: true,
        ..Default::default()
    });
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    Engine::with_backend(backend.clone(), sink).run(cfg).await;

    let notices: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .any(|n| n.contains("context window") || n.contains("does not fit")),
        "the operator needs to know the window was the reason: {notices:?}"
    );
    // It must stop rather than compact forever: exactly one retry per delegate.
    for model in ["m-a", "m-b", "m-c"] {
        assert!(
            backend.sizes(model).len() <= 2,
            "{model} was retried without end: {:?}",
            backend.sizes(model)
        );
    }
}

#[tokio::test]
async fn a_malformed_request_is_not_compacted_away() {
    // The false positive that matters: a 400 for a malformed request would, if
    // misread as an over-long prompt, cost the delegate part of its own memory
    // for nothing. RejectUsage already proves this shape for a different field.
    let mut cfg = config();
    cfg.max_rounds = 1;
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let backend = Arc::new(RejectUsage::default());
    Engine::with_backend(backend.clone(), sink).run(cfg).await;
    let notices: Vec<String> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !notices.iter().any(|n| n.contains("context window")),
        "a malformed request must not trigger compaction: {notices:?}"
    );
}

// ---------------------------------------------------------------------------
// The taken seat: a delegate the operator argues, and answers ballots for.
// ---------------------------------------------------------------------------

/// A council where one arguing seat belongs to the operator.
///
/// Two changes only: the seat is marked `human` and its model is blanked. The
/// essentials and the endpoint stay exactly as they were, so the debate is
/// otherwise the same one every other test runs.
fn human_config(seat: &str) -> DebateConfig {
    let mut cfg = config();
    cfg.max_rounds = 1;
    let d = cfg
        .delegates
        .iter_mut()
        .find(|d| d.id == seat)
        .expect("that seat is in the default council");
    d.human = true;
    d.model = String::new();
    d.endpoint_id = String::new();
    cfg
}

/// Answers parked prompts as they arrive, the way the UI does.
///
/// Driven by the event stream rather than by turn order, because that is the
/// only thing the operator actually sees. `turn` shapes the reply, `ballot`
/// the ballot answer; the responder runs until the debate ends.
fn respond(
    engine: Arc<Engine<Script>>,
    rec: Arc<Recorder>,
    turn: Box<dyn Fn(u64) -> HumanTurnSubmit + Send>,
    ballot: Box<dyn Fn(BallotKind) -> HumanBallot + Send>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut acted_turns = std::collections::HashSet::new();
        let mut acted_ballots = std::collections::HashSet::new();
        loop {
            let (mut next_turn, mut next_ballot) = (None, None);
            {
                let events = rec.events.lock().expect("events");
                for e in events.iter() {
                    match e {
                        EngineEvent::YourTurn { turn, delegate, .. } => {
                            let key = (turn.to_string(), delegate.clone());
                            if acted_turns.insert(key) {
                                next_turn = Some(*turn);
                            }
                        }
                        EngineEvent::BallotAsked { kind, delegate, .. } => {
                            if acted_ballots.insert((*kind, delegate.clone())) {
                                next_ballot = Some(*kind);
                            }
                        }
                        EngineEvent::DebateEnded { .. } | EngineEvent::Failed { fatal: true, .. } => {
                            return;
                        }
                        _ => {}
                    }
                }
            }
            if let Some(t) = next_turn {
                // A refusal is the operator's chance to fix it, so this panics
                // rather than retrying: a test that silently resubmits a bad
                // answer would pass for the wrong reason.
                engine
                    .submit_turn(turn(t))
                    .unwrap_or_else(|e| panic!("human turn {t} refused: {e}"));
            }
            if let Some(k) = next_ballot {
                engine
                    .submit_ballot(ballot(k))
                    .unwrap_or_else(|e| panic!("human {k:?} ballot refused: {e}"));
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
}

fn human_turn(text: &str) -> HumanTurnSubmit {
    HumanTurnSubmit {
        text: text.into(),
        thinking: String::new(),
        calls: Vec::new(),
    }
}

fn call_to(name: &str, args: serde_json::Value) -> RawToolCall {
    RawToolCall {
        id: "human".into(),
        name: name.into(),
        arguments: args,
    }
}

/// Stopping a debate the operator is sitting in must skip, not fabricate.
///
/// `await_human_turn` classified cancellation as *answered* and fell through to
/// its empty-response default. So the natural reaction to a prompt you do not want
/// to answer — Stop — produced a full turn attributed to the seat, saying nothing
/// and doing nothing, with a notice about an empty turn. The debate then carried an
/// argument the operator never gave.
#[tokio::test]
async fn stopping_while_parked_on_a_taken_seat_skips_the_turn() {
    let cfg = human_config("a");
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let script = Arc::new(Script::new(vec![]));
    let engine = Arc::new(Engine::with_backend(script.clone(), sink));
    let runner = {
        let engine = engine.clone();
        tokio::spawn(async move { engine.run(cfg).await })
    };
    // Wait until it really is parked, then stop — the same order the operator uses.
    tokio::time::timeout(Duration::from_secs(10), async {
        while engine.pending_prompt().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the seat never parked");
    assert!(
        engine.pending_prompt().is_some(),
        "the seat should be parked waiting for the operator"
    );
    engine.stop();

    tokio::time::timeout(Duration::from_secs(10), runner)
        .await
        .expect("stopping a parked debate must not hang")
        .expect("the debate thread panicked");

    let events = rec.events.lock().expect("events").clone();
    assert!(
        events.iter().any(|e| matches!(e, EngineEvent::TurnSkipped { .. })),
        "the turn should have been skipped, not answered: {events:?}"
    );
    let speeches = rec.speeches();
    assert!(
        !speeches.iter().any(|s| s.delegate == "a"),
        "a stopped seat must not have a speech: {speeches:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            EngineEvent::Notice { text, .. } if text.contains("empty turn")
        )),
        "stopping is not an empty turn, and must not be reported as one"
    );
}

/// The closing address is asked for, not written for you.
///
/// `closing_address` was the one path that never checked `is_human` — the four
/// ballot casts and `take_turn` all did. With closing statements on, a taken seat
/// got a closing address written by whatever model had been configured for it and
/// streamed into the transcript under the operator's name, without ever being asked.
#[tokio::test]
async fn a_taken_seat_is_asked_for_its_closing_address() {
    let mut cfg = human_config("a");
    cfg.closing_statements = true;
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    // Only the model seats' closings are scripted; the taken seat must not need one.
    let script = Arc::new(Script::new(vec![
        Script::call("make_point", json!({"text": "P1 claim"})),
        Script::call("vote", json!({"point": 1, "verdict": "win"})),
        Script::call("vote", json!({"point": 1, "verdict": "lose"})),
        Script::text("round laid out"),
        Script::text("I have nothing further."),
        Script::text("Nor have I."),
        Script::call(
            "conclude",
            json!({"agreed": false, "answer": "undetermined", "lean": "neither"}),
        ),
    ]));
    let engine = Arc::new(Engine::with_backend(script.clone(), sink));
    // Different text per turn, deliberately: with one answer for every prompt the
    // round-1 speech satisfied the closing assertion and the test passed with the
    // closing branch disabled entirely.
    let responder = respond(
        engine.clone(),
        rec.clone(),
        Box::new(|turn| {
            if turn == 1 {
                human_turn("To begin: the motion is arguable but weak.")
            } else {
                human_turn("IN-CLOSING: the motion should be refused.")
            }
        }),
        Box::new(|_| unreachable!("no ballot is opened in this debate")),
    );
    engine.run(cfg).await;
    responder.abort();

    let speeches = rec.speeches();
    assert!(
        speeches
            .iter()
            .any(|s| s.delegate == "a" && s.text.contains("IN-CLOSING")),
        "the operator's own closing address is missing: {speeches:?}"
    );
    assert!(
        speeches
            .iter()
            .any(|s| s.delegate == "a" && s.text.contains("To begin")),
        "precondition: the seat's first turn should still be there"
    );
    let asked: Vec<String> = script
        .seen
        .lock()
        .expect("seen")
        .iter()
        .map(|r| r.model.clone())
        .collect();
    assert!(
        !asked.iter().any(|m| m == "m-a"),
        "the taken seat's closing came from a model: {asked:?}"
    );
}

#[tokio::test]
async fn a_taken_seat_argues_its_own_turn_and_the_record_shows_it() {
    // The whole feature in one test: the seat is marked human, the engine
    // parks on it rather than calling a model, the speech lands on the
    // transcript, and the debate carries on to the seats that are not.
    let cfg = human_config("a");
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let script = Arc::new(Script::new(vec![]));
    let engine = Arc::new(Engine::with_backend(script.clone(), sink));
    let responder = respond(
        engine.clone(),
        rec.clone(),
        Box::new(|_| human_turn("I argue the motion, and I raise a point.")),
        Box::new(|_| unreachable!("no ballot is opened in this debate")),
    );
    engine.run(cfg).await;
    responder.abort();

    let speeches = rec.speeches();
    assert!(
        speeches.iter().any(|s| s.delegate == "a" && s.text.contains("I argue the motion")),
        "the operator's speech is missing: {speeches:?}"
    );
    // The models were never asked to argue for it: only the other two seats
    // reached the backend, and `m-a` reached it never.
    let asked: Vec<String> = script
        .seen
        .lock()
        .expect("seen")
        .iter()
        .map(|r| r.model.clone())
        .collect();
    assert!(
        !asked.iter().any(|m| m == "m-a"),
        "the taken seat reached the backend: {asked:?}"
    );
    assert!(
        asked.iter().any(|m| m == "m-b") && asked.iter().any(|m| m == "m-c"),
        "the seats that are not taken should have argued: {asked:?}"
    );
    assert!(
        rec.kinds().contains(&"yourTurn"),
        "the turn must be announced as the operator's, not as a model's"
    );
    assert!(
        rec.kinds().contains(&"ended"),
        "the debate must finish on its own: {:?}",
        rec.kinds()
    );
}

#[tokio::test]
async fn a_taken_seat_acts_on_the_board_with_the_pickers_calls() {
    // The action picker emits ordinary tool calls, so they travel the same
    // validation a model's do. Two actions here: raise a point, then vote on it.
    let cfg = human_config("b");
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(Arc::new(Script::new(vec![])), sink));
    let responder = respond(
        engine.clone(),
        rec.clone(),
        Box::new(|_| HumanTurnSubmit {
            text: "A claim, and my vote on it.".into(),
            thinking: String::new(),
            calls: vec![
                call_to("make_point", json!({ "text": "Lilies are cheaper per stem" })),
                call_to("vote", json!({ "point": 1, "verdict": "win", "reason": "it is true" })),
            ],
        }),
        Box::new(|_| unreachable!("no ballot is opened in this debate")),
    );
    engine.run(cfg).await;
    responder.abort();

    // The board accepted both calls and the vote landed on the point: the claim
    // carries, 1 up and 0 down.
    assert_eq!(rec.final_board(), vec![(1, "carried", 1)], "the point or vote is missing");
    let texts: Vec<String> = rec.speeches().iter().map(|s| s.text.clone()).collect();
    assert!(
        texts.iter().any(|t| t.contains("A claim, and my vote on it.")),
        "the operator's speech is missing: {texts:?}"
    );
}

#[tokio::test]
async fn a_bad_call_is_refused_so_it_can_be_fixed_rather_than_lost() {
    // The difference from a model: a mistyped vote is *rejected at submit*, so
    // the operator sees why and can correct it. A model's is dropped. Here the
    // refusal is the whole assertion — the turn must still be waiting.
    let cfg = human_config("a");
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(Arc::new(Script::new(vec![])), sink));

    // Wait for the prompt, then submit three things that must all be refused.
    let bad = engine.clone();
    let rec2 = rec.clone();
    tokio::spawn(async move {
        loop {
            let parked = rec2
                .events
                .lock()
                .expect("events")
                .iter()
                .any(|e| matches!(e, EngineEvent::YourTurn { .. }));
            if parked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        // A point that does not exist.
        let why = bad
            .submit_turn(HumanTurnSubmit {
                text: "voting into the void".into(),
                thinking: String::new(),
                calls: vec![call_to("vote", json!({ "point": 7, "verdict": "win" }))],
            })
            .expect_err("a vote on a point that does not exist must be refused");
        assert!(why.contains("no point 7"), "{why}");
        // An empty turn.
        let why = bad
            .submit_turn(human_turn("   "))
            .expect_err("an empty turn must be refused");
        assert!(why.contains("empty turn"), "{why}");
        // A call the parser rejects outright.
        let why = bad
            .submit_turn(HumanTurnSubmit {
                text: "bad argument".into(),
                thinking: String::new(),
                calls: vec![call_to("not_a_tool", json!({}))],
            })
            .expect_err("an unknown tool must be refused");
        assert!(why.contains("not_a_tool"), "{why}");
        // All three refused, so the turn is still the operator's to answer.
        let pending = engine_pending(&bad);
        assert!(
            matches!(pending, Some(PendingPrompt::Turn { .. })),
            "a refused submit must leave the turn waiting: {pending:?}"
        );
        // And the corrected answer is accepted.
        bad.submit_turn(HumanTurnSubmit {
            text: "Corrected: no claim to make, only a position.".into(),
            thinking: String::new(),
            calls: vec![],
        })
        .expect("a valid submit is accepted");
    });

    // Nothing will answer a ballot, so end the debate as soon as the turn is in.
    let stopper = engine.clone();
    tokio::spawn(async move {
        loop {
            if stopper.pending_prompt().is_some() {
                tokio::time::sleep(Duration::from_millis(60)).await;
                stopper.stop();
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    engine.run(cfg).await;

    let texts: Vec<String> = rec.speeches().iter().map(|s| s.text.clone()).collect();
    assert!(
        texts.iter().any(|t| t.contains("Corrected:")),
        "the corrected turn should be the one on the record: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("bad argument")),
        "a refused submit must never reach the record: {texts:?}"
    );
}

#[tokio::test]
async fn a_taken_turn_can_be_skipped_like_any_other() {
    // Skip must reach a parked turn exactly as it reaches a request in flight:
    // the round moves on, the seat loses its turn, and the debate ends.
    let cfg = human_config("a");
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(Arc::new(Script::new(vec![])), sink));
    let skipper = {
        let engine = engine.clone();
        let rec = rec.clone();
        tokio::spawn(async move {
            loop {
                let parked = rec
                    .events
                    .lock()
                    .expect("events")
                    .iter()
                    .any(|e| matches!(e, EngineEvent::YourTurn { .. }));
                if parked {
                    assert!(engine.skip_turn(), "skip found no turn in flight");
                    return;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
    };
    engine.run(cfg).await;
    skipper.await.expect("skipper");

    let kinds = rec.kinds();
    assert!(kinds.contains(&"skipped"), "the skip was not recorded: {kinds:?}");
    assert!(kinds.contains(&"ended"), "the debate did not finish: {kinds:?}");
    // And nothing was recorded as said by the skipped seat.
    assert!(
        !rec
            .speeches()
            .iter()
            .any(|s| s.delegate == "a" && !s.text.is_empty()),
        "a skipped turn put words on the record"
    );
}

#[tokio::test]
async fn a_taken_seat_answers_its_own_poll() {
    // A poll opened by a model seat reaches the operator's seat as a question,
    // and their answer is counted like any other ballot.
    let cfg = human_config("a");
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    // The Critic opens the poll; the operator's seat votes second.
    let script = vec![
        Script::call(
            "poll",
            json!({ "question": "Does the board help?", "options": ["yes", "no"] }),
        ),
    ];
    let engine = Arc::new(Engine::with_backend(Arc::new(Script::new(script)), sink));
    let responder = respond(
        engine.clone(),
        rec.clone(),
        Box::new(|_| human_turn("I hold my position.")),
        Box::new(|kind| {
            assert_eq!(kind, BallotKind::Poll, "unexpected ballot kind");
            HumanBallot::Poll {
                choice: "no".into(),
                reason: Some("the board records, it does not decide".into()),
                confidence: Some(0.7),
            }
        }),
    );
    engine.run(cfg).await;
    responder.abort();

    let ballots: Vec<(String, String)> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::PollBallotCast { delegate, choice, .. } => Some((delegate.clone(), choice.clone())),
            _ => None,
        })
        .collect();
    assert!(
        ballots.iter().any(|(d, c)| d == "a" && c == "no"),
        "the operator's ballot was not counted: {ballots:?}"
    );
}

#[tokio::test]
async fn a_taken_seat_answers_the_deadlock_review() {
    // The reading is what matters here: the operator's vote on the reading is
    // recorded, and a changed vote on the point rides the same answer.
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    // Two models split P1 into a stalemate; the Critic opens the review in round
    // two and the operator's seat is asked to rule on why. The script is
    // written out turn by turn — including the summarizer's intermission, which
    // is a request like any other — because a test that assumed only the
    // arguing seats consumed replies would silently vote the summarizer's turn
    // with somebody else's reply.
    let mut cfg = human_config("a");
    cfg.max_rounds = 2;
    let script = vec![
        // Round one: the taken seat answers (no reply consumed), then Critic
        // raises the claim and Arbiter votes against it — 1 up, 1 down.
        Script::call("make_point", json!({ "text": "Lilies are cheaper per stem" })),
        Script::call("vote", json!({ "point": 1, "verdict": "lose" })),
        // The intermission.
        Script::text("Round one laid out: one claim, split down the middle."),
        // Round two: the Critic opens the review. It asks the seats that may
        // vote, and the taken seat comes first in roster order, so it is asked
        // before the Arbiter's reply below.
        Script::call("review", json!({ "point": 1 })),
        Script::call(
            "stalemate_ballot",
            json!({ "reading": "decidable", "reason": "one more argument settles it" }),
        ),
    ];
    let engine = Arc::new(Engine::with_backend(Arc::new(Script::new(script)), sink));
    let responder = respond(
        engine.clone(),
        rec.clone(),
        Box::new(|_| human_turn("I rule on the deadlock.")),
        Box::new(|kind| {
            assert_eq!(kind, BallotKind::Review);
            HumanBallot::Review {
                reading: StalemateReading::Decidable,
                reason: Some("one more argument would settle it".into()),
                vote: Some(Verdict::Win),
            }
        }),
    );
    engine.run(cfg).await;
    responder.abort();

    let ballots: Vec<(String, String)> = rec
        .events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            EngineEvent::ReviewBallotCast { delegate, reading, .. } => {
                Some((delegate.clone(), format!("{reading:?}")))
            }
            _ => None,
        })
        .collect();
    assert!(
        ballots.iter().any(|(d, r)| d == "a" && r == "Decidable"),
        "the operator's reading was not counted: {ballots:?}"
    );
    // The vote carried in the same breath applied to the point. A review-ballot
    // change is not an action on a turn, so it has no `actionTaken` of its own:
    // it shows on the tally. The point was split 1-1; the operator's change to
    // win with the Arbiter's reading makes it 2 up, 1 down.
    assert_eq!(
        rec.final_board(),
        vec![(1, "carried", 1)],
        "the vote carried with the reading was not applied to the point"
    );
}

#[tokio::test]
async fn a_taken_seat_is_never_asked_as_judge_or_summarizer() {
    // The rule is enforced by validation, not by the UI: a judge or summarizer
    // marked taken is refused before any request is made.
    for role in [SeatRole::Judge, SeatRole::Summarizer] {
        let mut cfg = config();
        let d = cfg
            .delegates
            .iter_mut()
            .find(|d| d.seat_role == role)
            .expect("the council has that role");
        d.human = true;
        let rec = Arc::new(Recorder::default());
        let sink: Arc<dyn EventSink> = {
            let rec = rec.clone();
            Arc::new(move |e: EngineEvent| rec.emit(e))
        };
        Engine::with_backend(Arc::new(Script::new(vec![])), sink).run(cfg).await;
        let failures = rec.failures();
        assert!(
            failures.iter().any(|f| f.contains("only arguing seats can be taken")),
            "{role:?} was allowed: {failures:?}"
        );
        // Refused before any turn began: the only event is the refusal itself.
        let kinds = rec.kinds();
        assert!(
            !kinds.iter().any(|k| k.contains(&"turn") || k.contains("speech")),
            "{role:?} was refused but the council still ran: {kinds:?}"
        );
    }
}

#[tokio::test]
async fn a_taken_seat_needs_no_model_or_endpoint() {
    // The converse: marking a seat taken is what makes the model requirement
    // go away. A blank model on a *model* seat is still refused, so the flag is
    // doing the work rather than the field merely being ignored.
    let mut blank = config();
    blank.max_rounds = 1;
    let d = blank.delegates.iter_mut().find(|d| d.id == "a").expect("a seat");
    d.model = String::new();
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    Engine::with_backend(Arc::new(Script::new(vec![])), sink)
        .run(blank)
        .await;
    assert!(
        rec.failures()
            .iter()
            .any(|f| f.contains("no model selected")),
        "a blank model on a model seat must still be refused: {:?}",
        rec.failures()
    );
}

#[tokio::test]
async fn a_reload_during_a_taken_turn_carries_the_pending_prompt() {
    // The engine is parked waiting, so a snapshot must say so: otherwise a
    // reload shows an empty chamber while the debate runs on behind it, and
    // the operator never learns the council is waiting for them.
    let cfg = human_config("a");
    let rec = Arc::new(Recorder::default());
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    let engine = Arc::new(Engine::with_backend(Arc::new(Script::new(vec![])), sink));
    // The snapshot is taken *while* the turn is parked, which is the only
    // moment it matters: stopping the debate releases the prompt, so a
    // snapshot afterwards would correctly report nothing pending.
    let watcher = {
        let engine = engine.clone();
        tokio::spawn(async move {
            loop {
                if engine.pending_prompt().is_some() {
                    return engine.snapshot();
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
    };
    let stopper = {
        let engine = engine.clone();
        tokio::spawn(async move {
            loop {
                if engine.pending_prompt().is_some() {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    engine.stop();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };
    engine.run(cfg).await;

    let snap = watcher.await.expect("watcher");
    match &snap.pending {
        Some(PendingPrompt::Turn { delegate, .. }) => assert_eq!(delegate, "a", "{snap:?}"),
        other => panic!("the snapshot did not carry the pending turn: {other:?}"),
    }
    // And the debate is still running while it waits: a remount that adopts
    // this snapshot sees an unfinished council, not a finished one.
    assert!(!snap.finished, "a parked debate read as finished");
    stopper.abort();
}

/// The engine's pending prompt, read through the public API.
fn engine_pending(engine: &Engine<Script>) -> Option<PendingPrompt> {
    engine.pending_prompt()
}

/// A local model that starts repeating itself on the third turn.
///
/// Every local model but one started looping around the third turn of round one,
/// and it was not the models: it was the shape of what we were sending.
///
/// `prune_superseded_briefs` drops the user turn that asked the question, and
/// `finish_turn` pushes the delegate's reply after it — so from turn three the
/// history was `system, user, assistant, assistant, user`. Two assistants in a
/// row is not a shape any chat template is written for, and it is worst when
/// both are empty, which is what a model that only made tool calls leaves behind.
///
/// The engine's own state machine was never wrong, so every test above passed
/// with a request no local server would render sanely. This one looks at the
/// wire shape, which is the only place this failure is visible.
#[tokio::test]
async fn a_delegate_history_never_puts_two_assistant_turns_in_a_row() {
    // The shape only appears from a seat's *third* turn: turn 1 ends
    // `[system, opening, brief1, a1]`, turn 2 ends `[system, opening, a1, brief2]`
    // because the prune removed brief1, and turn 3 ends `[system, opening, a1, a2,
    // brief3]` — a1 and a2 adjacent. So this needs at least three rounds and a
    // seat that speaks in all of them. `max_rounds = 1` here made the old version
    // of this test vacuous: every seat spoke once, so the run could not form and
    // the assertion could not fail. Three rounds, and the debate must reach them.
    let mut cfg = config();
    cfg.max_rounds = 3;
    cfg.closing_statements = false;
    // Enough script for every seat to speak in all three rounds plus the
    // confirmation ballots those votes trigger. The engine drains the script in
    // order, so the tail being generous costs nothing.
    let mut script: Vec<ChatResponse> = Vec::new();
    for round in 1..=3u32 {
        // A tool-only reply: `finish_turn` writes no prose for it, so the
        // history grows an *empty* assistant message. Two of those adjacent is
        // the degenerate case the models actually fell apart on.
        script.push(Script::call(
            "rebut",
            json!({ "argument": format!("round {round}: the claim is unfalsifiable as stated") }),
        ));
        script.push(Script::text(&format!(
            "Round {round}: I stand by the record as it stands."
        )));
    }
    let rec = Arc::new(Recorder::default());
    let backend = Arc::new(Script::new(script));
    let sink: Arc<dyn EventSink> = {
        let rec = rec.clone();
        Arc::new(move |e: EngineEvent| rec.emit(e))
    };
    Engine::with_backend(backend.clone(), sink).run(cfg).await;

    // Count how many *speaking* requests each seat made. The bug needs a run of
    // assistants, which needs a seat on its third turn, so the test must first
    // establish that a seat really did speak three times — otherwise this is a
    // test that passes because nothing happened.
    let seen = backend.seen.lock().expect("seen");
    let mut speeches_per_seat: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for req in seen.iter() {
        // A request carrying a turn brief is a speaking turn; a ballot has none.
        if req.messages
            .iter()
            .any(|m| m.role == "user" && m.content.contains("You have the floor"))
        {
            *speeches_per_seat.entry(req.model.clone()).or_default() += 1;
        }
    }
    let deepest = speeches_per_seat.values().copied().max().unwrap_or(0);
    assert!(
        deepest >= 3,
        "this test only means something if a seat speaks on its third turn; deepest seat spoke {deepest} times ({speeches_per_seat:?})"
    );

    let mut checked = 0;
    for req in seen.iter() {
        let roles: Vec<&str> = req.messages.iter().map(|m| m.role.as_str()).collect();
        for (i, pair) in roles.windows(2).enumerate() {
            assert!(
                !(pair[0] == "assistant" && pair[1] == "assistant"),
                "{} was sent two assistant turns in a row on request {i}: {roles:?}",
                req.model
            );
        }
        // The specific degenerate case: an empty assistant must never be sent at
        // all. Anthropic 400s on it, and a template sees "you said nothing".
        for (i, m) in req.messages.iter().enumerate() {
            if m.role == "assistant" {
                assert!(
                    !m.content.trim().is_empty(),
                    "{} was sent an empty assistant message at {i}: {roles:?}",
                    req.model
                );
            }
        }
        checked += 1;
    }
    assert!(checked >= 5, "expected several requests to inspect, got {checked}");
}
