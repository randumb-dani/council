//! `council-core` — the deliberation engine behind the Council chamber.
//!
//! A debate is a shared board of points. Models raise them, attack them, cite
//! against them, and rate every one of them `win` / `lose` / `stalemate`.
//! Nothing a model says decides anything; only the board does.

pub mod anthropic;
pub mod client;
pub mod decide;
pub mod engine;
pub mod events;
pub mod prompt;
pub mod tools;
pub mod types;
pub mod web;

pub use anthropic::{
    list_anthropic_models, AnthropicBackend, ProviderBackend, ANTHROPIC_DEFAULT_MAX_TOKENS,
    ANTHROPIC_VERSION,
};
pub use client::{
    client_for, list_models, ChatBackend, ChatMessage, ChatRequest, ChatResponse, HttpBackend, LlmError,
};
pub use decide::{ClassifyRequest, Decision, DecisionBackend, JevBackend};
pub use engine::{Engine, EventSink, Snapshot};
pub use events::EngineEvent;
pub use types::*;
