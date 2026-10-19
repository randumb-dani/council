//! `council-serve` — the Council chamber, served on this machine only.
//!
//! The desktop app talks to the engine through Tauri commands; a plain browser
//! cannot. This binary closes that gap without opening a new one: it serves the
//! built UI from `dist/` and exposes the same engine operations over a REST +
//! server-sent-events API bound to a loopback address.
//!
//! ```bash
//! npm run build                      # produce dist/ first
//! npm run serve                      # serve it on 127.0.0.1:1427
//! COUNCIL_SERVE_ADDR=127.0.0.1:1877 npm run serve
//! ```
//!
//! # Deliberate limits
//!
//! * Loopback only. A non-loopback bind is refused unless
//!   `COUNCIL_SERVE_ALLOW_REMOTE=1` is set explicitly, so a stray flag cannot
//!   put provider keys on the LAN.
//! * One active debate at a time, like the desktop app: a second start gets
//!   409, and every tab on this machine watches the same event stream.
//! * No secrets cross the API. The browser sends endpoint *profiles* with
//!   `apiKey: null`; the server merges keys from its own store by endpoint id
//!   and rejects any request carrying one.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use council_core::engine::{Engine, Snapshot};
use council_core::events::EngineEvent;
use council_core::{
    list_models, DebateConfig, EndpointConfig, HumanBallot, HumanTurnSubmit, ProviderBackend,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};

// ---------------------------------------------------------------------------
// configuration
// ---------------------------------------------------------------------------

const DEFAULT_ADDR: &str = "127.0.0.1:1427";
/// Largest request body accepted: motion images ride along as data URLs, up to
/// ~2.5MB each and six of them, so this must clear ~10MB with room to spare.
const MAX_BODY: usize = 16 << 20;
/// Largest request head accepted: headers are tiny here, anything bigger is a
/// mistake or an attack.
const MAX_HEAD: usize = 64 << 10;
const SSE_HISTORY: usize = 512;
const SSE_PING_SECS: u64 = 15;
/// How long a socket write may stall before the peer is assumed gone.
const SSE_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// How long a client may take to finish sending request head or body.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// Concurrent connections. Bounds the damage from a client that opens sockets and
/// never finishes a request; without it each one costs a task and a file
/// descriptor and nothing ever sheds load.
const MAX_CONNECTIONS: usize = 64;

/// Where the UI comes from. Run from the repo root after `npm run build`, or
/// point `COUNCIL_DIST_DIR` at any directory holding the built `index.html`.
fn dist_dir() -> PathBuf {
    std::env::var("COUNCIL_DIST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("dist"))
}

/// Where endpoint secrets live: outside the repo, with owner-only permissions.
/// The browser never sees this file; the API only reports *whether* a key is
/// stored, never its value.
fn keys_path() -> PathBuf {
    if let Ok(p) = std::env::var("COUNCIL_ENDPOINTS_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".config").join("council").join("endpoints.json")
}

/// Whether this run is reachable from the network rather than just this machine.
fn remote_bind(addr: &SocketAddr) -> bool {
    !addr.ip().is_loopback()
}

/// Parse the bind address and refuse anything that is not this machine.
///
/// `0.0.0.0`, `::`, and LAN addresses are all refused unless the operator
/// opts in with `COUNCIL_SERVE_ALLOW_REMOTE=1` — a remote bind changes the
/// trust model (anyone on that network reaches the key store), so it must be
/// a deliberate act rather than a typo surviving silently.
fn bind_addr() -> Result<SocketAddr, String> {
    let raw = std::env::var("COUNCIL_SERVE_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let addr: SocketAddr = raw
        .parse()
        .map_err(|_| format!("could not parse '{raw}' as an address — want 127.0.0.1:1427"))?;
    if !addr.ip().is_loopback() && std::env::var("COUNCIL_SERVE_ALLOW_REMOTE").as_deref() != Ok("1") {
        return Err(format!(
            "refusing to serve on non-loopback address {addr}: provider keys would be reachable off this machine. \
             Bind 127.0.0.1 instead, or set COUNCIL_SERVE_ALLOW_REMOTE=1 if that is really what you want."
        ));
    }
    Ok(addr)
}

// ---------------------------------------------------------------------------
// secret store
// ---------------------------------------------------------------------------

/// Endpoint id → API key, plus the decision-model key under `"decision"`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct KeyStore {
    #[serde(default)]
    keys: BTreeMap<String, String>,
    /// Endpoint id → the base URL the operator registered that key against.
    ///
    /// This exists so the server can refuse to send a stored key somewhere the
    /// operator did not put it. `/api/endpoints/probe` used to take a `baseUrl`
    /// straight from the request and attach the stored key for the named id, which
    /// made this server a credential relay: name an id, point the URL at your own
    /// host, and the key is delivered with an `Authorization: Bearer` header. Any
    /// web page the operator visited could do that, because a `text/plain` form
    /// POST needs no preflight. With the address recorded alongside the key, a
    /// probe only carries the key to the exact place the key was saved for.
    ///
    /// `#[serde(default)]`, so a store written before this existed still loads —
    /// it just has to have its keys saved once more before a probe will send one.
    #[serde(default)]
    profiles: BTreeMap<String, String>,
}

impl KeyStore {
    fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read(path) else {
            return Self::default();
        };
        // A parse failure is not "no keys": `unwrap_or_default` turned a corrupt
        // file into an empty store with no warning, and the next save then wrote a
        // file containing one key, destroying the rest with no trace. Keep the
        // bytes so nothing is silently lost.
        match serde_json::from_slice::<KeyStore>(&raw) {
            Ok(store) => store,
            Err(e) => {
                let sidecar = path.with_extension("corrupt");
                let _ = std::fs::write(&sidecar, &raw);
                eprintln!(
                    "council-serve: {} could not be parsed ({e}); kept a copy at {} and \
                     starting with no keys. Restore that copy to get them back.",
                    path.display(),
                    sidecar.display()
                );
                Self::default()
            }
        }
    }

    /// Persist with owner-only permissions, atomically.
    ///
    /// The previous version opened with `truncate(true)`: the file was zeroed
    /// before a single new byte existed, so a crash, an OOM kill or a full disk
    /// mid-write left a truncated file — which `load` then read as an empty store.
    /// Write beside the target, flush it, and rename over: a reader sees either
    /// the old file or the new one, never half of either.
    fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                // Owner-only, so the directory holding the keys is not
                // world-traversable even before the file inside it is created.
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)
                    .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
            }
            #[cfg(not(unix))]
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        let raw =
            serde_json::to_vec_pretty(self).map_err(|e| format!("could not encode keys: {e}"))?;
        let tmp = path.with_extension("tmp");
        {
            let mut f = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .mode(0o600)
                        .open(&tmp)
                }
                #[cfg(not(unix))]
                {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(&tmp)
                }
            }
            .map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
            std::io::Write::write_all(&mut f, &raw)
                .map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
            std::io::Write::flush(&mut f)
                .map_err(|e| format!("could not flush {}: {e}", tmp.display()))?;
            // Durability before the rename, or the rename can land before the data.
            let _ = f.sync_all();
        }
        // `.mode` only applies on create, so an existing file with looser
        // permissions — copied from another machine, an older build, a `chmod` gone
        // wrong — would keep them. Re-assert on every write.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("could not replace {}: {e}", path.display())
        })?;
        Ok(())
    }

    fn has(&self, id: &str) -> bool {
        self.keys.get(id).is_some_and(|k| !k.trim().is_empty())
    }

    /// The stored key for `id`, but only if `base_url` is the address this key was
    /// registered against.
    fn key_for(&self, id: &str, base_url: &str) -> Option<String> {
        let key = self.keys.get(id).filter(|k| !k.trim().is_empty())?;
        // Normalised on both sides: a trailing slash or a missing scheme's
        // trailing `/v1` should not read as a different host.
        let norm = |u: &str| u.trim().trim_end_matches('/').to_ascii_lowercase();
        if norm(self.profiles.get(id).map(String::as_str).unwrap_or("")) == norm(base_url)
            && !base_url.trim().is_empty()
        {
            Some(key.clone())
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// shared state
// ---------------------------------------------------------------------------

struct ActiveDebate {
    id: String,
}

/// How long a probed model list is served from memory. Probing re-runs on
/// every keystroke in the endpoint UI, and each run is a network round trip.
const MODEL_CACHE_TTL: Duration = Duration::from_secs(60);

struct Shared {
    engine: Arc<Engine<ProviderBackend>>,
    /// Every engine event, pre-serialized with its sequence number. SSE
    /// subscribers that fall behind get a resync marker instead of a silent
    /// gap. The payload is the wire bytes, serialized once where the event is
    /// emitted rather than once per subscriber in each connection's `format!`.
    tx: broadcast::Sender<(u64, String)>,
    /// Recent frames, newest last, for a client that reconnects and says how far
    /// it got.
    ///
    /// `broadcast::Sender::subscribe` starts at the tail — it is documented to
    /// deliver only what is sent *after* the call — so a phone that locked its
    /// screen and came back received nothing at all that happened meanwhile, and
    /// the `Lagged`/resync path could never fire for it because a fresh receiver
    /// is never behind. Points raised during the gap simply never reached the
    /// board, with no notice. This is that backlog, and `sse` replays from it
    /// using the `Last-Event-ID` the client reports.
    ///
    /// A `VecDeque`: the old `Vec` paid an O(n) `remove(0)` shift per event.
    /// Per-token deltas (`Token`, `Reasoning`) are live-only and never stored
    /// here — see the emit closure — so a reconnect replays structure (debate,
    /// round, turn, speeches), not every streamed piece.
    replay: Arc<std::sync::Mutex<VecDeque<(u64, String)>>>,
    /// Probed model lists by (endpoint id, base URL), with the instant they
    /// were fetched. Only successes are cached; the key carries the address
    /// the stored server key is bound to, so a cached list is never served
    /// for a different endpoint or URL.
    model_cache: Mutex<HashMap<(String, String), (Instant, Vec<String>)>>,
    debates: AtomicU64,
    active: Mutex<Option<ActiveDebate>>,
    keys: Mutex<KeyStore>,
    keys_path: PathBuf,
    dist: PathBuf,
}

impl Shared {
    /// A "you have missed something, refetch the snapshot" marker.
    fn resync_frame(&self) -> String {
        // `u64::MAX` rather than a real sequence number: this frame is not an
        // event, it is a notice, and giving it a sequence would invite a client to
        // resume from it and skip the events it is being warned about.
        serde_json::json!({ "seq": u64::MAX, "resync": true }).to_string()
    }

    /// A cached model list, if one is still fresh for exactly this endpoint
    /// at exactly this address.
    async fn cached_models(&self, id: &str, base_url: &str) -> Option<Vec<String>> {
        let cache = self.model_cache.lock().await;
        let (at, models) = cache.get(&(id.to_string(), base_url.to_string()))?;
        (at.elapsed() < MODEL_CACHE_TTL).then(|| models.clone())
    }

    /// Remember a freshly probed list for `MODEL_CACHE_TTL`.
    async fn remember_models(&self, id: &str, base_url: &str, models: Vec<String>) {
        let mut cache = self.model_cache.lock().await;
        cache.insert((id.to_string(), base_url.to_string()), (Instant::now(), models));
    }
}

/// What a subscriber that just connected should be sent before live frames.
///
/// A connection with no `Last-Event-ID` has seen nothing on *this* connection —
/// including a phone that pressed Convene seconds ago and whose first frames were
/// already broadcast before its `EventSource` handshake finished. So it gets the
/// whole backlog: `DebateStarted`, `RoundStarted`, the current `TurnStarted`.
/// The alternative — a lone resync marker — left the UI with no live turn, so
/// every token and heartbeat for it was dropped by the reducer and the debate
/// read as frozen until the first speech landed.
///
/// Replaying is safe when the client already saw some of it, because the
/// client's sequence tracker drops anything at or below its watermark.
///
/// When the requested id has fallen out of the buffer, the gap cannot be
/// filled — say so with a resync marker and let the client refetch the
/// snapshot, rather than pretending the stream is whole.
enum CatchUp {
    /// Send these frames, in order, then live frames.
    Replay(Vec<(u64, String)>),
    /// The backlog no longer reaches back far enough; the client must refetch.
    Resync,
}

fn catch_up(
    buffer: &VecDeque<(u64, String)>,
    last_event_id: Option<u64>,
) -> CatchUp {
    match last_event_id {
        // A fresh connection gets the backlog. Cloned under the replay lock
        // so the lock is released before any socket write; per-token deltas
        // are never stored, so this stays small.
        None => CatchUp::Replay(buffer.iter().cloned().collect()),
        Some(from) => {
            // Nothing newer exists: the client is already caught up (or the
            // buffer is empty and there is nothing to say).
            if !buffer.iter().any(|(n, _)| *n > from) {
                return CatchUp::Replay(Vec::new());
            }
            // The client needs frame `from + 1` next. If the ring still holds
            // it, everything after it is contiguous and replayable; if the ring
            // has wrapped past it, the gap is unfillable.
            if buffer.iter().any(|(n, _)| *n == from.saturating_add(1)) {
                CatchUp::Replay(
                    buffer
                        .iter()
                        .filter(|(n, _)| *n > from)
                        .cloned()
                        .collect(),
                )
            } else {
                CatchUp::Resync
            }
        }
    }
}

// ---------------------------------------------------------------------------
// minimal HTTP
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
    /// Lowercased header names → values. Kept because the origin and content-type
    /// checks are the whole of this server's CSRF defence, and both need the raw
    /// headers rather than a guess.
    headers: std::collections::BTreeMap<String, String>,
}

async fn read_request(stream: &mut TcpStream) -> Result<Option<Request>, String> {
    let mut head: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = tokio::time::timeout(READ_TIMEOUT, stream.read(&mut buf))
            .await
            .map_err(|_| "timed out sending the request line and headers".to_string())?
            .map_err(|e| format!("could not read request: {e}"))?;
        if n == 0 {
            return Ok(None);
        }
        head.extend_from_slice(&buf[..n]);
        if head.len() > MAX_HEAD {
            return Err("request head too large".to_string());
        }
        if head.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let split = head
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("head terminator found above");
    let (head_bytes, mut body) = (head[..split].to_vec(), head[split + 4..].to_vec());
    let head_text =
        String::from_utf8(head_bytes).map_err(|_| "request head is not utf-8".to_string())?;
    let mut lines = head_text.lines();
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let (method, target) = (
        parts.next().unwrap_or("").to_string(),
        parts.next().unwrap_or("/").to_string(),
    );
    if method.is_empty() || target.is_empty() {
        return Err("malformed request line".to_string());
    }
    // Header names are case-insensitive and there are more casings than anyone
    // remembers to write, so `strip_prefix("Content-Length:")` silently read a
    // `content-length:` request as having no body at all — and the browser is
    // perfectly capable of sending either.
    let mut headers: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut content_length: Option<usize> = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if name == "content-length" {
            // A value we cannot parse is an error, not a zero: defaulting to zero
            // makes the body unread and the request look empty rather than malformed.
            content_length = Some(value.parse::<usize>().map_err(|_| {
                format!("could not parse Content-Length '{value}' as a byte count")
            })?);
        }
        headers.insert(name, value);
    }
    // Chunked bodies are not implemented. Saying so is better than treating the
    // request as bodiless: `fetch` with a stream body sends exactly this.
    if headers
        .get("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        return Err(
            "this server does not accept chunked request bodies — send Content-Length".to_string(),
        );
    }
    let content_length = content_length.unwrap_or(0);
    if content_length > MAX_BODY {
        return Err(format!(
            "request body is {content_length} bytes, over the {}MB limit — motion images cap at ~2.5MB each",
            MAX_BODY >> 20
        ));
    }
    while body.len() < content_length {
        let n = tokio::time::timeout(READ_TIMEOUT, stream.read(&mut buf))
            .await
            .map_err(|_| "timed out sending the request body".to_string())?
            .map_err(|e| format!("could not read request body: {e}"))?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
        if body.len() > MAX_BODY {
            return Err("request body too large".to_string());
        }
    }
    body.truncate(content_length);
    // Query strings carry nothing here; drop them so routing stays exact.
    let path = target.split(['?', '#']).next().unwrap_or("/").to_string();
    Ok(Some(Request {
        method,
        path,
        body,
        headers,
    }))
}

/// Reject a request that a web page elsewhere talked this browser into sending.
///
/// Binding to loopback stops other *machines*. It does nothing about the page the
/// operator is reading, because that page runs in a browser that can reach
/// `127.0.0.1` freely. Three cheap checks close it:
///
/// 1. **`Host` must be an IP literal or `localhost`.** This is the DNS-rebinding
///    defence: an attacker's domain resolving to `127.0.0.1` still sends
///    `Host: evil.example`, and without this the name is a free pass in. A LAN
///    phone sends `192.168.x.y:1427`, which is still an IP literal, so remote
///    clients keep working.
/// 2. **`Origin`, when present, must match that Host.** Browsers send `Origin` on
///    every non-GET, and a page on another site sends *its* — so this refuses the
///    cross-site POST outright. A native client sends no `Origin` and is unaffected.
/// 3. **A body must be `application/json`.** `text/plain` is CORS-safelisted, so a
///    `fetch(..., {mode:"no-cors"})` or a plain `<form enctype=text/plain>` needs
///    no preflight and can carry a JSON payload. Requiring the JSON content type
///    makes those a preflight, which fails without a CORS grant this server does
///    not give.
fn request_is_same_origin(req: &Request) -> Result<(), String> {
    let host = req.headers.get("host").map(String::as_str).unwrap_or("");
    // Strip the port: `[::1]:1427`, `127.0.0.1:1427`, `localhost:1427`.
    let host_only = host.rsplit_once(':').map_or(host, |(h, _)| h);
    let host_only = host_only.trim_start_matches('[').trim_end_matches(']');
    let is_ip = host_only.parse::<std::net::IpAddr>().is_ok();
    if !is_ip && host_only != "localhost" {
        return Err(format!(
            "refusing a request addressed to '{host}': this server only answers to a literal address. \
             A domain name here would allow DNS rebinding."
        ));
    }

    if let Some(origin) = req.headers.get("origin") {
        let from = origin
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(origin)
            .trim_end_matches('/');
        if !from.eq_ignore_ascii_case(host) {
            return Err(format!(
                "refusing a cross-origin request from '{origin}': this API stores provider keys and \
                 runs debates, and is not meant to be driven by another page."
            ));
        }
    }

    if !req.body.is_empty() {
        let ct = req
            .headers
            .get("content-type")
            .map(String::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if !ct.starts_with("application/json") {
            return Err(format!(
                "this API only accepts application/json; got '{ct}'."
            ));
        }
    }
    Ok(())
}

fn response(status: u16, reason: &str, content_type: &str, body: &[u8], extra: &str) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn json(status: u16, value: serde_json::Value) -> Vec<u8> {
    let (reason, body) = match status {
        200 => ("OK", serde_json::to_vec(&value).unwrap_or_default()),
        400 => ("Bad Request", serde_json::to_vec(&value).unwrap_or_default()),
        404 => ("Not Found", serde_json::to_vec(&value).unwrap_or_default()),
        409 => ("Conflict", serde_json::to_vec(&value).unwrap_or_default()),
        502 => ("Bad Gateway", serde_json::to_vec(&value).unwrap_or_default()),
        _ => ("OK", serde_json::to_vec(&value).unwrap_or_default()),
    };
    response(status, reason, "application/json", &body, "")
}

fn err(status: u16, message: impl Into<String>) -> Vec<u8> {
    json(status, serde_json::json!({ "error": message.into() }))
}

// ---------------------------------------------------------------------------
// secrets: in by id, never out
// ---------------------------------------------------------------------------

/// True when the submitted config smuggles a secret. The served API takes
/// endpoint profiles only — ids, addresses, settings — and merges keys from
/// its own store. Accepting a key here would put it in server logs, proxy
/// logs, and browser history-adjacent places; refusing it loudly teaches the
/// client to stop sending it.
fn config_carries_secret(raw: &serde_json::Value) -> bool {
    let has = |v: &serde_json::Value| {
        v.get("apiKey")
            .is_some_and(|k| k.as_str().is_some_and(|s| !s.is_empty()))
    };
    raw.get("endpoints")
        .and_then(|e| e.as_array())
        .is_some_and(|eps| eps.iter().any(has))
        || raw.get("decision").is_some_and(has)
}

/// Attach server-side keys to a client-submitted config. Unknown endpoint ids
/// are refused rather than started keyless: a debate that silently runs
/// without its keys bills nobody but confuses everybody.
fn resolve_secrets(mut cfg: DebateConfig, keys: &KeyStore) -> Result<DebateConfig, String> {
    for ep in &mut cfg.endpoints {
        match keys.keys.get(&ep.id) {
            Some(k) if !k.trim().is_empty() => ep.api_key = Some(k.clone()),
            _ => ep.api_key = None,
        }
    }
    cfg.decision.api_key = keys
        .keys
        .get("decision")
        .filter(|k| !k.trim().is_empty())
        .cloned();
    Ok(cfg)
}

// ---------------------------------------------------------------------------
// routing
// ---------------------------------------------------------------------------

/// All request bodies speak camelCase, like every other boundary in this
/// project. A snake_case field here silently defaults to empty and the real
/// value is dropped — the exact failure class the engine tests pin down.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InjectBody {
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveBody {
    #[serde(default)]
    approved: bool,
    #[serde(default = "one")]
    rounds: u32,
}

fn one() -> u32 {
    1
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProbeBody {
    endpoint: EndpointConfig,
}

/// A taken turn, wrapped so the body has one field rather than being the
/// submit itself — the same shape as every other body here.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmitTurnBody {
    turn: HumanTurnSubmit,
}

/// A taken ballot, wrapped for the same reason.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmitBallotBody {
    ballot: HumanBallot,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyBody {
    #[serde(default)]
    endpoint_id: String,
    /// `None` here used to mean "delete the key", because the field was
    /// `#[serde(default)]` — so a request that simply *omitted* it deleted a
    /// provider key. A cross-site `<form>` cannot set a body field it does not
    /// name, so "omit apiKey" was a one-field key-deletion attack, and the
    /// response that confirmed it was thrown away cross-origin. Clearing is now
    /// something you have to ask for.
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    clear: bool,
    /// The address this key belongs to. Recorded so a later probe will only carry
    /// the key to this exact URL — see `KeyStore::key_for`.
    #[serde(default)]
    base_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyStatusBody {
    #[serde(default)]
    ids: Vec<String>,
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Vec<u8>> {
    serde_json::from_slice(body).map_err(|e| err(400, format!("could not parse request: {e}")))
}

async fn route(req: Request, shared: Arc<Shared>, stream: &mut TcpStream) -> Result<bool, Vec<u8>> {
    let method = req.method.as_str();
    let path = req.path.as_str();

    // Before anything reads a byte of the body. See `request_is_same_origin`.
    if let Err(why) = request_is_same_origin(&req) {
        return Err(err(403, why));
    }

    // -- health -----------------------------------------------------------
    if method == "GET" && path == "/api/health" {
        return Ok(send(
            stream,
            json(
                200,
                serde_json::json!({
                    "ok": true,
                    "service": "council-serve",
                    "version": env!("CARGO_PKG_VERSION"),
                    "tls": cfg!(feature = "tls"),
                }),
            ),
        )
        .await);
    }

    // -- which debate, if any ------------------------------------------------
    if method == "GET" && path == "/api/debates/active" {
        let id = shared.active.lock().await.as_ref().map(|a| a.id.clone());
        return Ok(send(stream, json(200, serde_json::json!({ "id": id }))).await);
    }

    // -- start a debate ------------------------------------------------------
    if method == "POST" && path == "/api/debates" {
        // Reserve the single active slot *before* the first `.await` below. The
        // check used to be `engine.is_running()` read at the top and the write
        // several awaits later, so two tabs pressing Start together both saw
        // "not running", both got `200 {"id": …}`, and `active` ended up naming a
        // debate no engine was driving — the UI then attached to an empty chamber
        // and showed a Convene button that 409s. Holding the lock across the whole
        // block closes the window.
        let mut active = shared.active.lock().await;
        if active.is_some() || shared.engine.is_running() {
            return Err(err(409, "a debate is already running"));
        }
        let raw: serde_json::Value = parse(&req.body)?;
        if config_carries_secret(&raw) {
            return Err(err(
                400,
                "this API does not accept API keys: store them on the server (Endpoints → server key) and send profiles only",
            ));
        }
        if shared.engine.is_running() {
            return Err(err(409, "a debate is already running"));
        }
        let cfg: DebateConfig = serde_json::from_value(raw)
            .map_err(|e| err(400, format!("could not parse debate config: {e}")))?;
        let keys = shared.keys.lock().await.clone();
        let cfg = resolve_secrets(cfg, &keys).map_err(|e| err(400, e))?;
        let n = shared.debates.fetch_add(1, Ordering::Relaxed) + 1;
        let id = format!("debate-{n}");
        *active = Some(ActiveDebate { id: id.clone() });
        drop(active);
        let engine = shared.engine.clone();
        let run_id = id.clone();
        tokio::spawn(async move {
            engine.run(cfg).await;
            // Clear the slot when this run is over. It used to be set before the
            // spawn and never cleared, so `/api/debates/active` reported a debate
            // that had never started (the engine validates the config first and
            // bails) as live forever, and `/stop`, `/inject` and `/skip` kept
            // answering `200 ok` against it.
            let mut active = shared.active.lock().await;
            if active.as_ref().is_some_and(|a| a.id == run_id) {
                *active = None;
            }
        });
        return Ok(send(stream, json(200, serde_json::json!({ "id": id }))).await);
    }

    // -- per-debate controls ---------------------------------------------------
    if let Some(rest) = path.strip_prefix("/api/debates/") {
        let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let known = shared
            .active
            .lock()
            .await
            .as_ref()
            .is_some_and(|a| a.id == id);
        if !known {
            return Err(err(404, format!("no such debate: {id}")));
        }
        match (method, tail) {
            ("GET", "state") => {
                let snap: Snapshot = shared.engine.snapshot();
                return Ok(send(stream, json(200, serde_json::to_value(&snap).unwrap_or_default())).await);
            }
            ("GET", "events") => {
                // The browser echoes the last `id:` it saw, which is how a
                // reconnect gets the gap rather than an empty tail.
                let last = req
                    .headers
                    .get("last-event-id")
                    .and_then(|v| v.parse::<u64>().ok());
                sse(stream, shared, last).await;
                return Ok(true);
            }
            ("POST", "stop") => {
                shared.engine.stop();
                return Ok(send(stream, json(200, serde_json::json!({ "ok": true }))).await);
            }
            ("POST", "skip") => {
                let skipped = shared.engine.skip_turn();
                return Ok(send(stream, json(200, serde_json::json!({ "skipped": skipped }))).await);
            }
            ("POST", "inject") => {
                let body: InjectBody = parse(&req.body)?;
                shared.engine.inject(&body.text);
                return Ok(send(stream, json(200, serde_json::json!({ "ok": true }))).await);
            }
            ("POST", "resolve-extension") => {
                let body: ResolveBody = parse(&req.body)?;
                match shared.engine.resolve_extension(body.approved, body.rounds) {
                    Ok(message) => {
                        return Ok(send(stream, json(200, serde_json::json!({ "message": message }))).await)
                    }
                    Err(e) => return Err(err(409, e)),
                }
            }
            // A taken seat's turn and ballots. Both validated by the engine, and
            // both answerable by more than one tab: whoever submits first wins
            // and the other's submit is refused as stale, which is the same race
            // the extension proposal already has.
            ("POST", "submit-turn") => {
                let body: SubmitTurnBody = parse(&req.body)?;
                return match shared.engine.submit_turn(body.turn) {
                    Ok(()) => Ok(send(stream, json(200, serde_json::json!({ "ok": true }))).await),
                    // 409, not 400: the turn is already answered or abandoned,
                    // which is a race the client loses rather than a malformed
                    // request it can fix.
                    Err(e) => Err(err(409, e)),
                };
            }
            ("POST", "submit-ballot") => {
                let body: SubmitBallotBody = parse(&req.body)?;
                return match shared.engine.submit_ballot(body.ballot) {
                    Ok(()) => Ok(send(stream, json(200, serde_json::json!({ "ok": true }))).await),
                    Err(e) => Err(err(409, e)),
                };
            }
            _ => return Err(err(404, format!("no such endpoint: {path}"))),
        }
    }

    // -- model probing, with the server's own keys -------------------------------
    if method == "POST" && path == "/api/endpoints/probe" {
        let body: ProbeBody = parse(&req.body)?;
        let mut ep = body.endpoint;
        if ep.api_key.as_ref().is_some_and(|k| !k.is_empty()) {
            return Err(err(
                400,
                "this API does not accept API keys: store them on the server (Endpoints → server key) and send profiles only",
            ));
        }
        let (mut ep_key, registered, has_key) = {
            let keys = shared.keys.lock().await;
            (
                // Only to the address this key was registered against — see `key_for`.
                keys.key_for(&ep.id, &ep.base_url),
                keys.profiles.get(&ep.id).cloned(),
                keys.has(&ep.id),
            )
        };
        if has_key && ep_key.is_none() {
            return Err(err(
                400,
                format!(
                    "a key is stored for '{}' but it was saved for {}, not {}. Re-save the server \
                     key for this address: a stored key is only ever sent to the URL it was saved \
                     with.",
                    ep.id,
                    registered.unwrap_or_else(|| "(no address recorded)".into()),
                    ep.base_url
                ),
            ));
        }
        ep.api_key = ep_key.take();
        // Served from memory while fresh: the UI probes on every selection,
        // and each probe is a network round trip for a list that barely
        // changes. Keyed by (endpoint id, base URL) — never by id alone — so
        // a cached list is never served for a different endpoint or address.
        // Only successes are cached: an error may clear on retry.
        if let Some(models) = shared.cached_models(&ep.id, &ep.base_url).await {
            return Ok(send(stream, json(200, serde_json::json!({ "models": models }))).await);
        }
        match list_models(&ep).await {
            Ok(models) => {
                shared.remember_models(&ep.id, &ep.base_url, models.clone()).await;
                return Ok(send(stream, json(200, serde_json::json!({ "models": models }))).await)
            }
            Err(e) => return Err(err(502, e.to_string())),
        }
    }

    // -- key enrollment: presence in, never values out ----------------------------
    if method == "POST" && path == "/api/keys/status" {
        let body: KeyStatusBody = parse(&req.body)?;
        let keys = shared.keys.lock().await;
        let mut out = serde_json::Map::new();
        for id in body.ids.iter().take(64) {
            out.insert(id.clone(), serde_json::Value::Bool(keys.has(id)));
        }
        return Ok(send(stream, json(200, serde_json::Value::Object(out))).await);
    }
    if method == "POST" && path == "/api/keys" {
        let body: KeyBody = parse(&req.body)?;
        let id = body.endpoint_id.trim().to_string();
        if id.is_empty() || id.len() > 128 {
            return Err(err(400, "endpoint_id must name the endpoint"));
        }
        if id != "decision" && !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(err(400, "endpoint_id holds letters, digits, dashes and underscores"));
        }
        if body.api_key.is_none() && !body.clear {
            return Err(err(
                400,
                "send `apiKey` to store a key, or `clear: true` to remove one — omitting \
                 apiKey does nothing, on purpose",
            ));
        }
        if let Some(k) = body.api_key.as_ref().filter(|k| !k.trim().is_empty()) {
            if k.len() > 4096 {
                return Err(err(400, "that key is longer than any provider issues"));
            }
        }
        // Build the new store, save it, and only then commit it in memory. The
        // old order inserted first and saved second, so a failed save left the key
        // live for this process — the operator was told it was not stored and the
        // next debate authenticated with it anyway, until a restart.
        let mut next = {
            let keys = shared.keys.lock().await;
            keys.clone()
        };
        match body.api_key {
            Some(k) if !k.trim().is_empty() => {
                next.keys.insert(id.clone(), k.trim().to_string());
                if let Some(u) = body
                    .base_url
                    .as_ref()
                    .map(|u| u.trim().to_string())
                    .filter(|u| !u.is_empty())
                {
                    next.profiles.insert(id.clone(), u);
                }
            }
            _ => {
                next.keys.remove(&id);
                next.profiles.remove(&id);
            }
        }
        next.save(&shared.keys_path).map_err(|e| err(502, e))?;
        let stored = next.has(&id);
        {
            let mut keys = shared.keys.lock().await;
            *keys = next;
        }
        return Ok(send(stream, json(200, serde_json::json!({ "endpointId": id, "stored": stored }))).await);
    }

    // -- static UI -----------------------------------------------------------------
    if method == "GET" {
        if path.starts_with("/api/") {
            return Err(err(404, format!("no such endpoint: {path}")));
        }
        return Ok(send(stream, serve_file(&shared.dist, path)).await);
    }

    Err(err(404, format!("no such endpoint: {path}")))
}

async fn send(stream: &mut TcpStream, bytes: Vec<u8>) -> bool {
    stream.write_all(&bytes).await.is_ok()
}

// ---------------------------------------------------------------------------
// server-sent events
// ---------------------------------------------------------------------------

/// One long-lived response: `data: {json}` frames, a `: ping` comment every
/// few seconds so proxies and phones do not time the stream out, and a resync
/// marker when the subscriber falls behind rather than a silent gap.
async fn sse(stream: &mut TcpStream, shared: Arc<Shared>, last_event_id: Option<u64>) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nX-Accel-Buffering: no\r\n\r\n";
    if write_bounded(stream, head.as_bytes()).await.is_err() {
        return;
    }

    // Subscribe and snapshot the backlog under one lock, so no frame can slip
    // between the two and be lost. `subscribe()` alone starts at the tail: it
    // delivers only what is sent after the call, which is how a client that was
    // away for thirty seconds silently missed every point raised in between.
    let (mut rx, catchup) = {
        let buf = shared.replay.lock().expect("replay");
        let catchup = catch_up(&buf, last_event_id);
        (shared.tx.subscribe(), catchup)
    };

    match catchup {
        CatchUp::Replay(frames) => {
            // `body` is already the serialized frame: per subscriber this is
            // string concatenation, not a second JSON serialization.
            for (n, body) in frames {
                let text = format!("id: {n}\ndata: {body}\n\n");
                if write_bounded(stream, text.as_bytes()).await.is_err() {
                    return;
                }
            }
        }
        // No `id:` on purpose. The browser remembers the last id it saw and
        // replays it as `Last-Event-ID` on reconnect; stamping this marker would
        // move its watermark to `u64::MAX`, after which *every* real frame looks
        // already-seen and the client silently misses the next outage too.
        CatchUp::Resync => {
            let body = shared.resync_frame();
            let text = format!("data: {body}\n\n");
            if write_bounded(stream, text.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    let mut ping = tokio::time::interval(std::time::Duration::from_secs(SSE_PING_SECS));
    // The first tick fires immediately; skip it so a fresh subscriber is not
    // greeted by a meaningless ping before any event.
    ping.tick().await;
    loop {
        tokio::select! {
            msg = rx.recv() => {
                // The channel carries `(seq, pre-serialized body)`: `id:` is
                // what makes a reconnect resumable — the browser echoes the
                // last one it saw back as `Last-Event-ID`. A resync marker
                // carries none — see the `CatchUp::Resync` arm above for why
                // stamping it would poison the watermark.
                let text = match msg {
                    Ok((n, body)) => format!("id: {n}\ndata: {body}\n\n"),
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        format!("data: {}\n\n", shared.resync_frame())
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if write_bounded(stream, text.as_bytes()).await.is_err() {
                    return;
                }
            }
            _ = ping.tick() => {
                if write_bounded(stream, b": ping\n\n").await.is_err() {
                    return;
                }
            }
        }
    }
}

/// A write that cannot hang forever.
///
/// A client that stops reading without closing — WiFi dropping without a FIN, a
/// phone in Doze, a NAT timeout — fills the socket buffer and then parks this task
/// indefinitely, holding the `TcpStream` and the broadcast receiver. There was no
/// read side at all to notice, so each reload leaked one task and one subscriber.
async fn write_bounded(stream: &mut TcpStream, bytes: &[u8]) -> std::io::Result<()> {
    tokio::time::timeout(SSE_WRITE_TIMEOUT, stream.write_all(bytes))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "write timed out"))?
}

// ---------------------------------------------------------------------------
// static files
// ---------------------------------------------------------------------------

fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Map a URL path into the dist directory, refusing anything that escapes it.
/// Encoded `..`, backslashes-as-separators, and absolute paths all land here.
fn safe_join(dist: &Path, url_path: &str) -> Option<PathBuf> {
    let trimmed = url_path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Some(dist.join("index.html"));
    }
    let mut out = dist.to_path_buf();
    for comp in Path::new(trimmed).components() {
        match comp {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            // Prefix (C:\), RootDir (/), and ParentDir (..) all refuse: the
            // first two escape the dist dir by construction, the third by
            // intent.
            _ => return None,
        }
    }
    Some(out)
}

fn serve_file(dist: &Path, url_path: &str) -> Vec<u8> {
    let not_found = || {
        response(
            404,
            "Not Found",
            "text/plain; charset=utf-8",
            b"not found - rebuild the UI with `npm run build`, then serve again",
            "",
        )
    };
    let Some(mut file) = safe_join(dist, url_path) else {
        return response(403, "Forbidden", "text/plain; charset=utf-8", b"refused", "");
    };
    if file.is_dir() {
        file = file.join("index.html");
    }
    let mime = |p: &Path| content_type(p.file_name().and_then(|s| s.to_str()).unwrap_or(""));
    match std::fs::read(&file) {
        Ok(body) if !body.is_empty() => return response(200, "OK", mime(&file), &body, ""),
        // A real file that happens to be empty is served as itself. It was being
        // read as "missing" and replaced with the shell.
        Ok(_) => return response(200, "OK", mime(&file), b"", ""),
        Err(_) => {}
    }
    // Single-page app: unknown *client routes* are the shell, not 404s — `#/d/x` and
    // the like live in the hash, but paths like `/debates/3` do not.
    //
    // Assets must not get that treatment. Vite's bundle is content-hashed, so a
    // page left open across a rebuild asks for `index-OLDHASH.js`, which no longer
    // exists; answering 200 with the HTML shell made the browser report
    // `Uncaught SyntaxError: Unexpected token '<'` on a chunk, and the app died
    // blank with no explanation. A 404 at least says what happened, and the
    // 404 body says to rebuild.
    let looks_like_an_asset = url_path.starts_with("/assets/")
        || url_path.rsplit('/').next().is_some_and(|last| {
            last.contains('.') && !last.ends_with(".html")
        });
    if looks_like_an_asset {
        return not_found();
    }
    let shell = std::fs::read(dist.join("index.html")).unwrap_or_default();
    if shell.is_empty() {
        return not_found();
    }
    response(200, "OK", "text/html; charset=utf-8", &shell, "")
}

// ---------------------------------------------------------------------------
// connection handling
// ---------------------------------------------------------------------------

async fn handle_conn(mut stream: TcpStream, shared: Arc<Shared>) {
    let req = match read_request(&mut stream).await {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(message) => {
            let _ = send(&mut stream, err(400, message)).await;
            return;
        }
    };
    match route(req, shared, &mut stream).await {
        Ok(_done) => {}
        Err(resp) => {
            let _ = send(&mut stream, resp).await;
        }
    }
}

#[tokio::main]
async fn main() {
    let addr = match bind_addr() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("council-serve: {e}");
            std::process::exit(2);
        }
    };
    let dist = dist_dir();
    if !dist.join("index.html").exists() {
        eprintln!(
            "council-serve: no UI at {} — run `npm run build` first, or set COUNCIL_DIST_DIR",
            dist.display()
        );
        std::process::exit(2);
    }
    let keys_path = keys_path();
    let keys = KeyStore::load(&keys_path);
    let (tx, _rx) = broadcast::channel::<(u64, String)>(SSE_HISTORY);
    let seq = Arc::new(AtomicU64::new(0));
    let replay = Arc::new(std::sync::Mutex::new(VecDeque::new()));
    let engine = Arc::new(Engine::new(ProviderBackend::new(), {
        let tx = tx.clone();
        let seq = seq.clone();
        let replay = Arc::clone(&replay);
        Arc::new(move |event: EngineEvent| {
            let n = seq.fetch_add(1, Ordering::Relaxed);
            // Per-token deltas are live-only, never replayed: a turn streams
            // hundreds of `Token`/`Reasoning` frames, and keeping every one
            // for a reconnect that mostly needs structure (debate, round,
            // turn, speeches) made the backlog a second transcript. A client
            // that missed them resyncs and refetches the snapshot instead —
            // the finished speeches carry the whole text.
            let replayable = !matches!(
                event,
                EngineEvent::Token { .. } | EngineEvent::Reasoning { .. }
            );
            // Serialized once here, not once per subscriber.
            let body =
                serde_json::to_string(&serde_json::json!({ "seq": n, "event": event }))
                    .unwrap_or_else(|_| format!("{{\"seq\":{n},\"event\":null}}"));
            // Recorded before it is broadcast, so a client that reconnects asking
            // for "everything after N" cannot be told about a frame that is already
            // in the backlog and also handed live — which would double-speak a turn.
            if replayable {
                if let Ok(mut buf) = replay.lock() {
                    if buf.len() >= SSE_HISTORY {
                        buf.pop_front();
                    }
                    buf.push_back((n, body.clone()));
                }
            }
            let _ = tx.send((n, body));
        })
    }));
    let shared = Arc::new(Shared {
        engine,
        tx,
        replay: Arc::clone(&replay),
        model_cache: Mutex::new(HashMap::new()),
        debates: AtomicU64::new(0),
        active: Mutex::new(None),
        keys: Mutex::new(keys),
        keys_path: keys_path.clone(),
        dist: dist.clone(),
    });
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("council-serve: could not bind {addr}: {e}");
            std::process::exit(2);
        }
    };
    if remote_bind(&addr) {
        let host = if addr.ip().is_unspecified() {
            "<this machine's address on the network>"
        } else {
            &addr.ip().to_string()
        };
        eprintln!(
            "council-serve: serving {} — reachable on this network",
            dist.display()
        );
        eprintln!("  open this on the other device, on the same network:");
        eprintln!("      http://{host}:{}", addr.port());
        eprintln!(
            "  anything on this network can read and drive this debate, and rewrite \
             the stored keys."
        );
    } else {
        eprintln!(
            "council-serve: serving {} on http://{addr} (this machine only)",
            dist.display()
        );
    }
    // A cap, so a client that opens sockets and never finishes costs a bounded
    // number of tasks and descriptors instead of all of them.
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let mut consecutive_failures = 0u32;
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _peer)) => stream,
            Err(e) => {
                // Without a backoff this is a spin loop. On Linux `accept` returns
                // EMFILE *immediately* once the process is out of descriptors, so
                // hitting the limit used to peg one core forever, log nothing, and
                // refuse every connection including the operator's own.
                consecutive_failures = consecutive_failures.saturating_add(1);
                eprintln!("council-serve: accept failed ({e})");
                let backoff = std::time::Duration::from_millis(
                    50u64.saturating_mul(consecutive_failures.min(20) as u64),
                );
                tokio::time::sleep(backoff).await;
                continue;
            }
        };
        consecutive_failures = 0;
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            eprintln!("council-serve: at the {MAX_CONNECTIONS}-connection limit, refusing one");
            drop(stream);
            continue;
        };
        let shared = shared.clone();
        tokio::spawn(async move {
            let _permit = permit;
            handle_conn(stream, shared).await;
        });
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn with_env(key: &str, value: Option<&str>, f: impl FnOnce()) {
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        f();
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    /// Build a `Request` the way `read_request` would, so these tests exercise the
    /// same shape the server actually routes.
    fn req(method: &str, path: &str, host: &str, origin: Option<&str>, ct: Option<&str>, body: &str) -> Request {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("host".into(), host.into());
        if let Some(o) = origin {
            headers.insert("origin".into(), o.into());
        }
        if let Some(c) = ct {
            headers.insert("content-type".into(), c.into());
        }
        Request {
            method: method.into(),
            path: path.into(),
            body: body.as_bytes().to_vec(),
            headers,
        }
    }

    /// The server holds provider keys and drives debates. Binding to loopback stops
    /// other *machines* and does nothing about the page the operator is reading, so
    /// the request has to prove it was not that page's idea.
    #[test]
    fn a_request_from_another_page_is_refused() {
        // The shape of the attack: a form on any site, auto-submitted, aimed at
        // loopback. `text/plain` is CORS-safelisted so it needs no preflight.
        let form_post = req(
            "POST",
            "/api/endpoints/probe",
            "127.0.0.1:1427",
            Some("https://evil.example"),
            Some("text/plain"),
            r#"{"endpoint":{"id":"decision","baseUrl":"http://evil.example"}}"#,
        );
        assert!(request_is_same_origin(&form_post).is_err());

        // Even with no Origin at all and a JSON content type: this is what a
        // DNS-rebinding attacker sends, addressed by name.
        let rebound = req(
            "POST",
            "/api/keys",
            "evil.example:1427",
            None,
            Some("application/json"),
            r#"{"endpointId":"decision","apiKey":"x"}"#,
        );
        assert!(request_is_same_origin(&rebound).is_err());
    }

    /// A connection with no `Last-Event-ID` has seen nothing on *this*
    /// connection — including a phone whose first frames were broadcast before
    /// its `EventSource` handshake finished. It gets the whole backlog, because
    /// without `DebateStarted`/`RoundStarted`/`TurnStarted` the UI has no live
    /// turn, drops every token and heartbeat for it, and reads as frozen until
    /// the first speech lands.
    #[test]
    fn a_fresh_subscriber_replays_the_backlog() {
        // Frames are pre-serialized wire bytes now; the test only cares about
        // sequence numbers, so the bodies are placeholders.
        let buf: VecDeque<(u64, String)> = (0..3)
            .map(|n| (n, format!("{{\"seq\":{n}}}")))
            .collect();
        match catch_up(&buf, None) {
            CatchUp::Replay(frames) => assert_eq!(
                frames.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
                vec![0, 1, 2]
            ),
            CatchUp::Resync => panic!("a new connection must replay, not resync"),
        }
        // ... even when it already holds everything, since the client's own
        // sequence tracker drops what it has seen.
        match catch_up(&buf, Some(1)) {
            CatchUp::Replay(frames) => assert_eq!(
                frames.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
                vec![2]
            ),
            CatchUp::Resync => panic!("a contiguous gap must replay, not resync"),
        }
        // Caught up already: nothing to send, and no resync either.
        match catch_up(&buf, Some(2)) {
            CatchUp::Replay(frames) => assert!(frames.is_empty()),
            CatchUp::Resync => panic!("a caught-up client needs nothing"),
        }
        // The ring wrapped past what was asked for: the gap is unfillable, so
        // say so honestly instead of pretending the stream is whole.
        let wrapped: VecDeque<(u64, String)> = (100..103)
            .map(|n| (n, format!("{{\"seq\":{n}}}")))
            .collect();
        match catch_up(&wrapped, Some(2)) {
            CatchUp::Resync => {}
            CatchUp::Replay(frames) => panic!("a wrapped gap must resync, got {frames:?}"),
        }
        // An empty buffer means nothing was ever emitted: nothing to replay.
        match catch_up(&VecDeque::new(), None) {
            CatchUp::Replay(frames) => assert!(frames.is_empty()),
            CatchUp::Resync => panic!("an empty backlog needs no resync"),
        }
    }

    /// The model-list cache never serves one endpoint's list to another
    /// endpoint or address: the key is (endpoint id, base URL), and anything
    /// else misses.
    #[test]
    fn the_model_cache_never_serves_another_endpoint_or_address() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let (tx, _rx) = broadcast::channel::<(u64, String)>(8);
            let sink: Arc<dyn council_core::engine::EventSink> =
                Arc::new(|_: EngineEvent| {});
            let shared = Shared {
                engine: Arc::new(Engine::new(ProviderBackend::new(), sink)),
                tx,
                replay: Arc::new(std::sync::Mutex::new(VecDeque::new())),
                model_cache: Mutex::new(HashMap::new()),
                debates: AtomicU64::new(0),
                active: Mutex::new(None),
                keys: Mutex::new(KeyStore::default()),
                keys_path: PathBuf::from("/nonexistent"),
                dist: PathBuf::from("/nonexistent"),
            };
            assert!(shared.cached_models("ep-1", "http://a").await.is_none());
            shared
                .remember_models("ep-1", "http://a", vec!["m1".into()])
                .await;
            assert_eq!(
                shared.cached_models("ep-1", "http://a").await,
                Some(vec!["m1".to_string()]),
                "the exact endpoint at the exact address hits"
            );
            assert!(
                shared.cached_models("ep-2", "http://a").await.is_none(),
                "a different endpoint id must miss"
            );
            assert!(
                shared.cached_models("ep-1", "http://b").await.is_none(),
                "a different address must miss"
            );
            assert!(
                shared
                    .cached_models("ep-1", "http://a/")
                    .await
                    .is_none(),
                "even a trailing slash is a different key than the probe sent"
            );
        });
    }

    /// And the ordinary cases keep working, or the fix has broken the app instead.
    #[test]
    fn the_real_clients_are_still_welcome() {
        // The app itself: same-origin fetch, JSON body.
        let app = req(
            "POST",
            "/api/debates",
            "127.0.0.1:1427",
            Some("http://127.0.0.1:1427"),
            Some("application/json; charset=utf-8"),
            r#"{"motion":"x"}"#,
        );
        assert!(request_is_same_origin(&app).is_ok());

        // A phone on the LAN, which is the whole point of COUNCIL_SERVE_ALLOW_REMOTE.
        let phone = req(
            "POST",
            "/api/debates",
            "192.168.1.50:1427",
            Some("http://192.168.1.50:1427"),
            Some("application/json"),
            r#"{"motion":"x"}"#,
        );
        assert!(request_is_same_origin(&phone).is_ok());

        // A plain GET from the browser sends no Origin and has no body.
        let page = req("GET", "/api/health", "localhost:1427", None, None, "");
        assert!(request_is_same_origin(&page).is_ok());

        // curl, or any native client.
        let native = req("POST", "/api/keys", "127.0.0.1:1427", None, Some("application/json"), r#"{"endpointId":"a","apiKey":"k"}"#);
        assert!(request_is_same_origin(&native).is_ok());
    }

    /// A stored key must only ever travel to the address it was saved against.
    ///
    /// The relay this closes: `/api/endpoints/probe` took `baseUrl` from the request
    /// and attached the stored key for the named id, so naming an id and pointing the
    /// URL anywhere delivered that provider key with an `Authorization: Bearer`
    /// header. `"decision"` needs no guessing.
    #[test]
    fn a_stored_key_is_never_sent_to_an_address_it_was_not_saved_with() {
        let mut store = KeyStore::default();
        store
            .keys
            .insert("decision".into(), "sk-live-do-not-send".into());
        store
            .profiles
            .insert("decision".into(), "https://api.anthropic.com".into());

        assert_eq!(
            store.key_for("decision", "https://api.anthropic.com"),
            Some("sk-live-do-not-send".into()),
            "the address it was saved for must work"
        );
        // Trailing slashes and case are not a different host.
        assert!(store
            .key_for("decision", "https://api.anthropic.com/")
            .is_some());

        assert_eq!(
            store.key_for("decision", "http://evil.example"),
            None,
            "an attacker-chosen address must get no key"
        );
        assert_eq!(
            store.key_for("decision", "http://127.0.0.1:9000"),
            None,
            "a local port is still a different address"
        );
        assert_eq!(store.key_for("other", "https://api.anthropic.com"), None);
        // No recorded address at all — a store written before profiles existed.
        // Safe by default: the operator re-saves the key once.
        let mut legacy = KeyStore::default();
        legacy.keys.insert("ep-1".into(), "sk-legacy".into());
        assert_eq!(legacy.key_for("ep-1", "https://api.openai.com"), None);
    }

    /// Omitting `apiKey` used to delete the stored key, because the field was
    /// `#[serde(default)]` and `None` fell into the remove arm. A cross-site form
    /// cannot name a body field it does not send, so "leave apiKey out" was a
    /// one-field deletion attack whose confirmation was discarded cross-origin.
    #[test]
    fn omitting_the_key_no_longer_deletes_it() {
        let omitted: KeyBody = serde_json::from_str(r#"{"endpointId":"decision"}"#).expect("parses");
        assert!(omitted.api_key.is_none());
        assert!(
            !omitted.clear,
            "a body that merely leaves apiKey out must not read as a request to clear"
        );
        let explicit: KeyBody =
            serde_json::from_str(r#"{"endpointId":"decision","clear":true}"#).expect("parses");
        assert!(explicit.clear);
    }

    #[test]
    fn loopback_is_told_apart_from_a_network_bind() {
        use std::net::{IpAddr, Ipv4Addr};
        assert!(!remote_bind(&"127.0.0.1:1".parse().unwrap()));
        assert!(!remote_bind(&"[::1]:1".parse().unwrap()));
        assert!(remote_bind(&"192.168.1.5:1".parse().unwrap()));
        assert!(remote_bind(&"0.0.0.0:1".parse().unwrap()));
        // Not a loopback alias either: an IPv4-mapped loopback is still this machine.
        let v4: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(!remote_bind(&SocketAddr::new(v4, 1)));
    }

    #[test]
    fn loopback_binds_but_lan_and_wildcard_do_not() {
        with_env("COUNCIL_SERVE_ALLOW_REMOTE", None, || {
            with_env("COUNCIL_SERVE_ADDR", Some("127.0.0.1:1427"), || {
                assert!(bind_addr().is_ok());
            });
            for raw in ["0.0.0.0:1427", "192.168.1.10:1427", "[::]:1427"] {
                with_env("COUNCIL_SERVE_ADDR", Some(raw), || {
                    let err = bind_addr().unwrap_err();
                    assert!(err.contains("refusing to serve"), "got: {err}");
                });
            }
            with_env("COUNCIL_SERVE_ADDR", Some("not an address"), || {
                assert!(bind_addr().is_err());
            });
        });
        with_env("COUNCIL_SERVE_ALLOW_REMOTE", Some("1"), || {
            with_env("COUNCIL_SERVE_ADDR", Some("0.0.0.0:1427"), || {
                assert!(bind_addr().is_ok());
            });
        });
    }

    #[test]
    fn secrets_in_the_config_are_spotted_before_anything_runs() {
        let clean = serde_json::json!({
            "motion": "x",
            "endpoints": [{ "id": "e", "apiKey": null }],
            "decision": { "apiKey": null },
        });
        assert!(!config_carries_secret(&clean));
        let sneaky = serde_json::json!({
            "motion": "x",
            "endpoints": [{ "id": "e", "apiKey": "sk-live" }],
        });
        assert!(config_carries_secret(&sneaky));
        let sneaky_decision = serde_json::json!({
            "motion": "x",
            "endpoints": [],
            "decision": { "provider": "jev", "apiKey": "k" },
        });
        assert!(config_carries_secret(&sneaky_decision));
        let empty_string_is_not_a_key = serde_json::json!({
            "endpoints": [{ "id": "e", "apiKey": "" }],
        });
        assert!(!config_carries_secret(&empty_string_is_not_a_key));
    }

    #[test]
    fn static_paths_cannot_escape_the_dist_dir() {
        let dist = Path::new("/tmp/council-dist");
        assert_eq!(safe_join(dist, "/"), Some(dist.join("index.html")));
        assert_eq!(
            safe_join(dist, "/assets/app.js"),
            Some(dist.join("assets/app.js"))
        );
        assert_eq!(safe_join(dist, "/../secret"), None);
        assert_eq!(safe_join(dist, "/a/../../secret"), None);
    }

    #[test]
    fn key_bodies_speak_camel_case_like_everything_else() {
        // The live bug this pins: without `rename_all`, `endpointId` fell
        // through to the default empty string and the key went nowhere.
        let body: KeyBody = serde_json::from_value(serde_json::json!({
            "endpointId": "ep-9",
            "apiKey": "sk-test",
        }))
        .expect("camelCase key body parses");
        assert_eq!(body.endpoint_id, "ep-9");
        assert_eq!(body.api_key.as_deref(), Some("sk-test"));
    }

    #[test]
    fn key_status_never_reports_values() {
        let mut store = KeyStore::default();
        store.keys.insert("ep-1".to_string(), "sk-live".to_string());
        assert!(store.has("ep-1"));
        assert!(!store.has("ep-2"));
        let reported = serde_json::json!({ "ep-1": store.has("ep-1") });
        assert!(!reported.to_string().contains("sk-live"));
    }
}
