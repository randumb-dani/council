//! Structured decisions without prose.
//!
//! A poll ballot is classification, not deliberation: pick one of N options,
//! optionally with a reason. Burning a full LLM turn per voter for that is the
//! wrong tool — a decision model (TypeSafe's Jev and its kin) answers a typed
//! question with a choice plus a calibrated confidence in one pass, without
//! generating text. This module speaks to such models; votes stay with the
//! seats, because a vote needs a reason and an explanation, which is exactly
//! what decision models do not give.
//!
//! Jev-style APIs are young and their wire shapes vary, so both the request
//! and the parsing are tolerant: several response shapes are accepted, and
//! anything unparseable is an error the engine treats as "fall back to the
//! seat's own model", never as a silent wrong answer.

use crate::client::LlmError;
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ClassifyRequest {
    pub question: String,
    pub options: Vec<String>,
    /// Whatever the decider needs to ground the choice: board plus record.
    pub context: String,
    pub voter: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub choice: String,
    pub confidence: Option<f64>,
}

#[async_trait]
pub trait DecisionBackend: Send + Sync + 'static {
    async fn classify(&self, req: &ClassifyRequest) -> Result<Decision, LlmError>;
}

/// Snap a model's paraphrase back onto a real option, as with ballots.
pub fn snap_option(raw: &str, options: &[String]) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    options
        .iter()
        .find(|o| {
            o.eq_ignore_ascii_case(trimmed)
                || o.to_ascii_lowercase().contains(&trimmed.to_ascii_lowercase())
                || trimmed.to_ascii_lowercase().contains(&o.to_ascii_lowercase())
        })
        .cloned()
}

pub struct JevBackend {
    base_url: String,
    api_key: Option<String>,
    model: String,
    timeout_secs: u64,
}

impl JevBackend {
    pub fn new(
        base_url: String,
        api_key: Option<String>,
        model: String,
        timeout_secs: u64,
    ) -> Self {
        Self {
            base_url,
            api_key,
            model,
            timeout_secs,
        }
    }

    fn urls(&self) -> Vec<String> {
        let base = self.base_url.trim().trim_end_matches('/').to_string();
        vec![format!("{base}/v1/decide"), format!("{base}/decide")]
    }

    /// Read a decision out of whatever shape came back: a choice field, a
    /// probability map, or an OpenAI-style message naming an option.
    pub fn parse_decision(v: &Value, options: &[String]) -> Option<Decision> {
        if let Some(map) = v.get("probabilities").and_then(Value::as_object) {
            let mut best: Option<(&String, f64)> = None;
            for (k, p) in map {
                if let Some(p) = p.as_f64() {
                    if best.map(|(_, b)| p > b).unwrap_or(true) {
                        best = Some((k, p));
                    }
                }
            }
            if let Some((k, p)) = best {
                return snap_option(k, options).map(|choice| Decision {
                    choice,
                    confidence: (0.0..=1.0).contains(&p).then_some(p),
                });
            }
        }
        let raw = ["choice", "label", "decision", "answer"]
            .iter()
            .filter_map(|k| v.get(k).and_then(Value::as_str))
            .next()
            .or_else(|| v["choices"][0]["message"]["content"].as_str())?;
        let choice = snap_option(raw, options)?;
        let confidence = ["confidence", "probability", "score"]
            .iter()
            .filter_map(|k| v.get(k).and_then(Value::as_f64))
            .next()
            .filter(|c| (0.0..=1.0).contains(c));
        Some(Decision {
            choice,
            confidence,
        })
    }
}

#[async_trait]
impl DecisionBackend for JevBackend {
    async fn classify(&self, req: &ClassifyRequest) -> Result<Decision, LlmError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| LlmError::Stream(format!("could not build an HTTP client: {e}")))?;
        let mut last: Option<LlmError> = None;
        for url in self.urls() {
            let mut rb = client
                .post(&url)
                .timeout(Duration::from_secs(req_timeout(self.timeout_secs)))
                .json(&serde_json::json!({
                    "model": self.model,
                    "question": req.question,
                    "options": req.options,
                    "input": req.context,
                    "voter": req.voter,
                }));
            if let Some(key) = self.api_key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
                rb = if let Some(bare) = key.strip_prefix("Bearer ") {
                    rb.header(reqwest::header::AUTHORIZATION, format!("Bearer {bare}"))
                } else {
                    rb.bearer_auth(key)
                };
            }
            let res = match rb.send().await {
                Ok(r) => r,
                Err(source) => {
                    last = Some(LlmError::Transport {
                        url: url.clone(),
                        source,
                    });
                    continue;
                }
            };
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            if !status.is_success() {
                let e = LlmError::Status {
                    url: url.clone(),
                    status: status.as_u16(),
                    body: truncate(&body, 400),
                };
                if status.as_u16() == 404 {
                    last = Some(e);
                    continue;
                }
                return Err(e);
            }
            let v: Value =
                serde_json::from_str(&body).map_err(|e| LlmError::Stream(format!("{e}")))?;
            match Self::parse_decision(&v, &req.options) {
                Some(d) => return Ok(d),
                None => {
                    return Err(LlmError::Stream(format!(
                        "the decision named nothing on the ballot: {}",
                        truncate(&body, 200)
                    )));
                }
            }
        }
        Err(last.unwrap_or(LlmError::Stream("no decision url to try".into())))
    }
}

fn req_timeout(t: u64) -> u64 {
    t.clamp(5, 120)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let head: String = s.chars().take(n).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn opts() -> Vec<String> {
        vec!["roses".into(), "lilies".into()]
    }

    #[test]
    fn reads_a_choice_with_confidence() {
        let d = JevBackend::parse_decision(
            &json!({"choice": "roses", "confidence": 0.93}),
            &opts(),
        )
        .expect("decision");
        assert_eq!(d.choice, "roses");
        assert_eq!(d.confidence, Some(0.93));
    }

    #[test]
    fn tolerates_key_variants_and_paraphrase() {
        let d = JevBackend::parse_decision(&json!({"label": "I pick Lilies"}), &opts())
            .expect("decision");
        assert_eq!(d.choice, "lilies");
        assert_eq!(d.confidence, None);
    }

    #[test]
    fn reads_a_probability_map_by_argmax() {
        let d = JevBackend::parse_decision(
            &json!({"probabilities": {"roses": 0.2, "lilies": 0.8}}),
            &opts(),
        )
        .expect("decision");
        assert_eq!(d.choice, "lilies");
        assert_eq!(d.confidence, Some(0.8));
    }

    #[test]
    fn falls_back_to_an_openai_style_message() {
        let d = JevBackend::parse_decision(
            &json!({"choices": [{"message": {"content": "Roses, final answer"}}]}),
            &opts(),
        )
        .expect("decision");
        assert_eq!(d.choice, "roses");
    }

    #[test]
    fn rejects_answers_off_the_ballot() {
        assert!(JevBackend::parse_decision(&json!({"choice": "tulips"}), &opts()).is_none());
        assert!(JevBackend::parse_decision(&json!({}), &opts()).is_none());
    }

    #[test]
    fn confidence_outside_unit_range_is_dropped_not_trusted() {
        let d = JevBackend::parse_decision(
            &json!({"choice": "roses", "confidence": 7.5}),
            &opts(),
        )
        .expect("decision");
        assert_eq!(d.confidence, None);
    }
}
