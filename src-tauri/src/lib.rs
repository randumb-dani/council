//! The Tauri layer: a thin, dumb bridge. All of the deliberation lives in
//! `council-core`; this file only starts it, stops it, and forwards the event
//! stream to the webview.

use council_core::engine::{Engine, Snapshot};
use council_core::events::EngineEvent;
use council_core::types::{DebateConfig, EndpointConfig};
use council_core::HttpBackend;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

/// Every event the engine emits arrives in the webview on this channel.
pub const EVENT_CHANNEL: &str = "council://event";

type Council = Arc<Engine<HttpBackend>>;

#[tauri::command]
fn start_debate(engine: State<'_, Council>, config: DebateConfig) -> Result<(), String> {
    // The engine is shared, so a second call just returns; the webview is told
    // about that through the event stream rather than by failing here.
    if engine.is_running() {
        return Err("a debate is already running".into());
    }
    let engine = engine.inner().clone();
    tauri::async_runtime::spawn(async move {
        engine.run(config).await;
    });
    Ok(())
}

#[tauri::command]
fn stop_debate(engine: State<'_, Council>) {
    engine.stop();
}

#[tauri::command]
fn inject_note(engine: State<'_, Council>, text: String) {
    engine.inject(&text);
}

/// Answer the summarizer's waiting extension proposal. Errors when nothing is
/// waiting — the webview only offers the choice while a proposal is open.
#[tauri::command]
fn resolve_extension(engine: State<'_, Council>, approved: bool, rounds: u32) -> Result<String, String> {
    engine.resolve_extension(approved, rounds)
}

/// Abandon the turn currently in flight. Returns false when nobody is mid-turn.
#[tauri::command]
fn skip_turn(engine: State<'_, Council>) -> bool {
    engine.skip_turn()
}

#[tauri::command]
fn debate_state(engine: State<'_, Council>) -> Snapshot {
    engine.snapshot()
}

/// Probe an endpoint. Returns the models it serves, or the real reason it could
/// not be reached — this is the first thing anyone tries when nothing works, so
/// the error has to be actionable.
#[tauri::command]
async fn list_models(endpoint: EndpointConfig) -> Result<Vec<String>, String> {
    council_core::list_models(&endpoint)
        .await
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // The webview's own `download` attribute is unreliable on WebKitGTK:
        // the blob URL is ignored, the webview tries to navigate to it, and the
        // navigation is refused — so the button does nothing at all. A real save
        // dialog is the only export path that works everywhere.
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle: AppHandle = app.handle().clone();
            let sink = move |event: EngineEvent| {
                // A failed emit means the window went away; there is nothing
                // useful to do about it mid-debate.
                let _ = handle.emit(EVENT_CHANNEL, &event);
            };
            let engine: Council = Arc::new(Engine::new(HttpBackend::new(), Arc::new(sink)));
            app.manage(engine);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_debate,
            stop_debate,
            skip_turn,
            inject_note,
            resolve_extension,
            debate_state,
            list_models
        ])
        .run(tauri::generate_context!())
        .expect("error while running council");
}
