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
///
/// Three rules, and each of them exists because its absence inverted a vote:
///
/// 1. **Exact match first**, case- and whitespace-insensitively. Only if nothing
///    matches exactly do we look for a paraphrase.
/// 2. **Never accept an option that is merely a substring of a longer answer word.**
///    The old check was bidirectional, so with options `["supported","unsupported"]`
///    and the answer `"unsupported"`, the *first* option matched — because
///    `"unsupported".contains("supported")` is true — and a binary classifier voted
///    the wrong way. Same failure for `["yes","no"]` against `"none of the above"`.
///    This is why the confidence came back unchanged too: the probability map
///    argmaxed to `"unsupported"` and this re-snapped it to `"supported"` with the
///    original 0.6 attached.
/// 3. **When several options match, take the longest.** Short options are the ones
///    that tend to be substrings of the others.
pub fn snap_option(raw: &str, options: &[String]) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let answer = trimmed.to_ascii_lowercase();

    if let Some(exact) = options
        .iter()
        .find(|o| o.trim().to_ascii_lowercase() == answer)
    {
        return Some(exact.clone());
    }

    options
        .iter()
        .filter(|o| {
            let option = o.trim().to_ascii_lowercase();
            if option.is_empty() {
                return false;
            }
            // The option may be a phrase the answer contains (`"carry"`, `"strike"`),
            // but only on a word boundary, and only when it is not just a fragment of
            // a longer word inside the answer.
            let mut from = 0usize;
            while let Some(at) = answer[from..].find(&option) {
                let start = from + at;
                let end = start + option.len();
                let before_ok = start == 0
                    || !answer[..start]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_ascii_alphanumeric());
                let after_ok = end == answer.len()
                    || !answer[end..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric());
                if before_ok && after_ok {
                    return true;
                }
                from = end.max(start + 1);
            }
            false
        })
        // Longest wins: "not supported" beats "supported" when both could match.
        .max_by_key(|o| o.trim().len())
        .cloned()
}

pub struct JevBackend {
    base_url: String,
    api_key: Option<String>,
    model: String,
    timeout_secs: u64,
    /// One pooled client for the backend's life, like `HttpBackend.client`:
    /// the old code built (and dropped, with its connection pool) a client
    /// per `classify` call.
    client: reqwest::Client,
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
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
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
        let client = self.client.clone();
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
    if s.len() <= n {
        // At most `n` bytes, so at most `n` chars: already fits.
        return s.to_string();
    }
    // `nth(n)` is where char number `n` (0-indexed) starts, so `s[..at]` is
    // the first `n` chars — byte-identical to `chars().take(n).collect()` —
    // found in a single partial scan instead of a count plus a take. `None`
    // means multibyte input with at most `n` chars despite the byte length.
    match s.char_indices().nth(n).map(|(i, _)| i) {
        Some(at) => format!("{head}…", head = &s[..at]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn opts() -> Vec<String> {
        vec!["roses".into(), "lilies".into()]
    }

    /// A binary classifier must not vote the wrong way.
    ///
    /// The old matcher accepted an option that was a substring of the answer and took
    /// the *first* match, so `"unsupported"` snapped to `"supported"` — a reversed
    /// vote, with the model's own 0.6 confidence still attached, and nothing logged.
    #[test]
    fn an_option_that_is_a_fragment_of_another_word_never_wins() {
        let pair = vec!["supported".to_string(), "unsupported".to_string()];
        assert_eq!(
            snap_option("unsupported", &pair).as_deref(),
            Some("unsupported"),
            "the longer, exact option must win"
        );
        assert_eq!(snap_option("supported", &pair).as_deref(), Some("supported"));

        // `yes`/`no` against a "none of the above" answer.
        let yn = vec!["yes".to_string(), "no".to_string()];
        assert_ne!(
            snap_option("none of the above", &yn).as_deref(),
            Some("yes"),
            "`no` inside `none` is not the option `no`"
        );

        // And the same shape through a probability map, which is where the inverted
        // confidence did the most damage.
        let d = JevBackend::parse_decision(
            &json!({"probabilities": {"unsupported": 0.6, "supported": 0.4}}),
            &pair,
        );
        assert_eq!(
            d.as_ref().map(|d| d.choice.as_str()),
            Some("unsupported"),
            "the argmax must survive the snap"
        );
        assert!(
            d.and_then(|d| d.confidence).unwrap_or(0.0) > 0.5,
            "and keep its own confidence, not the other option's"
        );
    }

    #[test]
    fn an_exact_match_beats_a_phrase_match() {
        // "strike" is inside "strike the motion", but a seat that answered exactly
        // "strike" meant the option, not a longer phrase.
        let options = vec![
            "strike the motion".to_string(),
            "strike".to_string(),
        ];
        assert_eq!(snap_option("strike", &options).as_deref(), Some("strike"));
        assert_eq!(
            snap_option("we should strike the motion", &options).as_deref(),
            Some("strike the motion")
        );
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

    /// `truncate` on multibyte input: same head as `chars().take(n)`, and no
    /// split characters — including input that fits by chars but not bytes.
    #[test]
    fn truncate_never_splits_multibyte_input() {
        let s = format!("{}{}", "漢".repeat(50), "x".repeat(500));
        let old = if s.chars().count() <= 400 {
            s.clone()
        } else {
            format!("{}…", s.chars().take(400).collect::<String>())
        };
        let got = truncate(&s, 400);
        assert_eq!(got, old);
        assert_eq!(got.as_bytes(), old.as_bytes(), "byte output must match");
        assert!(!got.contains('�'), "a char was split: {got:?}");
        // Fits by chars (5) but not by bytes (20): untouched.
        let emoji = "🎉".repeat(5);
        assert_eq!(truncate(&emoji, 5), emoji);
        assert_eq!(truncate("abc", 10), "abc");
    }
}
