//! A thin OpenAI-compatible client: one POST to `/chat/completions`, SSE
//! streaming, and tolerant accumulation of partial tool-call arguments.
//!
//! Deliberately no vendor-specific code — this is expected to work unchanged
//! against Unsloth Studio, llama.cpp, vLLM, Ollama's compat endpoint, LM Studio,
//! OpenRouter, or OpenAI itself.

use crate::tools::RawToolCall;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// Data URLs attached to this message, forwarded as `image_url` parts.
    /// Empty for every message but the opening one. Skipped in serialization:
    /// `build_body` renders the wire shape by hand so a text-only message
    /// keeps exactly the shape it always had.
    #[serde(skip_serializing, default)]
    pub images: Vec<String>,
    /// The individual replies behind a folded run.
    ///
    /// `normalize_history` merges adjacent assistant messages so a chat template
    /// sees strict alternation. That is the right shape for the model and the
    /// wrong shape for compaction, which gives up the delegate's *turns* oldest
    /// first: a seat that has spoken four times looks like it has spoken once, and
    /// with only one candidate left every one of those turns is "recent" and
    /// therefore untouchable — so a prompt that genuinely does not fit gets
    /// "nothing left to shorten" and the turn is lost.
    ///
    /// So the parts are kept here and `compact_history` re-expands them before it
    /// reasons about what to drop. Empty for anything that is not a merged run.
    #[serde(skip_serializing, default)]
    pub merged: Vec<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
            images: Vec::new(),
            merged: Vec::new(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
            images: Vec::new(),
            merged: Vec::new(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
            images: Vec::new(),
            merged: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub endpoint: crate::types::EndpointConfig,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    /// OpenAI `tools` array. Empty when the endpoint has native tools off.
    pub tools: Vec<Value>,
    pub tool_choice: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

/// What a turn cost, as the server counted it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

impl Usage {
    pub fn total(&self) -> u32 {
        self.prompt_tokens.saturating_add(self.completion_tokens)
    }
}

/// Read usage out of a response object, tolerating the field names different
/// servers use. Returns None rather than a zero: a missing count is not the
/// same as a free turn, and a meter that silently reads zero is worse than one
/// that admits it does not know.
pub(crate) fn parse_usage(v: &Value) -> Option<Usage> {
    let u = v.get("usage")?;
    if u.is_null() {
        return None;
    }
    let pick = |keys: &[&str]| -> u32 {
        for k in keys {
            if let Some(n) = u.get(*k).and_then(|x| x.as_u64()) {
                return n.min(u32::MAX as u64) as u32;
            }
        }
        0
    };
    let prompt = pick(&["prompt_tokens", "input_tokens", "promptTokens", "inputTokens"]);
    let completion = pick(&[
        "completion_tokens",
        "output_tokens",
        "completionTokens",
        "outputTokens",
    ]);
    if prompt == 0 && completion == 0 {
        return None;
    }
    Some(Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
    })
}

#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub finish_reason: Option<String>,
    pub content: String,
    /// The model's private thinking, when the server exposes it (DeepSeek's
    /// `reasoning_content`, OpenRouter's `reasoning`, and friends). Shown in
    /// the UI, never fed back into anyone's history.
    pub reasoning: String,
    pub tool_calls: Vec<RawToolCall>,
    /// Non-fatal observations (a stream fallback, a retried URL). The engine
    /// surfaces these as notices so a slow turn explains itself.
    pub notes: Vec<String>,
    /// Token counts, when the server reported them and the endpoint asked.
    pub usage: Option<Usage>,
}

/// One piece of a streaming reply. Content and thinking arrive on different
/// keys depending on the server, and the UI renders them very differently, so
/// they stay distinct all the way down.
#[derive(Debug, Clone, Copy)]
pub enum ChatChunk<'a> {
    Content(&'a str),
    Reasoning(&'a str),
    /// The model has started emitting a tool call. The name only — the
    /// arguments arrive over the following chunks and mean nothing until they
    /// are whole.
    ///
    /// Reported separately from content because a seat using a tool looks
    /// identical to a hung one from outside: the words stop, no verdict
    /// arrives, and the turn sits there. Saying *which* tool turns a wait into
    /// something the operator can watch.
    Tool(&'a str),
    /// "Forget whatever you have streamed so far." Emitted when a request is
    /// retried in a different mode, so a partial reply is discarded rather
    /// than concatenated with the whole one.
    Reset,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// Carries the whole cause chain, not just reqwest's unhelpful summary.
    #[error("could not reach {url}: {}", cause_chain(std::error::Error::source(&.source)).unwrap_or_else(|| "no response".into()))]
    Transport { url: String, source: reqwest::Error },
    #[error("{url} returned HTTP {status}: {body}")]
    Status { url: String, status: u16, body: String },
    #[error("could not read the response: {0}")]
    Stream(String),
    #[error("the request was cancelled")]
    Cancelled,
}

impl LlmError {
    /// Whether trying the same request again could plausibly succeed.
    ///
    /// The distinction matters because the two kinds are not equally recoverable.
    /// A timeout or a 502 is the network having a moment, and retrying gets the
    /// turn back whole. A 401 or a malformed request is the same answer every
    /// time, and retrying it just delays the report and burns the seat's turn
    /// twice over.
    pub fn is_transient(&self) -> bool {
        match self {
            // Could not reach it, or it did not answer in time.
            Self::Transport { .. } | Self::Stream(_) => true,
            // Overloaded, rate-limited, or a server having a bad minute.
            Self::Status { status, .. } => *status == 429 || *status >= 500,
            // The caller went away, or the operator said stop.
            Self::Cancelled => false,
        }
    }

    /// Whether this is the model refusing the prompt for being too long, which
    /// is recoverable by sending a shorter one.
    ///
    /// Every provider in `client.rs`'s header list reports this as a plain 400,
    /// the same status as a malformed request — which is permanent. So this is
    /// body matching, and deliberately narrow: a false positive here means a
    /// delegate loses part of its history to fix a request that would have been
    /// rejected identically every time, and a false negative means the debate
    /// stops on a 400 that a shorter prompt would have cleared.
    ///
    /// The shapes, from each provider's own documentation:
    ///
    /// - OpenAI and DeepSeek: `This model's maximum context length is N tokens.
    ///   However, you requested M tokens…`
    /// - Gemini: `INVALID_ARGUMENT`, `The input token count (N) exceeds the
    ///   maximum number of tokens allowed (M).`
    /// - Claude: `invalid_request_error`, `prompt is too long`
    /// - OpenRouter: a typed `error.metadata.error_type` of
    ///   `context_length_exceeded` — the one provider that labels it, and the
    ///   one to check first where the body is machine-readable.
    pub fn is_context_overflow(&self) -> bool {
        let Self::Status { status, body, .. } = self else {
            return false;
        };
        // All four are 400. A 413 or a 529 is a size or capacity refusal, not
        // this.
        if *status != 400 {
            return false;
        }
        let b = body.to_ascii_lowercase();
        b.contains("context length")
            || b.contains("context_length_exceeded")
            || b.contains("maximum context length")
            || b.contains("input token count")
            || b.contains("prompt is too long")
            || b.contains("too many tokens")
            // OpenRouter and anything speaking its vocabulary.
            || b.contains("\"context_length_exceeded\"")
            // Gemini nests its own error object as a JSON *string*, so the
            // status name is quoted twice over in the body.
            // Deliberately NOT matching a bare "invalid_argument". Gemini's OpenAI-
            // compatible endpoint reports *every* 400 as INVALID_ARGUMENT — empty
            // contents, a malformed tool declaration, an unsupported field, a bad
            // thinking level — and this client reaches Gemini over that path. A false
            // positive is destructive rather than merely wasteful: compaction writes the
            // shortened history back to the board *before* resending, so one schema
            // error silently deleted a delegate's oldest speeches for the rest of the
            // debate, and then the identical request failed again and the turn was
            // reported as lost. The real Gemini overflow message contains the specific
            // text matched above.

            // The clearest signal of all, when a provider bothers to say it.
            || b.contains("reduce the length of the messages")
    }
}

/// The engine talks to models only through this trait, which is what makes the
/// debate loop testable without a server.
#[async_trait]
pub trait ChatBackend: Send + Sync + 'static {
    async fn chat(
        &self,
        req: &ChatRequest,
        on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
    ) -> Result<ChatResponse, LlmError>;
}

// ---------------------------------------------------------------------------

/// A client honouring the endpoint's TLS wishes. Only skips certificate
/// checking when the user has explicitly asked for it.
pub fn client_for(endpoint: &crate::types::EndpointConfig) -> Result<reqwest::Client, LlmError> {
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(IDLE_READ_SECS));
    #[cfg(feature = "tls")]
    if endpoint.allow_invalid_certs {
        return builder
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()
            .map_err(|e| LlmError::Stream(format!("could not build a TLS client: {e}")));
    }
    let _ = endpoint;
    builder
        .build()
        .map_err(|e| LlmError::Stream(format!("could not build an HTTP client: {e}")))
}

/// One shared connection pool; per-endpoint timeouts ride on the request.
pub struct HttpBackend {
    client: reqwest::Client,
}

impl Default for HttpBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpBackend {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(IDLE_READ_SECS))
            .build()
            .unwrap_or_default();
        Self { client }
    }

    /// Reuse the pooled client unless this endpoint needs a laxer TLS setup,
    /// in which case a dedicated one is built.
    fn for_endpoint(
        &self,
        endpoint: &crate::types::EndpointConfig,
    ) -> Result<reqwest::Client, LlmError> {
        if endpoint.allow_invalid_certs {
            client_for(endpoint)
        } else {
            Ok(self.client.clone())
        }
    }
}

fn build_body(req: &ChatRequest, stream: bool) -> Value {
    // Messages with images use the content-parts shape; everything else keeps
    // the plain string shape it always had, so text-only traffic is untouched.
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            if m.images.is_empty() {
                json!({ "role": m.role, "content": m.content })
            } else {
                let mut parts = vec![json!({ "type": "text", "text": m.content })];
                for url in &m.images {
                    parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                }
                json!({ "role": m.role, "content": parts })
            }
        })
        .collect();
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": stream,
    });
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(m) = req.max_tokens {
        body["max_tokens"] = json!(m);
    }
    // Only a streamed response can be asked for usage, and only the operator can
    // ask: see `EndpointConfig::include_usage`.
    if stream && req.endpoint.include_usage {
        body["stream_options"] = json!({ "include_usage": true });
    }
    // Reasoning effort, in whichever shape this endpoint actually reads. There is
    // no common spelling, and sending the wrong one is not free: a model whose
    // template does not know the value raises on it and the request comes back
    // 400 rather than being ignored — which is why this is off unless asked for.
    if let Some(effort) = req
        .endpoint
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        // Gemini spells this differently from everything else, and putting an
        // OpenAI-shaped `reasoning_effort` in a Gemini request is a 400 rather
        // than something ignored. So the mechanism is the provider's, and only
        // the *placement* below is the operator's to choose.
        match req.endpoint.provider.reasoning_mechanism() {
            crate::types::ReasoningMechanism::GeminiThinkingConfig => {
                body["thinking_config"] = json!({ "thinking_level": effort });
            }
            crate::types::ReasoningMechanism::EffortField => {
                match req.endpoint.reasoning_wire {
                    crate::types::ReasoningWire::ServerDefault => {}
                    crate::types::ReasoningWire::Flat => {
                        body["reasoning_effort"] = json!(effort);
                    }
                    crate::types::ReasoningWire::ChatTemplate => {
                        let mut template = body
                            .get("chat_template_kwargs")
                            .and_then(Value::as_object)
                            .cloned()
                            .unwrap_or_default();
                        template.insert("reasoning_effort".into(), json!(effort));
                        body["chat_template_kwargs"] = Value::Object(template);
                    }
                }
            }
        }
    }
    // `enable_thinking` is a separate knob from the effort level and a request in
    // its own right — the Qwen line gates its thinking block behind it, and
    // "stop thinking entirely" is something somebody asks for with no level set.
    if let Some(on) = req.endpoint.thinking {
        let mut existing = body
            .get("chat_template_kwargs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        existing.insert("enable_thinking".into(), json!(on));
        body["chat_template_kwargs"] = Value::Object(existing);
    }
    if !req.tools.is_empty() {
        body["tools"] = json!(req.tools);
        body["tool_choice"] = json!(req.tool_choice);
    }
    body
}

/// A per-request total timeout alone allows one token every few seconds for an
/// hour, and leaves a stalled-but-open connection looking like slow thinking.
/// This is the silence that means dead.
///
/// Set well above any plausible pause: a reasoning model can think for a while
/// before it emits speech, and cutting that off would fail the turn rather than
/// rescue it. Its job is to catch a connection that has stopped entirely, and
/// it does that long before a typical 300s endpoint timeout expires.
const IDLE_READ_SECS: u64 = 180;

/// A single SSE event should never approach this; anything larger without a
/// newline is a server that is not speaking the protocol.
const MAX_SSE_LINE: usize = 8 * 1024 * 1024;

fn apply_headers(
    mut rb: reqwest::RequestBuilder,
    endpoint: &crate::types::EndpointConfig,
) -> reqwest::RequestBuilder {
    // Endpoints differ wildly in how long a long reasoning turn takes, so the
    // budget is per endpoint rather than baked into the client.
    if let Some(key) = endpoint.api_key.as_ref().filter(|k| !k.trim().is_empty()) {
        let key = key.trim();
        if key.starts_with("Bearer ") {
            rb = rb.header(reqwest::header::AUTHORIZATION, key);
        } else {
            rb = rb.bearer_auth(key);
        }
    }
    rb = rb.timeout(Duration::from_secs(endpoint.timeout_secs.clamp(10, 3600)));
    for (k, v) in &endpoint.headers {
        rb = rb.header(k, v);
    }
    rb
}

#[async_trait]
impl ChatBackend for HttpBackend {
    async fn chat(
        &self,
        req: &ChatRequest,
        on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
    ) -> Result<ChatResponse, LlmError> {
        let client = self.for_endpoint(&req.endpoint)?;
        let candidates = req.endpoint.chat_completions_urls();
        let mut last: Option<LlmError> = None;

        // Rendered once per request, not once per attempt: the streaming and
        // single-response shapes differ only in flags, so the fallbacks below
        // toggle those on the same render instead of re-walking the messages
        // (multi-MB data URLs included) per URL tried. (The engine's
        // usage-retry necessarily re-renders: it changes `include_usage`, so
        // that second `chat` call builds a different body on purpose.)
        let first_body = build_body(req, req.endpoint.stream);
        for url in candidates {
            match attempt(&client, req, &url, &first_body, on_chunk).await {
                Ok(resp) => return Ok(resp),
                Err(LlmError::Cancelled) => return Err(LlmError::Cancelled),
                // A 404 on the first candidate usually means the base URL was
                // missing its `/v1`; try the other shape before giving up.
                Err(e @ LlmError::Status { status: 404, .. }) => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or(LlmError::Stream("no endpoint url to try".into())))
    }
}

/// One attempt at a URL: streaming if asked, whole-response otherwise.
async fn attempt(
    client: &reqwest::Client,
    req: &ChatRequest,
    url: &str,
    first_body: &Value,
    on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
) -> Result<ChatResponse, LlmError> {
    let mut notes = Vec::new();
    if req.endpoint.stream {
        match stream_once(client, req, url, first_body, on_chunk).await {
            Ok(mut resp) => {
                resp.notes = notes;
                return Ok(resp);
            }
            Err(LlmError::Cancelled) => return Err(LlmError::Cancelled),
            // Streaming is the fast path, not the only path: plenty of servers
            // advertise it and then get it wrong, and a Cloudflare tunnel will
            // happily cut it off mid-response. Say so, or the retry looks like
            // a stall from the outside.
            Err(e) => {
                notes.push(format!(
                    "streaming failed ({}), retrying as a single response",
                    truncate(&e.to_string(), 160)
                ));
                // Tell the UI to drop what the partial stream already put on
                // screen. Without it the fallback appends the whole reply and
                // the transcript visibly repeats itself from the top.
                on_chunk(ChatChunk::Reset);
            }
        }
        // The same request with the stream flag (and its usage ask, which only
        // exists on streamed responses) off. Cloned from the streamed render
        // rather than rebuilt from the messages: same bytes, one render.
        let mut plain = first_body.clone();
        if let Some(obj) = plain.as_object_mut() {
            obj.insert("stream".into(), Value::Bool(false));
            obj.remove("stream_options");
        }
        let mut resp = one_shot(client, req, url, &plain, on_chunk).await?;
        resp.notes.append(&mut notes);
        return Ok(resp);
    }
    let mut resp = one_shot(client, req, url, first_body, on_chunk).await?;
    resp.notes.append(&mut notes);
    Ok(resp)
}

async fn one_shot(
    client: &reqwest::Client,
    req: &ChatRequest,
    url: &str,
    body: &Value,
    on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
) -> Result<ChatResponse, LlmError> {
    let rb = apply_headers(
        client.post(url).json(body),
        &req.endpoint,
    );
    let res = rb.send().await.map_err(|source| LlmError::Transport {
        url: url.to_string(),
        source,
    })?;
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| LlmError::Stream(e.to_string()))?;
    if !status.is_success() {
        return Err(status_error(
            url,
            status.as_u16(),
            &truncate(&text, 800),
            &req.endpoint,
        ));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| LlmError::Stream(format!("{e}: {}", truncate(&text, 400))))?;
    // Several OpenAI-compatible gateways answer HTTP 200 with an error object —
    // "the model foo does not exist", a quota message — because their proxy layer
    // sits in front of the model router. `serde_json`'s `Index` yields Null for a
    // missing key, so `choices` was Null, `message` was Null, and this returned
    // `Ok` with empty content: the operator was told the seat "returned an empty
    // turn — no speech, no thinking, no actions" instead of being told the model does
    // not exist. The streaming path is unaffected, so this only bit endpoints where
    // streaming is off — exactly the tunnelled ones this fallback exists for.
    if let Some(err) = v.get("error") {
        let body = err.to_string();
        return Err(LlmError::Status {
            url: url.to_string(),
            // Reported as a 4xx so the outer loop does not retry a permanent problem.
            status: 400,
            body: truncate(&body, 800),
        });
    }
    if v.get("choices").and_then(Value::as_array).is_none_or(|c| c.is_empty()) {
        return Err(LlmError::Stream(format!(
            "the server answered with no choices, which is not a completion: {}",
            truncate(&text, 400)
        )));
    }
    let choice = &v["choices"][0];
    let message = &choice["message"];
    let content = message["content"].as_str().unwrap_or_default().to_string();
    let reasoning = reasoning_text(message);
    let tool_calls = parse_tool_calls(&message["tool_calls"]);
    // The non-streaming path arrives all at once, so there is no "starting to"
    // to report — but the operator still deserves to know what was called.
    for call in &tool_calls {
        on_chunk(ChatChunk::Tool(&call.name));
    }
    if !content.is_empty() {
        on_chunk(ChatChunk::Content(&content));
    }
    if !reasoning.is_empty() {
        on_chunk(ChatChunk::Reasoning(&reasoning));
    }
    let finish_reason = choice["finish_reason"].as_str().map(str::to_string);
    Ok(ChatResponse {
        finish_reason,
        content,
        reasoning,
        tool_calls,
        notes: Vec::new(),
        usage: parse_usage(&v),
    })
}

async fn stream_once(
    client: &reqwest::Client,
    req: &ChatRequest,
    url: &str,
    body: &Value,
    on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
) -> Result<ChatResponse, LlmError> {
    let rb = apply_headers(
        client.post(url).json(body),
        &req.endpoint,
    );
    let res = rb
        .send()
        .await
        .map_err(|source| LlmError::Transport {
            url: url.to_string(),
            source,
        })?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(status_error(url, status.as_u16(), &body, &req.endpoint));
    }

    let stream = res.bytes_stream();
    drain_sse(stream, on_chunk).await
}

/// Read a server-sent-event body into a finished response.
///
/// Split out from the request so the framing — chunk boundaries, missing final
/// newline, `[DONE]`, line size — can be tested against hand-built chunks
/// instead of a live socket.
async fn drain_sse<S, B, E>(
    stream: S,
    on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
) -> Result<ChatResponse, LlmError>
where
    S: futures_util::Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    futures_util::pin_mut!(stream);
    let mut acc = Accumulator::default();
    // Bytes, not a String, and only complete lines are ever decoded.
    //
    // `bytes_stream()` yields whatever byte runs hyper happened to read, and those
    // are *not* aligned to codepoint boundaries. Decoding each chunk with
    // `from_utf8_lossy` on its own replaces every byte of a split sequence with
    // U+FFFD, permanently — and U+FFFD is a legal character inside a JSON string, so
    // parsing succeeds and the damage lands silently on the board as `�` in the
    // middle of a point's text, a citation's source or a vote's reason. A newline
    // cannot occur inside a multi-byte sequence, so a line boundary is always a
    // character boundary: decoding up to the last newline is safe, and the tail
    // stays as bytes for the next chunk.
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| LlmError::Stream(e.to_string()))?;
        buf.extend_from_slice(bytes.as_ref());
        // A server that never sends a newline would otherwise grow this
        // without bound; one SSE event is never going to be this large.
        if buf.len() > MAX_SSE_LINE && !buf.contains(&b'\n') {
            return Err(LlmError::Stream(format!(
                "one SSE line exceeded {MAX_SSE_LINE} bytes without a newline"
            )));
        }
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line = String::from_utf8_lossy(&buf[..=nl]).into_owned();
            buf.drain(..=nl);
            if !process_line(&line, &mut acc, on_chunk) {
                return Ok(acc.finish(on_chunk));
            }
        }
        // `[DONE]` ends the response. Without stopping here we read to EOF, so
        // a server that sends it and then holds the connection open pins the
        // turn for the whole timeout.
        if acc.done {
            return Ok(acc.finish(on_chunk));
        }
    }
    // The server's last line may lack a trailing newline. Without this, the
    // final delta — often the tool call — is silently dropped and the turn
    // appears to hang.
    if !buf.is_empty() {
        let tail = String::from_utf8_lossy(&buf).into_owned();
        process_line(tail.trim(), &mut acc, on_chunk);
    }
    Ok(acc.finish(on_chunk))
}

/// One SSE line: strip `data:`, handle `[DONE]`, absorb the delta.
///
/// Returns false once the stream has been terminated, so the caller stops
/// reading rather than waiting out a connection the server will not close.
fn process_line(
    line: &str,
    acc: &mut Accumulator,
    on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
) -> bool {
    let line = line.trim();
    let Some(payload) = line.strip_prefix("data:") else {
        return true;
    };
    let payload = payload.trim();
    if payload.is_empty() {
        return true;
    }
    if payload == "[DONE]" {
        acc.done = true;
        return false;
    }
    let Ok(v) = serde_json::from_str::<Value>(payload) else {
        return true;
    };
    acc.absorb(&v, on_chunk);
    true
}

/// Folds the stream's deltas back into whole tool calls.
#[derive(Default)]
struct Accumulator {
    /// Set when the server sent `[DONE]`.
    done: bool,
    content: String,
    reasoning: String,
    finish_reason: Option<String>,
    calls: BTreeMap<usize, PartialCall>,
    order: Vec<usize>,
    usage: Option<Usage>,
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
}

impl Accumulator {
    fn absorb(&mut self, v: &Value, on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync)) {
        // The stop reason usually rides on the final chunk, beside the delta.
        if let Some(fr) = v["choices"][0]["finish_reason"].as_str() {
            self.finish_reason = Some(fr.to_string());
        }
        // Read before the delta check: the usage chunk is a bare `{"usage":…}`
        // with no choices at all, so anything that bailed on a missing delta
        // first would never see it.
        if self.usage.is_none() {
            self.usage = parse_usage(v);
        }
        let Some(delta) = v["choices"][0]["delta"].as_object() else {
            return;
        };
        if let Some(c) = delta.get("content").and_then(Value::as_str) {
            if !c.is_empty() {
                self.content.push_str(c);
                on_chunk(ChatChunk::Content(c));
            }
        }
        if let Some(r) = delta_reasoning(delta) {
            if !r.is_empty() {
                self.reasoning.push_str(&r);
                on_chunk(ChatChunk::Reasoning(&r));
            }
        }
        let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) else {
            return;
        };
        for tc in tool_calls {
            let idx = tc["index"].as_u64().unwrap_or(0) as usize;
            if !self.calls.contains_key(&idx) {
                self.calls.insert(idx, PartialCall::default());
                self.order.push(idx);
            }
            let slot = self.calls.get_mut(&idx).expect("just inserted");
            if let Some(id) = tc["id"].as_str() {
                slot.id = id.to_string();
            }
            if let Some(name) = tc["function"]["name"].as_str() {
                if !name.is_empty() && slot.name.is_empty() {
                    on_chunk(ChatChunk::Tool(name));
                }
                slot.name.push_str(name);
            }
            if let Some(args) = tc["function"]["arguments"].as_str() {
                slot.args.push_str(args);
            }
        }
    }

    fn finish(self, _on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync)) -> ChatResponse {
        let mut out = Vec::new();
        for idx in self.order {
            let Some(c) = self.calls.get(&idx) else {
                continue;
            };
            if c.name.trim().is_empty() {
                continue;
            }
            // A parse failure is not "no arguments". Both branches used to yield an
            // empty object, and `adjourn` is the one tool whose fields are all
            // optional — so it is the one action that parses out of `{}`. A reasoning
            // model that hit `max_tokens` inside
            // `{"reason":"the board is ready, P1 and P2 both` produced a *valid*
            // `Adjourn { reason: "the record is sufficient" }`, and the debate closed
            // early with no truncation recorded anywhere.
            //
            // Truncated JSON is never a valid tool call, so refuse it. A model that
            // genuinely sends no arguments still works: that is the empty string.
            let arguments = if c.args.trim().is_empty() {
                Value::Object(Default::default())
            } else {
                match serde_json::from_str(&c.args) {
                    Ok(v) => v,
                    Err(e) => {
                        return ChatResponse {
                            finish_reason: Some("length".into()),
                            content: self.content,
                            reasoning: self.reasoning,
                            tool_calls: Vec::new(),
                            notes: vec![format!(
                                "discarded {}: its arguments were not valid JSON ({e}); \
                                 this is what a reply cut off mid-tool-call looks like",
                                c.name
                            )],
                            usage: self.usage,
                        };
                    }
                }
            };
            out.push(RawToolCall {
                id: if c.id.is_empty() {
                    format!("call-{}", out.len())
                } else {
                    c.id.clone()
                },
                name: c.name.clone(),
                arguments,
            });
        }
        ChatResponse {
            finish_reason: self.finish_reason,
            content: self.content,
            reasoning: self.reasoning,
            tool_calls: out,
            notes: Vec::new(),
            usage: self.usage,
        }
    }
}

/// Thinking arrives under different keys depending on who serves the model:
/// DeepSeek and friends use `reasoning_content`, OpenRouter-style payloads use
/// `reasoning` as either a string or an object holding the text.
fn delta_reasoning(delta: &serde_json::Map<String, Value>) -> Option<String> {
    if let Some(r) = delta.get("reasoning_content").and_then(Value::as_str) {
        return (!r.is_empty()).then(|| r.to_string());
    }
    match delta.get("reasoning") {
        Some(Value::String(r)) => (!r.is_empty()).then(|| r.clone()),
        Some(Value::Object(o)) => o
            .get("content")
            .or_else(|| o.get("text"))
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// The same keys, but on a whole (non-streamed) message object.
fn reasoning_text(message: &Value) -> String {
    if let Some(r) = message.get("reasoning_content").and_then(Value::as_str) {
        if !r.is_empty() {
            return r.to_string();
        }
    }
    match message.get("reasoning") {
        Some(Value::String(r)) if !r.is_empty() => r.clone(),
        Some(Value::Object(o)) => o
            .get("content")
            .or_else(|| o.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn parse_tool_calls(v: &Value) -> Vec<RawToolCall> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .enumerate()
                .filter_map(|(i, tc)| {
                    let name = tc["function"]["name"].as_str()?.to_string();
                    let args = tc["function"]["arguments"].clone();
                    let args = match args {
                        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::Null),
                        other => other,
                    };
                    Some(RawToolCall {
                        id: tc["id"]
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("call-{i}")),
                        name,
                        arguments: args,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// reqwest says "error sending request for url (...)", which is useless on its
/// own. The reason is buried a few `source()` hops down, so walk the chain and
/// keep only the parts that explain the failure.
fn cause_chain(top: Option<&(dyn std::error::Error + 'static)>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = top;
    while let Some(err) = cur {
        let text = err.to_string();
        // Skip the layers that merely restate what we already know.
        let informative = !text.starts_with("error sending request")
            && !text.starts_with("client error")
            && text != "reqwest::Error";
        if informative && !parts.contains(&text) {
            parts.push(text);
        }
        cur = err.source();
    }
    (!parts.is_empty()).then(|| parts.join(" -> "))
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
        Some(at) => format!("{}…", &s[..at]),
        None => s.to_string(),
    }
}

/// Whether this endpoint's requests actually carry credentials.
///
/// The same test `apply_headers` uses, so the diagnostic can never disagree with
/// what went on the wire: a key that is present-but-blank counts as absent,
/// exactly as it does at the point of sending.
pub fn carries_credentials(endpoint: &crate::types::EndpointConfig) -> bool {
    endpoint
        .api_key
        .as_deref()
        .map(str::trim)
        .is_some_and(|k| !k.is_empty())
}

/// Explain a 401/403 in terms of what this app did or did not send.
///
/// A bare 401 says nothing actionable: "unauthenticated" is the server
/// describing its own gate, not telling the operator which of two very different
/// problems they have. Sending no key and sending the wrong key need opposite
/// fixes, so the message states which one happened — and never echoes the key.
///
/// This lives here, once, because it used to exist only on the two *model-list
/// probe* paths. A 401 during an actual debate came back as the bare server
/// text, which is precisely the moment the operator has least context and most
/// at stake: the debate is mid-flight and the message is one word long.
pub fn explain_auth_refusal(body: &str, carried: bool) -> String {
    let mut text = truncate(body, 400);
    if carried {
        text.push_str(
            " — this request carried an API key and the server still refused it: the key \
             is wrong for this server, or belongs to a different instance or account. Keys \
             set in served mode live on the server; check the one stored there.",
        );
    } else {
        text.push_str(
            " — no API key is set on this endpoint, so the request went out without \
             credentials. If this server requires one, add it under Model servers and \
             probe again. Note that some local servers advertise keyless access in their \
             settings and still refuse a request with no Authorization header — Unsloth is \
             one — so 'optional' here means 'optional for a server that allows it'.",
        );
    }
    text
}

/// The full refusal: body plus, for a 401/403, the credential explanation.
pub fn status_error(
    url: &str,
    status: u16,
    body: &str,
    endpoint: &crate::types::EndpointConfig,
) -> LlmError {
    let body = if status == 401 || status == 403 {
        explain_auth_refusal(body, carries_credentials(endpoint))
    } else {
        truncate(body, 800)
    };
    LlmError::Status {
        url: url.to_string(),
        status,
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EndpointConfig;

    fn request(endpoint: EndpointConfig) -> ChatRequest {
        ChatRequest {
            endpoint,
            model: "m".into(),
            messages: vec![ChatMessage::user("hi")],
            tools: vec![],
            tool_choice: "auto".into(),
            temperature: Some(0.4),
            max_tokens: None,
        }
    }

    #[test]
    fn usage_is_asked_for_only_when_the_operator_opted_in() {
        // Opt-in, because a server that does not know the field rejects the
        // whole request rather than ignoring it.
        let plain = EndpointConfig::default();
        let body = build_body(&request(plain), true);
        assert!(
            body.get("stream_options").is_none(),
            "the default build must not ask for usage"
        );

        let opted = EndpointConfig {
            include_usage: true,
            ..Default::default()
        };
        assert_eq!(
            build_body(&request(opted.clone()), true)["stream_options"],
            json!({ "include_usage": true })
        );
        // A one-piece request has no stream to attach the option to, and
        // sending it anyway is how you earn a 400 for nothing.
        assert!(
            build_body(&request(opted), false)
                .get("stream_options")
                .is_none()
        );
    }

    #[test]
    fn usage_is_read_from_whatever_the_server_calls_it() {
        let openai = json!({"usage": {"prompt_tokens": 1200, "completion_tokens": 340}});
        assert_eq!(
            parse_usage(&openai),
            Some(Usage {
                prompt_tokens: 1200,
                completion_tokens: 340
            })
        );
        assert_eq!(parse_usage(&openai).unwrap().total(), 1540);

        // Anthropic-shaped and OpenRouter-shaped servers spell it differently.
        let alt = json!({"usage": {"input_tokens": 10, "output_tokens": 5}});
        assert_eq!(
            parse_usage(&alt),
            Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5
            })
        );

        // Absent, null, or all-zero is "we do not know", never "it was free".
        assert_eq!(parse_usage(&json!({})), None);
        assert_eq!(parse_usage(&json!({"usage": null})), None);
        assert_eq!(parse_usage(&json!({"usage": {"prompt_tokens": 0}})), None);
    }

    #[test]
    fn a_usage_chunk_with_no_choices_is_still_absorbed() {
        // The counts arrive on a bare trailing chunk, which has no delta. An
        // accumulator that bailed on the missing delta first would never see
        // them, and the meter would read zero forever.
        let mut acc = Accumulator::default();
        acc.absorb(&json!({"choices": []}), &|_| {});
        acc.absorb(
            &json!({"usage": {"prompt_tokens": 900, "completion_tokens": 100}}),
            &|_| {},
        );
        assert_eq!(
            acc.finish(&|_| {}).usage,
            Some(Usage {
                prompt_tokens: 900,
                completion_tokens: 100
            })
        );
    }

    #[test]
    fn normalises_the_completions_url() {
        let cases = [
            ("http://127.0.0.1:8000/v1", "http://127.0.0.1:8000/v1/chat/completions"),
            ("http://127.0.0.1:8000/v1/", "http://127.0.0.1:8000/v1/chat/completions"),
            (
                "http://127.0.0.1:8000/v1/chat/completions",
                "http://127.0.0.1:8000/v1/chat/completions",
            ),
            ("https://api.openai.com/v1", "https://api.openai.com/v1/chat/completions"),
        ];
        for (input, want) in cases {
            let ep = EndpointConfig {
                base_url: input.into(),
                ..Default::default()
            };
            assert_eq!(ep.chat_completions_url(), want, "for {input}");
        }
    }

    fn ep(base_url: &str) -> EndpointConfig {
        EndpointConfig {
            base_url: base_url.into(),
            ..Default::default()
        }
    }

    /// The 401 has to say which of two opposite problems it is.
    ///
    /// A server answering `unauthenticated` is describing its own gate; it is
    /// not telling the operator whether they should add a key or fix one. Those
    /// need opposite actions, and the one that bites hardest is the first — the
    /// operator has left the key blank because the field said "optional", and
    /// gets one word back.
    #[test]
    fn a_refusal_without_a_key_says_the_key_never_travelled() {
        let mut e = ep("http://127.0.0.1:8888");
        assert!(!carries_credentials(&e));
        let text = explain_auth_refusal("unauthenticated", carries_credentials(&e));
        assert!(
            text.contains("without credentials"),
            "must say no key was sent: {text}"
        );
        // The specific trap that prompted this: the server's own settings say
        // keyless is allowed, and it still refuses. Naming that saves the
        // operator from concluding their setup is wrong.
        assert!(text.contains("Unsloth"), "names the known case: {text}");
        assert!(
            !text.to_lowercase().contains("sk-unsloth"),
            "must not suggest pasting a key it cannot see"
        );
    }

    /// A key that is present but blank counts as absent, because that is what
    /// `apply_headers` does. If these two disagreed the diagnostic would deny
    /// sending a key the request actually carried.
    #[test]
    fn a_blank_key_counts_as_no_key() {
        let mut e = ep("http://127.0.0.1:8888");
        e.api_key = Some("   ".into());
        assert!(!carries_credentials(&e), "whitespace is not a key");
        e.api_key = Some("sk-real".into());
        assert!(carries_credentials(&e));
    }

    #[test]
    fn a_refusal_with_a_key_points_at_the_key_not_at_adding_one() {
        let mut e = ep("http://127.0.0.1:8888");
        e.api_key = Some("sk-wrong".into());
        let text = explain_auth_refusal("unauthenticated", carries_credentials(&e));
        assert!(text.contains("wrong for this server"), "{text}");
        assert!(
            !text.contains("without credentials"),
            "must not tell someone with a key that no key was sent: {text}"
        );
        assert!(
            !text.contains("sk-wrong"),
            "the key itself must never be echoed back"
        );
    }

    /// Only 401/403 are credential problems. Bolting the explanation onto a 400
    /// or a 500 would send the operator to the API key when the real problem is
    /// the request.
    #[test]
    fn other_statuses_are_left_alone() {
        let e = ep("http://127.0.0.1:8888");
        for status in [400u16, 404, 429, 500, 503] {
            match status_error("u", status, "no model named qwen", &e) {
                LlmError::Status { body, .. } => {
                    assert_eq!(body, "no model named qwen", "status {status} was annotated");
                }
                other => panic!("status {status} produced {other:?}"),
            }
        }
    }

    #[test]
    fn a_refusal_through_the_shared_helper_carries_the_explanation() {
        let e = ep("http://127.0.0.1:8888");
        match status_error("u", 401, "unauthenticated", &e) {
            LlmError::Status { status, body, .. } => {
                assert_eq!(status, 401);
                assert!(body.contains("unauthenticated"), "keeps the server's own words");
                assert!(body.contains("without credentials"), "and adds the diagnosis");
            }
            other => panic!("expected a status error, got {other:?}"),
        }
    }

    #[test]
    fn a_bare_host_gets_v1_because_that_is_what_openai_servers_serve() {
        // Unsloth's docs say "use http://localhost:PORT as the base URL" while
        // its OpenAI routes live under /v1. This is the single most common way
        // to get a 404 out of an otherwise healthy server.
        assert_eq!(
            ep("http://localhost:8888").chat_completions_url(),
            "http://127.0.0.1:8888/v1/chat/completions"
        );
        assert_eq!(
            ep("http://127.0.0.1:8000").models_urls(),
            vec![
                "http://127.0.0.1:8000/v1/models",
                "http://127.0.0.1:8000/models"
            ]
        );
    }

    #[test]
    fn a_base_with_a_path_is_taken_literally() {
        assert_eq!(
            ep("http://localhost:8888/v1").chat_completions_urls(),
            vec!["http://127.0.0.1:8888/v1/chat/completions"]
        );
        assert_eq!(
            ep("https://api.openai.com/v1").chat_completions_urls(),
            vec!["https://api.openai.com/v1/chat/completions"]
        );
    }

    #[test]
    fn localhost_is_pinned_to_ipv4() {
        // Local servers usually bind 127.0.0.1 only; a resolver answering
        // ::1 first would be connection-refused from a client that works fine
        // on IPv4.
        assert_eq!(ep("http://localhost").base(), "http://127.0.0.1");
        assert_eq!(ep("http://localhost:1234/v1/").base(), "http://127.0.0.1:1234/v1");
        // But a host that merely starts with "localhost" is left alone.
        assert_eq!(ep("http://localhostexample:80").base(), "http://localhostexample:80");
    }

    #[test]
    fn a_completions_path_is_stripped_before_rebuilding() {
        for input in [
            "http://localhost:8888/v1/chat/completions",
            "http://localhost:8888/v1/completions",
        ] {
            assert_eq!(
                ep(input).chat_completions_urls(),
                vec!["http://127.0.0.1:8888/v1/chat/completions"],
                "for {input}"
            );
        }
    }

    #[test]
    fn body_omits_tools_when_none_are_offered() {
        let req = ChatRequest {
            endpoint: EndpointConfig::default(),
            model: "m".into(),
            messages: vec![ChatMessage::user("hi")],
            tools: vec![],
            tool_choice: "auto".into(),
            temperature: Some(0.4),
            max_tokens: None,
        };
        let body = build_body(&req, true);
        assert_eq!(body["stream"], json!(true));
        assert!(body.get("tools").is_none());
        let t = body["temperature"].as_f64().expect("temperature");
        assert!((t - 0.4).abs() < 1e-6, "temperature was {t}");
    }

    #[test]
    fn images_ride_as_content_parts_and_text_stays_a_string() {
        let mut with_images = ChatMessage::user("look at this");
        with_images.images = vec!["data:image/png;base64,AAA".into()];
        let req = ChatRequest {
            endpoint: EndpointConfig::default(),
            model: "m".into(),
            messages: vec![ChatMessage::user("hi"), with_images],
            tools: vec![],
            tool_choice: "none".into(),
            temperature: None,
            max_tokens: None,
        };
        let body = build_body(&req, false);
        // Text-only traffic keeps exactly the shape it always had.
        assert_eq!(body["messages"][0], json!({"role": "user", "content": "hi"}));
        assert_eq!(
            body["messages"][1],
            json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "look at this"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}},
                ],
            })
        );
    }

    /// Images ride exactly once: after the seat's first accepted turn the
    /// stored history carries no data URLs, so the second turn's body must
    /// not contain one anywhere — same messages, minus the payload.
    #[test]
    fn a_second_turn_body_carries_no_data_url() {
        let mut opening = ChatMessage::user("opening");
        opening.images = vec!["data:image/png;base64,AAA".into()];
        let first = ChatRequest {
            endpoint: EndpointConfig::default(),
            model: "m".into(),
            messages: vec![ChatMessage::system("s"), opening, ChatMessage::user("brief")],
            tools: vec![],
            tool_choice: "none".into(),
            temperature: None,
            max_tokens: None,
        };
        let first_body = serde_json::to_string(&build_body(&first, true)).expect("body");
        assert!(
            first_body.contains("data:image/png;base64,AAA"),
            "the first send must carry the image byte-identical"
        );
        // What `finish_turn` stores after that accepted turn: images cleared,
        // text untouched.
        let mut second_messages = first.messages.clone();
        for m in &mut second_messages {
            m.images.clear();
        }
        let second = ChatRequest {
            messages: second_messages,
            ..first.clone()
        };
        let second_body = serde_json::to_string(&build_body(&second, true)).expect("body");
        assert!(
            !second_body.contains("data:"),
            "the second send must not re-render the payload: {second_body}"
        );
        assert!(
            second_body.contains("opening") && second_body.contains("brief"),
            "only the payload goes, never the text: {second_body}"
        );
    }

    /// `truncate` on multibyte input: same head as `chars().take(n)`, and no
    /// split characters — including input that fits by chars but not bytes.
    #[test]
    fn truncate_never_splits_multibyte_input() {
        let s = format!("{}{}", "🎉".repeat(10), "x".repeat(500));
        let old = {
            if s.chars().count() <= 400 {
                s.clone()
            } else {
                format!("{}…", s.chars().take(400).collect::<String>())
            }
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

    #[test]
    fn reasoning_effort_is_sent_in_the_shape_the_endpoint_reads() {
        // There is no common spelling. llama-server has no top-level
        // `reasoning_effort` at all (ggml-org/llama.cpp#20408) — it goes through
        // `chat_template_kwargs`, straight into the model's Jinja template — while
        // OpenAI, vLLM and NIM read it beside `temperature`. Sending the wrong one
        // is not free: a template that does not recognise the value raises on it
        // and the request comes back 400 rather than being ignored.
        let with = |wire: crate::types::ReasoningWire, effort: Option<&str>| {
            build_body(
                &ChatRequest {
                    endpoint: EndpointConfig {
                        reasoning_wire: wire,
                        reasoning_effort: effort.map(str::to_string),
                        thinking: None,
                        ..EndpointConfig::default()
                    },
                    model: "m".into(),
                    messages: vec![ChatMessage::user("hi")],
                    tools: vec![],
                    tool_choice: "none".into(),
                    temperature: Some(0.4),
                    max_tokens: None,
                },
                false,
            )
        };

        // Off unless asked for: a reasoning model left to itself may default to
        // a level that is slow, or that fails.
        let body = with(crate::types::ReasoningWire::ServerDefault, Some("medium"));
        assert!(body.get("reasoning_effort").is_none(), "{body}");
        assert!(body.get("chat_template_kwargs").is_none(), "{body}");

        let body = with(crate::types::ReasoningWire::Flat, Some("medium"));
        assert_eq!(body["reasoning_effort"], json!("medium"));
        assert!(body.get("chat_template_kwargs").is_none(), "{body}");

        let body = with(crate::types::ReasoningWire::ChatTemplate, Some("low"));
        assert_eq!(body["chat_template_kwargs"]["reasoning_effort"], json!("low"));
        assert!(body.get("reasoning_effort").is_none(), "{body}");

        // Blank is not a level; sending "" is how you get a template to raise.
        let body = with(crate::types::ReasoningWire::Flat, Some("  "));
        assert!(body.get("reasoning_effort").is_none(), "{body}");
    }

    #[test]
    fn thinking_is_a_separate_knob_from_the_effort_level() {
        // Both are read by the same Jinja template, and a model with thinking off
        // does not want an effort level asserted at the same time.
        let body = build_body(
            &ChatRequest {
                endpoint: EndpointConfig {
                    reasoning_wire: crate::types::ReasoningWire::ChatTemplate,
                    reasoning_effort: None,
                    thinking: Some(false),
                    ..EndpointConfig::default()
                },
                model: "m".into(),
                messages: vec![ChatMessage::user("hi")],
                tools: vec![],
                tool_choice: "none".into(),
                temperature: Some(0.4),
                max_tokens: None,
            },
            false,
        );
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], json!(false));
    }

    #[test]
    fn a_tool_call_is_announced_once_its_name_starts_arriving() {
        // The signal that lets the UI show a sign of life while a seat reaches
        // for a tool. It fires on the name alone — the arguments are still
        // streaming and mean nothing until they are whole — and once per call,
        // not once per fragment, so a name split across three chunks is one tool
        // call and not three.
        let mut acc = Accumulator::default();
        let seen = std::sync::Mutex::new(Vec::<String>::new());
        let on_chunk = |c: ChatChunk<'_>| {
            if let ChatChunk::Tool(name) = c {
                seen.lock().expect("seen").push(name.to_string());
            }
        };
        for delta in [
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1",
                "function":{"name":"vo","arguments":""}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,
                "function":{"name":"te","arguments":"{\"point\":"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,
                "function":{"arguments":"1}"}}]}}]}),
        ] {
            acc.absorb(&delta, &on_chunk);
        }
        let out = acc.finish(&on_chunk);
        assert_eq!(
            seen.into_inner().expect("seen"),
            vec!["vo"],
            "only the first fragment is a new call"
        );
        // And the call still assembles, which is what it is announced for.
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].name, "vote");
        assert_eq!(out.tool_calls[0].arguments["point"], json!(1));
    }

    #[test]
    fn two_tool_calls_are_announced_separately() {
        let mut acc = Accumulator::default();
        let seen = std::sync::Mutex::new(Vec::<String>::new());
        let on_chunk = |c: ChatChunk<'_>| {
            if let ChatChunk::Tool(name) = c {
                seen.lock().expect("seen").push(name.to_string());
            }
        };
        acc.absorb(
            &json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"function":{"name":"vote","arguments":"{}"}},
                {"index":1,"function":{"name":"rule","arguments":"{}"}}]}}]}),
            &on_chunk,
        );
        assert_eq!(
            seen.into_inner().expect("seen"),
            vec!["vote", "rule"],
            "two calls in one delta are two calls, not one"
        );
    }

    #[test]
    fn accumulates_split_tool_call_deltas() {
        let mut acc = Accumulator::default();
        let noop = |_: ChatChunk<'_>| {};
        acc.absorb(
            &json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1",
                "function":{"name":"vote","arguments":"{\"point\":"}}]}}]}),
            &noop,
        );
        acc.absorb(
            &json!({"choices":[{"delta":{"tool_calls":[{"index":0,
                "function":{"arguments":"1,\"verdict\":\"win\"}"}}]}}]}),
            &noop,
        );
        let out = acc.finish(&noop);
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].name, "vote");
        assert_eq!(out.tool_calls[0].arguments["point"], json!(1));
        assert_eq!(out.tool_calls[0].arguments["verdict"], json!("win"));
    }

    #[test]
    fn accumulates_thinking_from_every_key_shape() {
        // DeepSeek streams `reasoning_content`; OpenRouter-style payloads use
        // `reasoning` as a string or an object. All three must become thinking,
        // never speech.
        let mut acc = Accumulator::default();
        let thought = std::sync::Mutex::new(String::new());
        let sink = |c: ChatChunk<'_>| {
            if let ChatChunk::Reasoning(s) = c {
                if let Ok(mut g) = thought.lock() {
                    g.push_str(s);
                }
            }
        };
        acc.absorb(
            &json!({"choices":[{"delta":{"reasoning_content":"deep "}}]}),
            &sink,
        );
        acc.absorb(&json!({"choices":[{"delta":{"reasoning":"seek "}}]}), &sink);
        acc.absorb(
            &json!({"choices": [{"delta": {"reasoning": {"content": "router"}}}]}),
            &sink,
        );
        acc.absorb(&json!({"choices":[{"delta":{"content":"spoken"}}]}), &sink);
        let out = acc.finish(&|_| {});
        assert_eq!(out.reasoning, "deep seek router");
        assert_eq!(out.content, "spoken");
        assert_eq!(*thought.lock().expect("lock"), "deep seek router");
    }

    #[test]
    fn the_stop_reason_survives_the_stream() {
        let mut acc = Accumulator::default();
        let noop = |_: ChatChunk<'_>| {};
        acc.absorb(&json!({"choices": [{"delta": {"content": "hi"}}]}), &noop);
        acc.absorb(
            &json!({"choices": [{"delta": {}, "finish_reason": "length"}]}),
            &noop,
        );
        let out = acc.finish(&noop);
        assert_eq!(out.finish_reason.as_deref(), Some("length"));
    }

    #[test]
    fn whole_responses_expose_their_thinking() {
        assert_eq!(
            reasoning_text(&json!({"reasoning_content": "r1"})),
            "r1"
        );
        assert_eq!(reasoning_text(&json!({"reasoning": "r2"})), "r2");
        assert_eq!(
            reasoning_text(&json!({"reasoning": {"content": "r3"}})),
            "r3"
        );
        assert_eq!(reasoning_text(&json!({"content": "only"})), "");
    }

    #[test]
    fn a_final_line_without_a_newline_is_still_processed() {
        // The server's last delta may lack a trailing newline. It must still
        // be absorbed, or the turn hangs with tokens on the table.
        let mut acc = Accumulator::default();
        let seen = std::sync::Mutex::new(String::new());
        let sink = |c: ChatChunk<'_>| {
            if let ChatChunk::Content(s) = c {
                if let Ok(mut g) = seen.lock() {
                    g.push_str(s);
                }
            }
        };
        // Simulate process_line being called on a line without a newline.
        process_line("data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}", &mut acc, &sink);
        let out = acc.finish(&|_| {});
        assert_eq!(out.content, "hi");
        assert_eq!(*seen.lock().expect("lock"), "hi");
    }

    #[test]
    fn accumulates_content_tokens() {
        let mut acc = Accumulator::default();
        let seen = std::sync::Mutex::new(String::new());
        let sink = |c: ChatChunk<'_>| {
            if let ChatChunk::Content(s) = c {
                if let Ok(mut g) = seen.lock() {
                    g.push_str(s);
                }
            }
        };
        acc.absorb(&json!({"choices":[{"delta":{"content":"Hel"}}]}), &sink);
        acc.absorb(&json!({"choices":[{"delta":{"content":"lo"}}]}), &sink);
        let out = acc.finish(&|_| {});
        assert_eq!(out.content, "Hello");
        assert_eq!(*seen.lock().expect("lock"), "Hello");
    }
}

#[cfg(test)]
mod sse_tests {
    use super::*;

    /// Feed the drain loop a hand-built body, chunked exactly as given.
    fn drain(chunks: &[&str]) -> Result<ChatResponse, LlmError> {
        let owned: Vec<Result<Vec<u8>, std::io::Error>> = chunks
            .iter()
            .map(|c| Ok(c.as_bytes().to_vec()))
            .collect();
        let noop = |_: ChatChunk<'_>| {};
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(drain_sse(futures_util::stream::iter(owned), &noop))
    }

    fn delta(content: &str) -> String {
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n", json!(content))
    }

    #[test]
    fn a_payload_split_across_chunks_is_reassembled() {
        let one = delta("Hello");
        let (a, b) = one.split_at(9);
        let out = drain(&[a, b]).expect("drained");
        assert_eq!(out.content, "Hello");
    }

    /// A socket read boundary is not a character boundary.
    ///
    /// `bytes_stream()` yields whatever hyper read, so a chunk can end in the first
    /// byte of a three-byte character. Decoding each chunk on its own replaced the
    /// split sequence with U+FFFD, permanently, and since U+FFFD is legal inside a
    /// JSON string the damage was invisible: it landed on the board as `\u{fffd}` in
    /// the middle of a point's text or a citation's source. The existing split tests
    /// all cut pure ASCII, at byte 9, so they could never see it.
    #[test]
    fn a_character_split_across_two_chunks_survives_intact() {
        // "Roses — lilies": an em dash is three bytes, and CJK is three bytes a
        // character, so both straddle a naive split.
        for text in ["Roses \u{2014} lilies", "\u{82b1}\u{3053}\u{306e}\u{82b1} are roses", "caf\u{e9}"] {
            let payload = delta(text);
            let bytes = payload.as_bytes();
            // Cut at every byte position that lands inside a character. Finding them
            // exactly is the point: `text.char_indices()` gives the boundaries.
            let mut cuts: Vec<usize> = vec![1, 2, 3];
            cuts.extend(text.char_indices().map(|(i, _)| i + 1));
            for cut in cuts {
                if cut >= bytes.len() {
                    continue;
                }
                let chunks = vec![
                    String::from_utf8(bytes[..cut].to_vec()).unwrap_or_default(),
                    // The tail may not be valid UTF-8 on its own — that is the point.
                    String::from_utf8_lossy(&bytes[cut..]).into_owned(),
                ];
                let owned: Vec<Result<Vec<u8>, std::io::Error>> = vec![
                    Ok(bytes[..cut].to_vec()),
                    Ok(bytes[cut..].to_vec()),
                ];
                let noop = |_: ChatChunk<'_>| {};
                let out = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("runtime")
                    .block_on(drain_sse(futures_util::stream::iter(owned), &noop))
                    .expect("drained");
                assert_eq!(
                    out.content, text,
                    "cut at byte {cut} corrupted the reply (chunks {chunks:?})"
                );
                assert!(
                    !out.content.contains('\u{fffd}'),
                    "a replacement character reached the board: {:?}",
                    out.content
                );
            }
        }
    }

    #[test]
    fn a_final_line_without_a_newline_is_not_lost() {
        // The bug that made a finished turn look hung: the server's last delta
        // had no trailing newline and sat in the buffer forever.
        let out = drain(&[&delta("Hi")]).expect("drained");
        assert_eq!(out.content, "Hi");
    }

    /// One `data:` line carrying an arbitrary JSON value.
    fn line(v: &Value) -> String {
        format!("data: {v}\n\n")
    }

    fn delta_obj(v: Value) -> String {
        line(&json!({ "choices": [{ "delta": v }] }))
    }

    #[test]
    fn a_tool_call_split_across_chunks_survives() {
        let first = delta_obj(json!({
            "tool_calls": [{
                "index": 0, "id": "c1",
                "function": { "name": "vote", "arguments": "{\"point\":" }
            }]
        }));
        let second = delta_obj(json!({
            "tool_calls": [{
                "index": 0,
                "function": { "arguments": "1,\"verdict\":\"win\"}" }
            }]
        }));
        let stop = line(&json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }));
        let body = format!("{first}{second}{stop}");
        // Deliberately split mid-payload: the arguments string is only valid
        // once both halves are joined.
        let cut = body.len() / 3;
        let out = drain(&[&body[..cut], &body[cut..]]).expect("drained");
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].name, "vote");
        assert_eq!(out.tool_calls[0].arguments["point"], json!(1));
        assert_eq!(out.tool_calls[0].arguments["verdict"], json!("win"));
    }

    #[test]
    fn carriage_returns_are_tolerated() {
        let crlf = delta("x").replace("\n", "\r\n");
        let out = drain(&[&crlf]).expect("drained");
        assert_eq!(out.content, "x");
    }

    #[test]
    fn done_stops_the_read_loop() {
        // A server that sends [DONE] and then holds the connection open must
        // not pin the turn: the loop returns at the terminator.
        let owned: Vec<Result<Vec<u8>, std::io::Error>> = vec![
            Ok(format!("{}data: [DONE]\n\n", delta("x")).into_bytes()),
            // a chunk after DONE that must never be absorbed
            Ok(delta("SHOULD NOT APPEAR").into_bytes()),
        ];
        let noop = |_: ChatChunk<'_>| {};
        let out = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(drain_sse(futures_util::stream::iter(owned), &noop))
            .expect("drained");
        assert_eq!(out.content, "x");
        assert!(!out.content.contains("SHOULD NOT APPEAR"));
    }

    #[test]
    fn a_comment_only_line_is_ignored() {
        let body = format!(": keep-alive\n\n{}", delta("ok"));
        let out = drain(&[&body]).expect("drained");
        assert_eq!(out.content, "ok");
    }

    #[test]
    fn an_unterminated_run_of_bytes_is_refused_rather_than_grown() {
        let huge = "x".repeat(MAX_SSE_LINE + 16);
        assert!(drain(&[&huge]).is_err(), "an endless line must be refused");
    }

    #[test]
    fn a_malformed_line_does_not_abort_the_stream() {
        let body = format!("data: not json at all\n\n{}", delta("still here"));
        let out = drain(&[&body]).expect("drained");
        assert_eq!(out.content, "still here");
    }
}

/// Ask an endpoint what it is serving. Not every OpenAI-compatible server
/// implements `/models`, so a failure here is a hint, not a hard error.
pub async fn list_models(
    endpoint: &crate::types::EndpointConfig,
) -> Result<Vec<String>, LlmError> {
    if matches!(
        endpoint.provider,
        crate::types::Provider::Anthropic
    ) {
        return crate::anthropic::list_anthropic_models(endpoint).await;
    }
    let client = client_for(endpoint)?;
    let api_key = endpoint
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty());
    let mut last: Option<LlmError> = None;
    for url in endpoint.models_urls() {
        let mut rb = client
            .get(&url)
            .timeout(Duration::from_secs(endpoint.timeout_secs.clamp(5, 60)));
        if let Some(key) = api_key {
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
            // A 401/403 gets the credential explanation from the one shared
            // helper, so the probe and a live turn cannot word the same
            // diagnosis differently.
            let e = status_error(&url, status.as_u16(), &body, endpoint);
            if status.as_u16() == 404 {
                last = Some(e);
                continue;
            }
            return Err(e);
        }
        let v: Value = serde_json::from_str(&body).map_err(|e| LlmError::Stream(format!("{e}")))?;
        return Ok(v["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default());
    }
    Err(last.unwrap_or(LlmError::Stream("no endpoint url to try".into())))
}

#[cfg(test)]
mod overflow_tests {
    use super::*;

    /// Every provider in the header comment reports an over-long prompt as a
    /// 400. These are the documented bodies, verbatim as far as possible, so a
    /// provider changing its wording shows up here rather than in production.
    #[test]
    fn each_provider_shape_is_recognised() {
        let cases: &[(&str, &str)] = &[
            (
                "openai",
                r#"{"error":{"message":"This model's maximum context length is 128000 tokens. However, you requested 131072 tokens (129000 in the messages, 2072 in the completion). Please reduce the length of the messages or completion.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#,
            ),
            (
                "deepseek",
                r#"{"error":{"message":"This model's maximum context length is 65536 tokens. However, you requested 66581 tokens (66581 in the messages, 0 in the completion). Please reduce the length of the messages or completion.","type":"invalid_request_error","param":null,"code":"invalid_request_error"}}"#,
            ),
            (
                "gemini",
                r#"{"error":{"code":400,"message":"The input token count (1067544) exceeds the maximum number of tokens allowed (1048576).","status":"INVALID_ARGUMENT"}}"#,
            ),
            (
                "claude",
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 215685 tokens > 200000 maximum"}}"#,
            ),
            (
                "openrouter",
                r#"{"error":{"code":400,"message":"This endpoint's maximum context length is 256000 tokens. However, you requested about 256493 tokens.","metadata":{"error_type":"context_length_exceeded"}}}"#,
            ),
            // Gemini nests its error as a JSON string, so the status arrives
            // double-encoded. This is the shape that actually arrives on the wire.
            (
                "gemini-nested",
                r#"{"error":{"message":"{\"error\": {\"code\": 400, \"message\": \"The input token count (87814) exceeds the maximum number of tokens allowed (65536).\", \"status\": \"INVALID_ARGUMENT\"}}","code":400,"status":"Bad Request"}}"#,
            ),
        ];
        for (name, body) in cases {
            let e = LlmError::Status {
                url: "https://example.invalid".into(),
                status: 400,
                body: (*body).into(),
            };
            assert!(e.is_context_overflow(), "{name} shape was not recognised");
            // And it is not transient: retrying the identical request is the
            // same 400 every time, which is why compaction has to intervene
            // rather than another backoff.
            assert!(!e.is_transient(), "{name} must not be treated as transient");
        }
    }

    /// A 400 for any other reason must not trigger history loss. This is the
    /// false positive that matters: compaction drops part of a delegate's
    /// memory, and doing that to fix a request that was malformed to begin with
    /// loses information for nothing.
    #[test]
    fn other_four_hundreds_are_not_overflow() {
        let cases = [
            r#"{"error":{"message":"unrecognized request argument supplied: stream_options","type":"invalid_request_error","param":"stream_options","code":"unknown_parameter"}}"#,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: must be greater than 0"}}"#,
            r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error","code":"invalid_api_key"}}"#,
            r#"{"error":{"message":"messages: text content blocks must contain non-whitespace text","type":"invalid_request_error"}}"#,
        ];
        for body in cases {
            let e = LlmError::Status {
                url: "https://example.invalid".into(),
                status: 400,
                body: body.into(),
            };
            assert!(!e.is_context_overflow(), "false positive on: {body}");
        }
    }

    /// Only 400 is overflow. A 413 is a payload-size refusal and a 529 is
    /// capacity; neither is fixed by shortening the prompt in the way compaction
    /// does, and treating them as overflow would silently truncate history.
    #[test]
    fn only_400_counts() {
        for status in [413u16, 429, 500, 503, 529] {
            let e = LlmError::Status {
                url: "https://example.invalid".into(),
                status,
                body: "maximum context length is 128000 tokens".into(),
            };
            assert!(!e.is_context_overflow(), "status {status} must not count");
        }
    }

    #[test]
    fn non_status_errors_are_never_overflow() {
        assert!(!LlmError::Stream("truncated".into()).is_context_overflow());
        assert!(!LlmError::Cancelled.is_context_overflow());
    }
}
