//! Anthropic’s Messages API, behind the same [`ChatBackend`] contract.
//!
//! This is deliberately separate from the OpenAI-compatible client. Anthropic
//! differs in authentication, URLs, request shape, streaming events, tool calls,
//! usage reporting and stop reasons. Sharing code at the `ChatResponse` boundary
//! keeps the engine, retries, compaction and UI code provider-agnostic without
//! letting Anthropic conditionals leak into the OpenAI path.

use crate::client::{
    ChatBackend, ChatChunk, ChatMessage, ChatRequest, ChatResponse, LlmError, Usage, client_for,
    parse_usage,
};
use crate::tools::RawToolCall;
use crate::types::{EndpointConfig, Provider};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Output ceiling for Anthropic requests.
///
/// Anthropic requires `max_tokens`; the rest of this codebase intentionally
/// leaves that field unset, so this constant is what every seat gets.
///
/// It has to be a number the API *accepts*, not a number that sounds generous.
/// The Messages API validates `max_tokens` against the model's maximum output and
/// returns 400 rather than clamping: max output is 64K on current Sonnet/Haiku
/// models and 32K on Opus 4.5+, and a request asking for more is rejected outright.
/// The previous 200_000 therefore failed on *every* turn of *every* Anthropic seat
/// with `max_tokens: 200000 > 128000, which is the maximum allowed number of
/// output tokens` — and 400 is not transient, so there was no retry and no
/// fallback. Sixteen thousand is comfortably above a debate turn or a verdict
/// while staying inside every current model's ceiling.
///
/// An operator who needs more should raise it per endpoint rather than this
/// constant being inflated for everyone.
pub const ANTHROPIC_DEFAULT_MAX_TOKENS: u32 = 16_000;

/// One SSE event should never approach this; anything larger without a newline
/// is a server that is not speaking the protocol.
const MAX_SSE_LINE: usize = 8 * 1024 * 1024;

pub struct AnthropicBackend {
    client: reqwest::Client,
}

impl Default for AnthropicBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicBackend {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(180))
                .build()
                .unwrap_or_default(),
        }
    }

    fn http_client(&self, endpoint: &EndpointConfig) -> Result<reqwest::Client, LlmError> {
        if endpoint.allow_invalid_certs {
            client_for(endpoint)
        } else {
            Ok(self.client.clone())
        }
    }
}

/// A backend that can serve a mixed council: each request is routed by the
/// endpoint it was built for, so Anthropic and OpenAI-compatible seats can
/// argue in the same debate.
pub struct ProviderBackend {
    openai: crate::client::HttpBackend,
    anthropic: AnthropicBackend,
}

impl Default for ProviderBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderBackend {
    pub fn new() -> Self {
        Self {
            openai: crate::client::HttpBackend::new(),
            anthropic: AnthropicBackend::new(),
        }
    }
}

#[async_trait]
impl ChatBackend for ProviderBackend {
    async fn chat(
        &self,
        req: &ChatRequest,
        on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
    ) -> Result<ChatResponse, LlmError> {
        match req.endpoint.provider {
            Provider::Anthropic => self.anthropic.chat(req, on_chunk).await,
            _ => self.openai.chat(req, on_chunk).await,
        }
    }
}

fn anthropic_api_key(endpoint: &EndpointConfig) -> Option<String> {
    endpoint
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(|key| {
            key.strip_prefix("Bearer ")
                .or_else(|| key.strip_prefix("bearer "))
                .unwrap_or(key)
                .to_string()
        })
}

fn apply_anthropic_headers(
    mut rb: reqwest::RequestBuilder,
    endpoint: &EndpointConfig,
) -> reqwest::RequestBuilder {
    if let Some(key) = anthropic_api_key(endpoint) {
        rb = rb.header("x-api-key", key);
    }
    rb = rb
        .header("anthropic-version", ANTHROPIC_VERSION)
        .timeout(Duration::from_secs(endpoint.timeout_secs.clamp(10, 3600)));
    for (name, value) in &endpoint.headers {
        rb = rb.header(name, value);
    }
    rb
}

fn build_anthropic_body(req: &ChatRequest, stream: bool) -> Result<Value, LlmError> {
    if !matches!(req.endpoint.provider, Provider::Anthropic) {
        return Err(LlmError::Stream(
            "Anthropic request built for a non-Anthropic endpoint".into(),
        ));
    }
    if req.model.trim().is_empty() {
        return Err(LlmError::Stream(
            "Anthropic needs a model name; the delegate has none".into(),
        ));
    }

    let mut systems = Vec::new();
    let mut messages = Vec::new();
    for message in &req.messages {
        if message.role == "system" {
            if !message.images.is_empty() {
                return Err(LlmError::Stream(
                    "Anthropic system prompts cannot carry images in this release".into(),
                ));
            }
            if !message.content.trim().is_empty() {
                systems.push(message.content.clone());
            }
            continue;
        }
        messages.push(anthropic_message(message)?);
    }
    if messages.is_empty() {
        return Err(LlmError::Stream(
            "Anthropic needs at least one user or assistant message".into(),
        ));
    }

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS),
        "messages": messages,
        "stream": stream,
    });
    if !systems.is_empty() {
        body["system"] = json!(systems.join("\n\n"));
    }
    if let Some(temperature) = req.temperature {
        // Anthropic accepts 0..=1; the rest of the app allows up to 2. Clamp at
        // this boundary rather than rejecting a valid OpenAI-style setting.
        body["temperature"] = json!(temperature.clamp(0.0, 1.0));
    }
    if !req.tools.is_empty() {
        let mut converted = Vec::with_capacity(req.tools.len());
        for tool in &req.tools {
            converted.push(anthropic_tool(tool)?);
        }
        body["tools"] = Value::Array(converted);
        body["tool_choice"] = anthropic_tool_choice(&req.tool_choice)?;
    }
    // Anthropic thinking controls vary by model generation and are display-only
    // in this release. Never translate the OpenAI/Gemini reasoning fields here:
    // a wrong thinking mode is a 400 rather than something ignored.
    Ok(body)
}

fn anthropic_message(message: &ChatMessage) -> Result<Value, LlmError> {
    if message.role != "user" && message.role != "assistant" {
        return Err(LlmError::Stream(format!(
            "Anthropic accepts only user and assistant messages, not {:?}",
            message.role
        )));
    }
    if message.images.is_empty() {
        // `TextBlockParam` has `minLength: 1` and the API answers
        // "messages: text content blocks must contain non-whitespace text". A
        // tool-only turn writes an empty assistant message, so an Anthropic seat 400s
        // on the turn after its first tool call — permanently, since a 400 is not
        // transient. The system path below already guarded this; the user/assistant
        // path did not.
        if message.content.trim().is_empty() {
            return Err(LlmError::Stream(
                "Anthropic will not accept an empty message".into(),
            ));
        }
        return Ok(json!({"role": message.role, "content": message.content}));
    }

    let mut parts = Vec::new();
    if !message.content.trim().is_empty() {
        parts.push(json!({"type": "text", "text": message.content}));
    }
    for image in &message.images {
        parts.push(anthropic_image_block(image)?);
    }
    if parts.is_empty() {
        return Err(LlmError::Stream(
            "Anthropic needs text, an image, or both in every message".into(),
        ));
    }
    Ok(json!({"role": message.role, "content": parts}))
}

fn anthropic_image_block(source: &str) -> Result<Value, LlmError> {
    if let Some(data_url) = source.strip_prefix("data:") {
        let Some((media_type, data)) = data_url.split_once(";base64,") else {
            return Err(LlmError::Stream(
                "Anthropic images must be base64 data URLs in this release".into(),
            ));
        };
        if !matches!(
            media_type,
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        ) {
            return Err(LlmError::Stream(format!(
                "Anthropic does not accept {media_type} images in this release"
            )));
        }
        if data.trim().is_empty() {
            return Err(LlmError::Stream("Anthropic image data is empty".into()));
        }
        return Ok(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data}
        }));
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        return Ok(json!({
            "type": "image",
            "source": {"type": "url", "url": source}
        }));
    }
    Err(LlmError::Stream(
        "Anthropic images must be data URLs or http(s) URLs in this release".into(),
    ))
}

fn anthropic_tool(definition: &Value) -> Result<Value, LlmError> {
    let function = definition.get("function").unwrap_or(definition);
    let Some(name) = function.get("name").and_then(Value::as_str) else {
        return Err(LlmError::Stream("An Anthropic tool needs a name".into()));
    };
    if !(1..=200).contains(&name.len())
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(LlmError::Stream(format!(
            "Anthropic tool name {name:?} is outside the API’s name pattern"
        )));
    }
    let parameters = function
        .get("parameters")
        .or_else(|| function.get("input_schema"))
        .cloned()
        .unwrap_or_else(|| json!({"type": "object"}));
    if !parameters.is_object() {
        return Err(LlmError::Stream(format!(
            "Anthropic tool {name:?} needs an object schema"
        )));
    }
    Ok(json!({
        "name": name,
        "description": function.get("description").and_then(Value::as_str).unwrap_or(""),
        "input_schema": parameters,
    }))
}

fn anthropic_tool_choice(choice: &str) -> Result<Value, LlmError> {
    match choice {
        "auto" => Ok(json!({"type": "auto"})),
        "none" => Ok(json!({"type": "none"})),
        other => Err(LlmError::Stream(format!(
            "Anthropic tool_choice {other:?} is not supported in this release"
        ))),
    }
}

#[derive(Default)]
struct AnthropicBlocks {
    text: String,
    reasoning: String,
    calls: Vec<RawToolCall>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
}

fn collect_anthropic_blocks(content: &Value, blocks: &mut AnthropicBlocks) {
    let Some(items) = content.as_array() else {
        return;
    };
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    blocks.text.push_str(text);
                }
            }
            Some("thinking") => {
                // Display only. Signatures are intentionally not retained: the
                // engine stores ordinary text history and must never echo a
                // modified or reconstructed thinking block.
                if let Some(thinking) = item.get("thinking").and_then(Value::as_str) {
                    blocks.reasoning.push_str(thinking);
                }
            }
            Some("tool_use") => {
                if let Some(call) = anthropic_tool_use(item, blocks.calls.len()) {
                    blocks.calls.push(call);
                }
            }
            _ => {}
        }
    }
}

fn anthropic_tool_use(block: &Value, position: usize) -> Option<RawToolCall> {
    let name = block.get("name").and_then(Value::as_str).unwrap_or_default();
    if name.trim().is_empty() {
        return None;
    }
    let id = block
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("call-{position}"));
    let arguments = match block.get("input").cloned().unwrap_or_else(|| json!({})) {
        Value::Object(_) => block.get("input").cloned().unwrap_or_else(|| json!({})),
        _ => json!({}),
    };
    Some(RawToolCall {
        id,
        name: name.to_string(),
        arguments,
    })
}

fn anthropic_stop_reason(raw: Option<&str>) -> Option<String> {
    match raw {
        None | Some("end_turn") => None,
        // The engine’s truncation, ballot and continuation logic is keyed to
        // OpenAI’s "length". Map the equivalent here so an Anthropic reply cut
        // by its own output ceiling is never mistaken for a complete answer.
        Some("max_tokens") => Some("length".to_string()),
        Some(other) => Some(other.to_string()),
    }
}

fn parse_anthropic_message(value: &Value) -> Result<ChatResponse, LlmError> {
    let Some(content) = value.get("content") else {
        return Err(LlmError::Stream(
            "Anthropic response had no content blocks".into(),
        ));
    };
    let mut blocks = AnthropicBlocks::default();
    collect_anthropic_blocks(content, &mut blocks);
    blocks.finish_reason =
        anthropic_stop_reason(value.get("stop_reason").and_then(Value::as_str));
    blocks.usage = parse_usage(value);
    Ok(ChatResponse {
        finish_reason: blocks.finish_reason,
        content: blocks.text,
        reasoning: blocks.reasoning,
        tool_calls: blocks.calls,
        notes: Vec::new(),
        usage: blocks.usage,
    })
}

#[derive(Default)]
struct AnthropicCall {
    id: String,
    name: String,
    input: String,
    announced: bool,
}

#[derive(Default)]
struct AnthropicAccumulator {
    text: String,
    reasoning: String,
    calls: BTreeMap<usize, AnthropicCall>,
    order: Vec<usize>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
}

impl AnthropicAccumulator {
    fn slot(&mut self, index: usize) -> &mut AnthropicCall {
        if !self.calls.contains_key(&index) {
            self.calls.insert(index, AnthropicCall::default());
            self.order.push(index);
        }
        self.calls.get_mut(&index).expect("just inserted")
    }

    fn absorb_event(
        &mut self,
        value: &Value,
        on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
    ) -> Result<(), LlmError> {
        let kind = value.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "message_start" => {
                self.usage = parse_usage(value.get("message").unwrap_or(value));
            }
            "content_block_start" => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let block = &value["content_block"];
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if name.trim().is_empty() {
                        return Ok(());
                    }
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call-{index}"));
                    let slot = self.slot(index);
                    slot.id = id;
                    slot.name = name.clone();
                    if !slot.announced {
                        slot.announced = true;
                        on_chunk(ChatChunk::Tool(&name));
                    }
                }
            }
            "content_block_delta" => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let delta = &value["delta"];
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                self.text.push_str(text);
                                on_chunk(ChatChunk::Content(text));
                            }
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(thinking) = delta.get("thinking").and_then(Value::as_str) {
                            if !thinking.is_empty() {
                                self.reasoning.push_str(thinking);
                                on_chunk(ChatChunk::Reasoning(thinking));
                            }
                        }
                    }
                    Some("signature_delta") => {
                        // Display-only thinking has nowhere to retain this, and
                        // retaining it would invite echoing a stale signature.
                    }
                    Some("input_json_delta") => {
                        if let Some(partial) =
                            delta.get("partial_json").and_then(Value::as_str)
                        {
                            self.slot(index).input.push_str(partial);
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(call) = self.calls.get(&index) {
                    if call.name.trim().is_empty() {
                        return Ok(());
                    }
                    let arguments = if call.input.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&call.input).unwrap_or_else(|_| json!({}))
                    };
                    // The name was announced at block start when present; a
                    // malformed stream can still complete a call whose start was
                    // never seen, and the engine treats that as an action.
                    let id = if call.id.is_empty() {
                        format!("call-{index}")
                    } else {
                        call.id.clone()
                    };
                    // Borrow ends before emitting below.
                    let finished = RawToolCall {
                        id,
                        name: call.name.clone(),
                        arguments,
                    };
                    // Reborrow mutably only to mark announcement state.
                    let announced = self.calls.get(&index).map(|c| c.announced).unwrap_or(false);
                    if !announced {
                        if let Some(slot) = self.calls.get_mut(&index) {
                            slot.announced = true;
                        }
                        on_chunk(ChatChunk::Tool(&finished.name));
                    }
                    // Store the finished call separately from partial slots.
                    self.order.retain(|i| *i != index);
                    self.order.push(index);
                    if let Some(slot) = self.calls.get_mut(&index) {
                        slot.input = serde_json::to_string(&finished.arguments)
                            .unwrap_or_else(|_| "{}".to_string());
                    }
                }
            }
            "message_delta" => {
                self.finish_reason = anthropic_stop_reason(
                    value["delta"]["stop_reason"].as_str(),
                );
                // Merged, not replaced. Anthropic's `message_delta.usage` carries
                // *only* `output_tokens` — both official streaming examples show
                // `{"usage":{"output_tokens":15}}` and nothing else — so replacing the
                // struct threw away the `input_tokens` correctly captured from
                // `message_start` and every streamed Anthropic seat reported a prompt
                // cost of zero. The input side is the dominant cost and the thing
                // that tracks context pressure.
                let delta = parse_usage(value);
                self.usage = match (self.usage, delta) {
                    // No earlier reading: take whatever this frame carried.
                    (None, d) => d,
                    // `message_delta` has no input count, so keep the one
                    // `message_start` gave us and take only the output side forward.
                    (Some(prev), Some(d)) => Some(crate::client::Usage {
                        prompt_tokens: prev.prompt_tokens,
                        completion_tokens: d.completion_tokens.max(prev.completion_tokens),
                    }),
                    (Some(prev), None) => Some(prev),
                };
            }
            "message_stop" | "ping" => {}
            "error" => {
                let detail = value
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Anthropic stream error");
                let kind = value
                    .get("error")
                    .and_then(|e| e.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("error");
                return Err(LlmError::Stream(format!("Anthropic {kind}: {detail}")));
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(
        mut self,
        on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
    ) -> ChatResponse {
        let mut calls = Vec::new();
        for index in std::mem::take(&mut self.order) {
            let Some(call) = self.calls.remove(&index) else {
                continue;
            };
            if call.name.trim().is_empty() {
                continue;
            }
            let arguments = if call.input.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&call.input).unwrap_or_else(|_| json!({}))
            };
            if !call.announced {
                on_chunk(ChatChunk::Tool(&call.name));
            }
            calls.push(RawToolCall {
                id: if call.id.is_empty() {
                    format!("call-{index}")
                } else {
                    call.id
                },
                name: call.name,
                arguments,
            });
        }
        ChatResponse {
            finish_reason: self.finish_reason,
            content: self.text,
            reasoning: self.reasoning,
            tool_calls: calls,
            notes: Vec::new(),
            usage: self.usage,
        }
    }
}

fn process_anthropic_line(
    line: &str,
    acc: &mut AnthropicAccumulator,
    on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
) -> Result<bool, LlmError> {
    let line = line.trim();
    let Some(payload) = line.strip_prefix("data:") else {
        return Ok(true);
    };
    let payload = payload.trim();
    if payload.is_empty() {
        return Ok(true);
    }
    if payload == "[DONE]" {
        return Ok(true);
    }
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        return Ok(true);
    };
    acc.absorb_event(&value, on_chunk)?;
    Ok(true)
}

async fn drain_anthropic_sse<S, B, E>(
    stream: S,
    on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
) -> Result<ChatResponse, LlmError>
where
    S: futures_util::Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    futures_util::pin_mut!(stream);
    let mut acc = AnthropicAccumulator::default();
    // Bytes, decoding only complete lines. See `drain_sse` in client.rs: socket
    // chunks are not aligned to codepoint boundaries, and decoding one on its own
    // replaces every byte of a split sequence with U+FFFD — which then parses fine,
    // because U+FFFD is legal inside a JSON string, and lands on the board as a
    // corrupted character with nothing to notice it.
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| LlmError::Stream(e.to_string()))?;
        buf.extend_from_slice(bytes.as_ref());
        if buf.len() > MAX_SSE_LINE && !buf.contains(&b'\n') {
            return Err(LlmError::Stream(format!(
                "one SSE line exceeded {MAX_SSE_LINE} bytes without a newline"
            )));
        }
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line = String::from_utf8_lossy(&buf[..=nl]).into_owned();
            buf.drain(..=nl);
            process_anthropic_line(&line, &mut acc, on_chunk)?;
        }
    }
    if !buf.is_empty() {
        let tail = String::from_utf8_lossy(&buf).into_owned();
        process_anthropic_line(tail.trim(), &mut acc, on_chunk)?;
    }
    Ok(acc.finish(on_chunk))
}

fn truncate(text: &str, n: usize) -> String {
    if text.len() <= n {
        // At most `n` bytes, so at most `n` chars: already fits.
        return text.to_string();
    }
    // `nth(n)` is where char number `n` (0-indexed) starts, so `text[..at]`
    // is the first `n` chars — byte-identical to `chars().take(n).collect()`
    // — found in a single partial scan instead of a count plus a take. `None`
    // means multibyte input with at most `n` chars despite the byte length.
    match text.char_indices().nth(n).map(|(i, _)| i) {
        Some(at) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

async fn stream_anthropic_once(
    client: &reqwest::Client,
    req: &ChatRequest,
    url: &str,
    body: &Value,
    on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
) -> Result<ChatResponse, LlmError> {
    let rb = apply_anthropic_headers(
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
        return Err(LlmError::Status {
            url: url.to_string(),
            status: status.as_u16(),
            body: truncate(&body, 800),
        });
    }
    drain_anthropic_sse(res.bytes_stream(), on_chunk).await
}

async fn one_shot_anthropic(
    client: &reqwest::Client,
    req: &ChatRequest,
    url: &str,
    body: &Value,
    on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
) -> Result<ChatResponse, LlmError> {
    let rb = apply_anthropic_headers(
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
    let text = res
        .text()
        .await
        .map_err(|e| LlmError::Stream(e.to_string()))?;
    if !status.is_success() {
        return Err(LlmError::Status {
            url: url.to_string(),
            status: status.as_u16(),
            body: truncate(&text, 800),
        });
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| LlmError::Stream(format!("{e}: {}", truncate(&text, 400))))?;
    let response = parse_anthropic_message(&value)?;
    for call in &response.tool_calls {
        on_chunk(ChatChunk::Tool(&call.name));
    }
    if !response.content.is_empty() {
        on_chunk(ChatChunk::Content(&response.content));
    }
    if !response.reasoning.is_empty() {
        on_chunk(ChatChunk::Reasoning(&response.reasoning));
    }
    Ok(response)
}

async fn attempt_anthropic(
    client: &reqwest::Client,
    req: &ChatRequest,
    url: &str,
    first_body: &Value,
    on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
) -> Result<ChatResponse, LlmError> {
    let mut notes = Vec::new();
    if req.endpoint.stream {
        match stream_anthropic_once(client, req, url, first_body, on_chunk).await {
            Ok(mut response) => {
                response.notes = notes;
                return Ok(response);
            }
            Err(e) => {
                notes.push(format!(
                    "streaming failed ({}), retrying as a single response",
                    truncate(&e.to_string(), 160)
                ));
                on_chunk(ChatChunk::Reset);
            }
        }
        // Same request, single-response shape: the streamed render with the
        // flag flipped, not a second walk of the messages.
        let mut plain = first_body.clone();
        if let Some(obj) = plain.as_object_mut() {
            obj.insert("stream".into(), Value::Bool(false));
        }
        let mut response = one_shot_anthropic(client, req, url, &plain, on_chunk).await?;
        response.notes.append(&mut notes);
        return Ok(response);
    }
    let mut response = one_shot_anthropic(client, req, url, first_body, on_chunk).await?;
    response.notes.append(&mut notes);
    Ok(response)
}

#[async_trait]
impl ChatBackend for AnthropicBackend {
    async fn chat(
        &self,
        req: &ChatRequest,
        on_chunk: &(dyn for<'a> Fn(ChatChunk<'a>) + Send + Sync),
    ) -> Result<ChatResponse, LlmError> {
        if !matches!(req.endpoint.provider, Provider::Anthropic) {
            return Err(LlmError::Stream(
                "Anthropic backend called for a non-Anthropic endpoint".into(),
            ));
        }
        let client = self.http_client(&req.endpoint)?;
        // Rendered once per request: the streaming and single-response shapes
        // differ only in the `stream` flag, so the fallback toggles that on
        // the same render instead of rebuilding the messages per URL tried.
        let first_body = build_anthropic_body(req, req.endpoint.stream)?;
        // Anthropic has one route per base, not the OpenAI fallback pair. Trying
        // another provider’s shape after a 404 would misreport a wrong address.
        let mut last: Option<LlmError> = None;
        for url in req.endpoint.anthropic_messages_urls() {
            match attempt_anthropic(&client, req, &url, &first_body, on_chunk).await {
                Ok(response) => return Ok(response),
                Err(error @ LlmError::Status { status: 404, .. }) => last = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(last.unwrap_or(LlmError::Stream("no Anthropic url to try".into())))
    }
}

fn parse_anthropic_model_list(value: &Value) -> Vec<String> {
    value["data"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|model| model.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub async fn list_anthropic_models(
    endpoint: &EndpointConfig,
) -> Result<Vec<String>, LlmError> {
    if !matches!(endpoint.provider, Provider::Anthropic) {
        return Err(LlmError::Stream(
            "Anthropic model listing called for a non-Anthropic endpoint".into(),
        ));
    }
    let client = client_for(endpoint)?;
    let api_key = anthropic_api_key(endpoint);
    let mut last: Option<LlmError> = None;
    for url in endpoint.anthropic_models_urls() {
        let mut rb = client
            .get(&url)
            .timeout(Duration::from_secs(endpoint.timeout_secs.clamp(5, 60)));
        if let Some(key) = api_key.clone() {
            rb = rb.header("x-api-key", key);
        }
        rb = rb.header("anthropic-version", ANTHROPIC_VERSION);
        for (name, value) in &endpoint.headers {
            rb = rb.header(name, value);
        }
        let res = match rb.send().await {
            Ok(response) => response,
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
            // Shared with the OpenAI-compatible paths, for the same reason: one
            // wording, and no path left showing a bare "unauthenticated".
            let error = crate::client::status_error(&url, status.as_u16(), &body, endpoint);
            if status.as_u16() == 404 {
                last = Some(error);
                continue;
            }
            return Err(error);
        }
        let value: Value =
            serde_json::from_str(&body).map_err(|e| LlmError::Stream(format!("{e}")))?;
        return Ok(parse_anthropic_model_list(&value));
    }
    Err(last.unwrap_or(LlmError::Stream("no Anthropic url to try".into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EndpointConfig;

    fn endpoint() -> EndpointConfig {
        EndpointConfig {
            provider: Provider::Anthropic,
            base_url: "https://api.anthropic.com".into(),
            ..EndpointConfig::default()
        }
    }

    fn request(endpoint: EndpointConfig) -> ChatRequest {
        ChatRequest {
            endpoint,
            model: "claude-test".into(),
            messages: vec![
                ChatMessage::system("Be decisive."),
                ChatMessage::user("What should stand?"),
            ],
            tools: vec![],
            tool_choice: "auto".into(),
            temperature: Some(1.7),
            max_tokens: None,
        }
    }

    #[test]
    fn anthropic_body_moves_system_out_of_messages_and_caps_output() {
        let body = build_anthropic_body(&request(endpoint()), true)
            .expect("Anthropic body");
        assert_eq!(body["model"], json!("claude-test"));
        assert_eq!(body["max_tokens"], json!(ANTHROPIC_DEFAULT_MAX_TOKENS));
        assert_eq!(body["system"], json!("Be decisive."));
        assert_eq!(
            body["messages"],
            json!([{"role": "user", "content": "What should stand?"}])
        );
        // Anthropic accepts 0..=1; the app allows wider OpenAI-style settings.
        assert_eq!(body["temperature"], json!(1.0));
        assert!(body.get("stream_options").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("chat_template_kwargs").is_none());
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn anthropic_tools_use_input_schema_and_object_choice() {
        let mut endpoint = endpoint();
        endpoint.reasoning_effort = Some("high".into());
        endpoint.thinking = Some(false);
        let mut req = request(endpoint);
        req.tools = vec![json!({
            "type": "function",
            "function": {
                "name": "vote",
                "description": "Rate a point.",
                "parameters": {
                    "type": "object",
                    "properties": {"point": {"type": "integer"}},
                    "required": ["point"],
                },
            },
        })];
        let body = build_anthropic_body(&req, false).expect("Anthropic tools");
        assert_eq!(
            body["tools"],
            json!([{
                "name": "vote",
                "description": "Rate a point.",
                "input_schema": {
                    "type": "object",
                    "properties": {"point": {"type": "integer"}},
                    "required": ["point"],
                },
            }])
        );
        assert_eq!(body["tool_choice"], json!({"type": "auto"}));
        // Thinking controls are display-only in this release. Sending them
        // would risk a generation-specific 400 for no supported behaviour.
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn anthropic_images_become_base64_blocks() {
        let mut message =
            ChatMessage::user("Which of these stands?");
        message.images = vec!["data:image/jpeg;base64,AAA".into()];
        let converted = anthropic_message(&message).expect("image message");
        assert_eq!(
            converted,
            json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "Which of these stands?"},
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/jpeg",
                            "data": "AAA",
                        },
                    },
                ],
            })
        );
    }

    /// The API validates `max_tokens` against the model's maximum *output* and
    /// 400s rather than clamping, so a default above every model's ceiling is not
    /// generous — it is total. This is the shape of that failure.
    #[test]
    fn the_default_output_ceiling_is_inside_every_current_models_limit() {
        // Lowest maximum output across the current Anthropic models: 32K on the
        // Opus 4.5 line, 64K on Sonnet/Haiku 4.5+.
        const SMALLEST_MAX_OUTPUT: u32 = 32_000;
        assert!(
            ANTHROPIC_DEFAULT_MAX_TOKENS <= SMALLEST_MAX_OUTPUT,
            "every Anthropic seat would 400 on every turn: default {} exceeds the \
             smallest current model ceiling {}",
            ANTHROPIC_DEFAULT_MAX_TOKENS,
            SMALLEST_MAX_OUTPUT
        );
        // And not so small that a verdict or a long turn gets truncated: the
        // engine asks for `None`, so this is the whole budget.
        assert!(
            ANTHROPIC_DEFAULT_MAX_TOKENS >= 8_000,
            "too small to finish a turn or write a verdict"
        );
    }

    #[test]
    fn anthropic_rejects_a_foreign_message_role_before_the_network() {
        let message = ChatMessage {
            role: "developer".into(),
            content: "hi".into(),
            images: Vec::new(),
            merged: Vec::new(),
        };
        assert!(anthropic_message(&message).is_err());
    }

    #[test]
    fn anthropic_rejects_forced_tool_choice() {
        assert!(anthropic_tool_choice("any").is_err());
        assert!(anthropic_tool_choice("pick-one").is_err());
    }

    #[test]
    fn anthropic_non_streaming_response_maps_stop_and_usage() {
        let response = parse_anthropic_message(&json!({
            "content": [
                {"type": "thinking", "thinking": "Check the record. ", "signature": "sig"},
                {"type": "text", "text": "Roses stand."},
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "vote",
                    "input": {"point": 1, "verdict": "win"},
                },
            ],
            "stop_reason": "max_tokens",
            "usage": {"input_tokens": 1200, "output_tokens": 340},
        }))
        .expect("Anthropic response");
        assert_eq!(response.content, "Roses stand.");
        assert_eq!(response.reasoning, "Check the record. ");
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "vote");
        assert_eq!(response.tool_calls[0].arguments["point"], json!(1));
        assert_eq!(response.finish_reason.as_deref(), Some("length"));
        assert_eq!(
            response.usage,
            Some(crate::client::Usage {
                prompt_tokens: 1200,
                completion_tokens: 340,
            })
        );
    }

    #[test]
    fn anthropic_stream_accumulates_text_thinking_tools_and_usage() {
        let events = [
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"First, \"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Roses \"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"stand.\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"vote\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"point\\\":\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"1}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"input_tokens\":25,\"output_tokens\":89}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ];
        let seen = std::sync::Mutex::new(Vec::<String>::new());
        let on_chunk = |chunk: ChatChunk<'_>| match chunk {
            ChatChunk::Content(text) => seen
                .lock()
                .expect("seen")
                .push(format!("content:{text}")),
            ChatChunk::Reasoning(text) => seen
                .lock()
                .expect("seen")
                .push(format!("thinking:{text}")),
            ChatChunk::Tool(name) => {
                seen.lock().expect("seen").push(format!("tool:{name}"))
            }
            ChatChunk::Reset => {}
        };
        let stream = futures_util::stream::iter(events.iter().map(|event| {
            Ok::<Vec<u8>, std::io::Error>(event.as_bytes().to_vec())
        }));
        let response = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(drain_anthropic_sse(stream, &on_chunk))
            .expect("Anthropic stream");
        assert_eq!(response.reasoning, "First, ");
        assert_eq!(response.content, "Roses stand.");
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].arguments["point"], json!(1));
        assert_eq!(response.finish_reason.as_deref(), Some("tool_use"));
        assert_eq!(
            response.usage,
            Some(crate::client::Usage {
                prompt_tokens: 25,
                completion_tokens: 89,
            })
        );
        assert_eq!(
            seen.into_inner().expect("seen"),
            vec![
                "thinking:First, ".to_string(),
                "content:Roses ".to_string(),
                "content:stand.".to_string(),
                "tool:vote".to_string()
            ]
        );
    }

    #[test]
    fn anthropic_stream_error_becomes_an_llm_error() {
        let events = [
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
        ];
        let stream = futures_util::stream::iter(events.iter().map(|event| {
            Ok::<Vec<u8>, std::io::Error>(event.as_bytes().to_vec())
        }));
        let error = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(drain_anthropic_sse(stream, &|_| {}))
            .expect_err("stream error should fail");
        assert!(matches!(error, LlmError::Stream(_)));
        assert!(error.to_string().contains("overloaded_error"));
    }

    #[test]
    fn anthropic_model_listing_reads_documented_ids() {
        assert_eq!(
            parse_anthropic_model_list(&json!({
                "data": [
                    {"type": "model", "id": "claude-test-1"},
                    {"type": "model", "id": "claude-test-2"},
                ],
            })),
            vec!["claude-test-1".to_string(), "claude-test-2".to_string()]
        );
        assert!(parse_anthropic_model_list(&json!({})).is_empty());
    }

    #[test]
    fn bearer_prefixed_keys_become_anthropic_api_keys() {
        let mut endpoint = endpoint();
        endpoint.api_key = Some("Bearer secret".into());
        assert_eq!(
            anthropic_api_key(&endpoint).as_deref(),
            Some("secret")
        );
    }

    /// `truncate` on multibyte input: same head as `chars().take(n)`, and no
    /// split characters — including input that fits by chars but not bytes.
    #[test]
    fn truncate_never_splits_multibyte_input() {
        let s = format!("{}{}", "🎉".repeat(10), "x".repeat(900));
        let old = if s.chars().count() <= 800 {
            s.clone()
        } else {
            format!("{}…", s.chars().take(800).collect::<String>())
        };
        let got = truncate(&s, 800);
        assert_eq!(got, old);
        assert_eq!(got.as_bytes(), old.as_bytes(), "byte output must match");
        assert!(!got.contains('�'), "a char was split: {got:?}");
        // Fits by chars (5) but not by bytes (20): untouched.
        let emoji = "🎉".repeat(5);
        assert_eq!(truncate(&emoji, 5), emoji);
        assert_eq!(truncate("abc", 10), "abc");
    }
}
