import { memo, useEffect, useRef, useState } from "react";
import { ArrowsClockwise, Plus, X } from "@phosphor-icons/react";
import { inTauri, listModels } from "../bridge";
import {
  REASONING_EFFORT_HINT,
  ROLE_LABEL,
  type DebateConfig,
  type DelegateConfig,
  type EndpointConfig,
  type ImageAttachment,
} from "../types";

/** Extra seats a council may have. Must match the engine's own limit. */
const MAX_CUSTOM_SEATS = 6;

/** At most this many images ride on the motion. Must match App's sanitize. */
export const MAX_IMAGES = 6;

const PALETTE = [
  "#65ccc6",
  "#2e8b83",
  "#bffffa",
  "#4aa8a1",
  "#8ad9d3",
  "#1f6b64",
  "#d9fffc",
  "#0f4a45",
];

export function freshEndpoint(index: number): EndpointConfig {
  return {
    id: `ep-${Date.now().toString(36)}-${index}`,
    name: `Endpoint ${index + 1}`,
    baseUrl: "http://127.0.0.1:8888",
    apiKey: null,
    headers: {},
    timeoutSecs: 300,
    nativeTools: true,
    stream: true,
    allowInvalidCerts: false,
    includeUsage: true,
    // Nothing is sent until asked for: the shapes are not interchangeable and a
    // model left to its own default may pick a level that fails.
    reasoningWire: "serverDefault",
    reasoningEffort: null,
    thinking: null,
  };
}

interface Props {
  config: DebateConfig;
  onChange: (next: DebateConfig) => void;
  disabled: boolean;
  /** Discard the stored setup and reload from defaults. */
  onReset: () => void;
}

function Section({
  title,
  action,
  children,
}: {
  title: string;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <section className="border-b border-line px-3 py-4">
      <div className="mb-2.5 flex items-center justify-between">
        <h2 className="section-title">{title}</h2>
        {action}
      </div>
      {children}
    </section>
  );
}

function Check({
  checked,
  disabled,
  onChange,
  label,
  title,
}: {
  checked: boolean;
  disabled: boolean;
  onChange: (v: boolean) => void;
  label: string;
  title?: string;
}) {
  return (
    <label className="flex cursor-pointer items-center gap-1.5 text-[11.5px] whitespace-nowrap text-ink-2">
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        className="size-3.5 accent-[var(--primary)]"
      />
      <span title={title}>{label}</span>
    </label>
  );
}

function Slider({
  label,
  value,
  min,
  max,
  step,
  disabled,
  onChange,
  format,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  disabled: boolean;
  onChange: (v: number) => void;
  format?: (v: number) => string;
}) {
  return (
    <label className="min-w-0 flex-1">
      <span className="block text-[10.5px] text-ink-3">
        {label} {format ? format(value) : value}
      </span>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(Number(e.target.value))}
        className="w-full accent-[var(--primary)]"
      />
    </label>
  );
}

function ago(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  return `${Math.floor(s / 60)}m ago`;
}

/** A model picker backed by the endpoint's own list. Falls back to free text
 *  when the list was never fetched, and always allows a custom name. */
function ModelPicker({
  value,
  models,
  disabled,
  onChange,
}: {
  value: string;
  models: string[] | null;
  disabled: boolean;
  onChange: (v: string) => void;
}) {
  const [custom, setCustom] = useState(false);
  const known = models ?? [];
  const showSelect = !custom && known.length > 0;
  // A stored name the server no longer lists is still offered, so a saved
  // config never silently loses its selection.
  const options = value && !known.includes(value) ? [value, ...known] : known;

  if (!showSelect) {
    return (
      <div>
        <input
          className="field font-mono !text-[12px]"
          value={value}
          placeholder={
            models === null
              ? "model name — checking the endpoint…"
              : "model name, e.g. unsloth/Qwen3-27B"
          }
          disabled={disabled}
          onChange={(e) => onChange(e.target.value)}
          aria-label="model name"
        />
        {known.length > 0 && (
          <button
            type="button"
            className="mt-1 text-[11px] text-primary hover:underline"
            disabled={disabled}
            onClick={() => setCustom(false)}
          >
            pick from the list instead
          </button>
        )}
      </div>
    );
  }
  return (
    <div>
      <select
        className="field font-mono !text-[12px]"
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
      >
        <option value="">choose a model…</option>
        {options.map((m) => (
          <option value={m} key={m}>
            {m}
          </option>
        ))}
      </select>
      <button
        type="button"
        className="mt-1 text-[11px] text-ink-3 hover:text-ink-2 hover:underline"
        disabled={disabled}
        onClick={() => setCustom(true)}
      >
        or type a custom name
      </button>
    </div>
  );
}

/**
 * Shrink a picked image to something a context window can actually carry:
 * max 1568px on a side, JPEG. Full-resolution photos bill thousands of image
 * tokens a turn and blow past localStorage on save; a downscaled one argues
 * just as well about charts, screenshots and diagrams.
 */
function downscaleImage(file: File): Promise<ImageAttachment> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      URL.revokeObjectURL(url);
      const MAX_DIM = 1568;
      const scale = Math.min(1, MAX_DIM / Math.max(img.width, img.height));
      const w = Math.max(1, Math.round(img.width * scale));
      const h = Math.max(1, Math.round(img.height * scale));
      const canvas = document.createElement("canvas");
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext("2d");
      if (!ctx) {
        reject(new Error("this browser cannot process images"));
        return;
      }
      ctx.drawImage(img, 0, 0, w, h);
      canvas.toBlob(
        (blob) => {
          if (!blob) {
            reject(new Error(`"${file.name}" could not be read`));
            return;
          }
          const reader = new FileReader();
          reader.onload = () =>
            resolve({
              id: `img-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`,
              name: file.name.slice(0, 80) || "image",
              mediaType: "image/jpeg",
              dataUrl: String(reader.result),
            });
          reader.onerror = () => reject(new Error(`"${file.name}" could not be read`));
          reader.readAsDataURL(blob);
        },
        "image/jpeg",
        0.85,
      );
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error(`"${file.name}" is not a readable image`));
    };
    img.src = url;
  });
}

/** Images on the motion, for the council to argue over. */
function ImagePicker({
  images,
  disabled,
  onChange,
}: {
  images: ImageAttachment[];
  disabled: boolean;
  onChange: (next: ImageAttachment[]) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const slotsLeft = MAX_IMAGES - images.length;

  const add = async (files: FileList | File[] | null) => {
    if (!files || disabled || busy) return;
    const pending: File[] = Array.isArray(files) ? files : Array.from(files);
    const picked = pending
      .filter((f) => f.type.startsWith("image/"))
      .slice(0, Math.max(0, slotsLeft));
    if (!picked.length) return;
    setBusy(true);
    setError("");
    try {
      const done: ImageAttachment[] = [];
      const failed: string[] = [];
      for (const f of picked) {
        try {
          if (f.size > 20 * 1024 * 1024) throw new Error(`"${f.name}" is over 20MB`);
          done.push(await downscaleImage(f));
        } catch (e) {
          failed.push(e instanceof Error ? e.message : String(e));
        }
      }
      if (done.length) onChange([...images, ...done].slice(0, MAX_IMAGES));
      if (failed.length) setError(failed.join(" "));
    } finally {
      setBusy(false);
    }
  };

  // Clipboard screenshots land here: a document-level paste that stands down
  // inside text fields (so pasting the motion still pastes text) and ignores
  // text-only clipboard contents. The ref carries the latest `add` so a paste
  // never closes over a stale images list and drops picks.
  const addRef = useRef(add);
  addRef.current = add;
  useEffect(() => {
    if (disabled) return;
    const onPaste = (e: ClipboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) {
        return;
      }
      const pasted: File[] = [];
      const items = e.clipboardData?.items;
      if (items) {
        for (const it of items) {
          if (it.type.startsWith("image/")) {
            const f = it.getAsFile();
            if (f) pasted.push(f);
          }
        }
      }
      if (!pasted.length && e.clipboardData?.files?.length) {
        pasted.push(
          ...Array.from(e.clipboardData.files).filter((f) => f.type.startsWith("image/")),
        );
      }
      if (!pasted.length) return;
      e.preventDefault();
      void addRef.current(pasted);
    };
    document.addEventListener("paste", onPaste);
    return () => document.removeEventListener("paste", onPaste);
  }, [disabled]);

  return (
    <div className="mt-2">
      <div className="mb-1.5 text-[11px] font-medium text-ink-2">
        Images for the council — argue over what they show
      </div>
      {images.length > 0 && (
        <ul className="mb-1.5 flex flex-wrap gap-1.5">
          {images.map((im) => (
            <li key={im.id} className="card relative overflow-hidden !rounded-lg">
              <img
                src={im.dataUrl}
                alt={im.name}
                className="h-16 w-16 object-cover"
                title={`${im.name} · ${Math.round(im.dataUrl.length / 1024)}KB`}
              />
              <button
                type="button"
                className="absolute top-0.5 right-0.5 rounded-full bg-surface p-0.5 text-ink-2 hover:text-bad"
                disabled={disabled}
                onClick={() => onChange(images.filter((x) => x.id !== im.id))}
                title={`remove ${im.name}`}
                aria-label={`remove ${im.name}`}
              >
                <X size={12} />
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="flex flex-wrap items-center gap-2">
        <label className={`btn btn-ghost ${disabled || slotsLeft <= 0 || busy ? "opacity-40" : ""}`}>
          {busy ? "reading…" : slotsLeft <= 0 ? "full" : "add images"}
          <input
            type="file"
            accept="image/*"
            multiple
            className="hidden"
            disabled={disabled || slotsLeft <= 0 || busy}
            onChange={(e) => {
              void add(e.target.files);
              e.target.value = "";
            }}
            aria-label="attach images to the motion"
          />
        </label>
        <span className="text-[11px] text-ink-3 tabular-nums">
          {images.length}/{MAX_IMAGES}
        </span>
      </div>
      {error && (
        <p className="mt-1 text-[11px] text-bad" role="alert">
          {error}
        </p>
      )}
      <p className="mt-1 text-[11px] leading-relaxed text-ink-3">
        Add files above, or copy a screenshot and press Ctrl+V anywhere outside
        a text field. Every seat sees these with its opening message. Needs a
        vision-capable model — a text-only model will fail the turn instead.
      </p>
    </div>
  );
}

/**
 * Memoized: it holds ~60 inputs and its `config` identity is stable between
 * tokens, so re-rendering it on every streamed word was pure waste.
 */
export const SetupPanel = memo(function SetupPanel({
  config,
  onChange,
  disabled,
  onReset,
}: Props) {
  const [models, setModels] = useState<Record<string, string[] | null>>({});
  const [probeError, setProbeError] = useState<Record<string, string>>({});
  const [checking, setChecking] = useState<Record<string, boolean>>({});
  const [lastChecked, setLastChecked] = useState<Record<string, number>>({});
  const inFlight = useRef(new Set<string>());

  const patch = (p: Partial<DebateConfig>) => onChange({ ...config, ...p });

  const setEndpoint = (id: string, p: Partial<EndpointConfig>) =>
    patch({ endpoints: config.endpoints.map((e) => (e.id === id ? { ...e, ...p } : e)) });

  const setDelegate = (id: string, p: Partial<DelegateConfig>) =>
    patch({ delegates: config.delegates.map((d) => (d.id === id ? { ...d, ...p } : d)) });

  const probe = async (ep: EndpointConfig, silent: boolean) => {
    if (!inTauri()) {
      if (!silent) {
        setProbeError((e) => ({
          ...e,
          [ep.id]: "Browser preview — models list only inside the desktop app. Type the name instead.",
        }));
      }
      return;
    }
    if (inFlight.current.has(ep.id)) return;
    inFlight.current.add(ep.id);
    setChecking((c) => ({ ...c, [ep.id]: true }));
    if (!silent) setProbeError((e) => ({ ...e, [ep.id]: "" }));
    try {
      const found = await listModels(ep);
      setModels((m) => ({ ...m, [ep.id]: found }));
      setLastChecked((l) => ({ ...l, [ep.id]: Date.now() }));
      // A stale error must not survive a successful check, and an empty list
      // is itself the error worth showing.
      setProbeError((e) => ({
        ...e,
        [ep.id]:
          found.length === 0
            ? "Reached the server, but it listed no models. Is one loaded?"
            : "",
      }));
    } catch (err) {
      // A background refresh must never wipe a list the user is working from.
      setModels((m) => (silent && m[ep.id] ? m : { ...m, [ep.id]: null }));
      setProbeError((e) => ({ ...e, [ep.id]: String(err) }));
    } finally {
      inFlight.current.delete(ep.id);
      setChecking((c) => ({ ...c, [ep.id]: false }));
    }
  };
  const probeRef = useRef(probe);
  probeRef.current = probe;
  const cfgRef = useRef(config);
  cfgRef.current = config;

  // The list used to sit empty until "test connection" was pressed, so every
  // dropdown read stale. Refresh shortly after the address or key changes,
  // and once on mount.
  const autoKey = config.endpoints
    .map((e) => `${e.id}|${e.baseUrl}|${e.apiKey ?? ""}`)
    .join("~");
  useEffect(() => {
    if (disabled || !inTauri()) return;
    const t = setTimeout(() => {
      for (const ep of cfgRef.current.endpoints) {
        if (ep.baseUrl.trim()) void probeRef.current(ep, true);
      }
    }, 800);
    return () => clearTimeout(t);
  }, [autoKey, disabled]);

  const addEndpoint = () =>
    patch({ endpoints: [...config.endpoints, freshEndpoint(config.endpoints.length)] });

  const removeEndpoint = (id: string) => {
    // The fallback must come from the list that survives the deletion. Using
    // the old first element re-points delegates at the endpoint being removed,
    // which then fails at turn time with "no endpoint configured".
    const endpoints = config.endpoints.filter((e) => e.id !== id);
    const fallback = endpoints[0]?.id ?? "";
    patch({
      endpoints,
      delegates: config.delegates.map((d) =>
        d.endpointId === id ? { ...d, endpointId: fallback } : d,
      ),
    });
  };

  // The engine refuses a council with more than this many extra seats. Keeping
  // the number here as well means the button goes quiet instead of letting you
  // build a setup that will not convene.
  const customSeats = config.delegates.filter((d) => d.seatRole === "custom").length;
  const atSeatLimit = customSeats >= MAX_CUSTOM_SEATS;

  const addDelegate = () => {
    if (atSeatLimit) return;
    const i = config.delegates.length;
    patch({
      delegates: [
        ...config.delegates,
        {
          id: `seat-${Date.now().toString(36)}-${i}`,
          label: `Delegate ${i + 1}`,
          model: "",
          endpointId: config.endpoints[0]?.id ?? "",
          color: PALETTE[i % PALETTE.length],
          seatRole: "custom",
          role: null,
          persona: null,
          temperature: 0.7,
        },
      ],
    });
  };

  const removeDelegate = (id: string) =>
    patch({ delegates: config.delegates.filter((d) => d.id !== id) });

  const applyModelToAll = (model: string) => {
    if (!model) return;
    patch({ delegates: config.delegates.map((d) => ({ ...d, model })) });
  };

  // Every model any endpoint currently lists, for the one-model-everywhere row.
  const allModels = [...new Set(Object.values(models).flatMap((m) => m ?? []))].sort();

  return (
    <div>
      <Section title="The motion">
        <textarea
          className="field mb-2 px-3 py-2 text-[13.5px] leading-relaxed"
          rows={4}
          placeholder="What should the council decide? e.g. Which of these two prompts produces better structured output?"
          aria-label="motion"
          value={config.motion}
          disabled={disabled}
          onChange={(e) => patch({ motion: e.target.value })}
        />
        <textarea
          className="field"
          rows={2}
          placeholder="Standing instructions for the whole council (optional)"
          aria-label="standing instructions for the whole council"
          value={config.extraInstructions ?? ""}
          disabled={disabled}
          onChange={(e) => patch({ extraInstructions: e.target.value || null })}
        />
        <ImagePicker
          images={config.images}
          disabled={disabled}
          onChange={(images) => patch({ images })}
        />
      </Section>

      <Section
        title="Endpoints"
        action={
          <button type="button" className="btn btn-ghost !px-2.5" onClick={addEndpoint} disabled={disabled} title="add endpoint">
            <Plus size={13} /> add
          </button>
        }
      >
        {config.endpoints.map((ep) => {
          const list = models[ep.id] ?? null;
          const err = probeError[ep.id] ?? "";
          const busy = !!checking[ep.id];
          const checked = lastChecked[ep.id];
          return (
            <div key={ep.id} className="card mb-1.5 p-2">
              <div className="mb-1.5 flex items-center gap-1.5">
                <span
                  className={`size-2 shrink-0 rounded-full ${
                    err ? "bg-bad" : list ? "bg-ok" : "bg-line-2"
                  } ${busy ? "live-dot" : ""}`}
                  title={
                    busy
                      ? "checking…"
                      : err
                        ? "unreachable"
                        : list
                          ? `${list.length} models`
                          : "not checked yet"
                  }
                />
                <input
                  className="field"
                  value={ep.name}
                  placeholder="name"
                  disabled={disabled}
                  onChange={(e) => setEndpoint(ep.id, { name: e.target.value })}
                />
                <button
                  type="button"
                  className="btn btn-ghost !px-2"
                  disabled={disabled}
                  onClick={() => removeEndpoint(ep.id)}
                  title="remove endpoint"
                aria-label="remove endpoint"
                >
                  <X size={13} />
                </button>
              </div>
              <input
                className="field font-mono !text-[12px]"
                value={ep.baseUrl}
                placeholder="http://localhost:8888"
                disabled={disabled}
                onChange={(e) => setEndpoint(ep.id, { baseUrl: e.target.value })}
              />
              <input
                className="field font-mono !text-[12px]"
                type="password"
                value={ep.apiKey ?? ""}
                placeholder="api key (optional)"
                disabled={disabled}
                onChange={(e) => setEndpoint(ep.id, { apiKey: e.target.value || null })}
              />

              <div className="mb-2 flex flex-wrap items-center gap-x-3 gap-y-1.5">
                <Check
                  checked={ep.nativeTools}
                  disabled={disabled}
                  onChange={(v) => setEndpoint(ep.id, { nativeTools: v })}
                  label="tool calls"
                />
                <Check
                  checked={ep.stream}
                  disabled={disabled}
                  onChange={(v) => setEndpoint(ep.id, { stream: v })}
                  label="stream"
                  title="off for a Cloudflare tunnel, which drops server-sent events"
                />
                <Check
                  checked={ep.allowInvalidCerts}
                  disabled={disabled}
                  onChange={(v) => setEndpoint(ep.id, { allowInvalidCerts: v })}
                  label="skip cert"
                  title="only for a local https server with a self-signed certificate"
                />
                <Check
                  checked={ep.includeUsage}
                  disabled={disabled}
                  onChange={(v) => setEndpoint(ep.id, { includeUsage: v })}
                  label="count tokens"
                  title="ask the server to report what each turn cost. Needs a server that supports stream_options; if it rejects the field the engine asks again without it, once, and remembers."
                />
                <Check
                  checked={ep.thinking === false}
                  disabled={disabled}
                  onChange={(v) => setEndpoint(ep.id, { thinking: v ? false : null })}
                  label="no thinking"
                  title="turn the thinking block off, for templates that gate it behind enable_thinking. Left alone, the model's own default stands — on, for the Qwen line."
                />
              </div>

              {/* Reasoning effort. There is no standard for where it goes, and
                  the wrong shape is not ignored: a template that does not know
                  the value raises on it and the request comes back 400. Hence a
                  shape selector rather than a guess. */}
              <div className="mt-2 flex flex-wrap items-center gap-2">
                <select
                  className="field !w-auto !py-1 text-[11.5px]"
                  value={ep.reasoningWire}
                  disabled={disabled}
                  onChange={(ev) =>
                    setEndpoint(ep.id, {
                      reasoningWire: ev.target.value as EndpointConfig["reasoningWire"],
                    })
                  }
                  title="where this server expects reasoning effort"
                >
                  <option value="serverDefault">thinking: server default</option>
                  <option value="chatTemplate">thinking: chat template kwargs</option>
                  <option value="flat">thinking: top-level field</option>
                </select>
                <input
                  className="field !w-28 !py-1 font-mono text-[11.5px]"
                  list={`reasoning-effort-levels-${ep.id}`}
                  placeholder={ep.reasoningWire === "serverDefault" ? "unset" : "medium"}
                  value={ep.reasoningEffort ?? ""}
                  disabled={disabled || ep.reasoningWire === "serverDefault"}
                  onChange={(e) =>
                    setEndpoint(ep.id, { reasoningEffort: e.target.value.trim() || null })
                  }
                  title="spelled exactly as this model spells it — see below"
                  aria-label="reasoning effort"
                />
                <datalist id={`reasoning-effort-levels-${ep.id}`}>
                  <option value="low" />
                  <option value="medium" />
                  <option value="high" />
                  <option value="xhigh" />
                </datalist>
                <span
                  className="text-[11px] text-ink-3"
                  title={`${REASONING_EFFORT_HINT}\n\nQwen3.8 defaults to xhigh, which returns an empty answer on roughly one call in five on llama.cpp (QwenLM/Qwen3.8#216). low and medium were clean.\n\nllama-server has no top-level reasoning_effort at all — llama.cpp#20408 — so use chat template kwargs there.`}
                >
                  {REASONING_EFFORT_HINT}
                </span>
              </div>

              <div className="flex items-center gap-2">
                <button
                  type="button"
                  className="btn btn-ghost"
                  disabled={disabled || busy}
                  onClick={() => probe(ep, false)}
                  title="ask the server what models it has loaded right now"
                >
                  {busy ? <span className="spinner" /> : <ArrowsClockwise size={13} />}
                  {busy ? "checking…" : list ? "refresh" : "check"}
                </button>
                <Slider
                  label="timeout"
                  value={ep.timeoutSecs}
                  min={10}
                  max={1800}
                  step={10}
                  disabled={disabled}
                  onChange={(v) => setEndpoint(ep.id, { timeoutSecs: v })}
                  format={(v) => `${v}s`}
                />
              </div>

              <div className="mt-1.5 text-[10.5px] text-ink-3">
                {list ? (
                  <span>
                    {list.length} model{list.length === 1 ? "" : "s"}
                    {checked ? ` · checked ${ago(checked)}` : ""}
                  </span>
                ) : busy ? (
                  <span>asking the server…</span>
                ) : (
                  <span>edits to the address re-check automatically</span>
                )}
              </div>

              {err && (
                <div
                  className="mt-1.5 rounded-xl border border-bad/70 bg-bad-soft px-2.5 py-1.5 text-[11px] leading-relaxed break-words text-bad"
                  title={err}
                >
                  {err}
                </div>
              )}
            </div>
          );
        })}
      </Section>

      <Section
        title="Delegates"
        action={
          <button
            type="button"
            className="btn btn-ghost !px-2.5"
            onClick={addDelegate}
            disabled={disabled || atSeatLimit}
            title={
              atSeatLimit
                ? `a council can have at most ${MAX_CUSTOM_SEATS} extra seats`
                : "add delegate"
            }
          >
            <Plus size={13} /> add
          </button>
        }
      >
        {config.delegates.length >= 2 && allModels.length > 0 && (
          <div className="card mb-1.5 p-2">
            <OneModelRow models={allModels} disabled={disabled} onApply={applyModelToAll} />
          </div>
        )}
        {config.delegates.length < 2 && (
          <p className="mb-2 text-[11.5px] text-ink-3">A council needs at least two.</p>
        )}
        {config.delegates.map((d) => {
          // The fixed seats are fixed furniture: their labels never edit
          // and they cannot be removed. Everything else about the seat —
          // model, endpoint, voice — configures like any other.
          const essential = d.seatRole !== "custom";
          return (
          <div key={d.id} className="card mb-1.5 p-2">
            <div className="mb-1.5 flex items-center gap-1.5">
              <span
                className="size-2.5 shrink-0 rounded-full"
                style={{ background: d.color }}
              />
              {essential ? (
                <span className="field flex items-center justify-between !bg-surface-2" title="essential seat — fixed">
                  <span className="font-medium text-ink">{d.label}</span>
                  <span className="rounded-pill bg-surface-3 px-1.5 py-px text-[10px] tracking-wide text-ink-2 uppercase">
                    {ROLE_LABEL[d.seatRole]}
                  </span>
                </span>
              ) : (
                <input
                  className="field"
                  value={d.label}
                  placeholder="name"
                  disabled={disabled}
                  onChange={(e) => setDelegate(d.id, { label: e.target.value })}
                />
              )}
              {!essential && (
                <button
                  type="button"
                  className="btn btn-ghost !px-2"
                  disabled={disabled}
                  onClick={() => removeDelegate(d.id)}
                  title="remove delegate"
                aria-label="remove delegate"
                >
                  <X size={13} />
                </button>
              )}
            </div>
            <ModelPicker
              value={d.model}
              models={models[d.endpointId] ?? null}
              disabled={disabled}
              onChange={(v) => setDelegate(d.id, { model: v })}
            />
            <select
              className="field"
              value={d.endpointId}
              disabled={disabled}
              onChange={(e) => setDelegate(d.id, { endpointId: e.target.value })}
              title="which server this seat talks to"
            >
              {config.endpoints.length === 0 && <option value="">no endpoints</option>}
              {config.endpoints.map((ep) => (
                <option value={ep.id} key={ep.id}>
                  {ep.name || ep.baseUrl}
                </option>
              ))}
            </select>
            <input
              className="field"
              value={d.role ?? ""}
              placeholder="role in the council"
              disabled={disabled}
              onChange={(e) => setDelegate(d.id, { role: e.target.value || null })}
            />
            <input
              className="field"
              value={d.persona ?? ""}
              placeholder="voice / persona (optional)"
              disabled={disabled}
              onChange={(e) => setDelegate(d.id, { persona: e.target.value || null })}
            />
            <Slider
              label="temperature"
              value={d.temperature ?? 0.7}
              min={0}
              max={2}
              step={0.05}
              disabled={disabled}
              onChange={(v) => setDelegate(d.id, { temperature: v })}
              format={(v) => v.toFixed(2)}
            />
          </div>
          );
        })}
      </Section>

      <Section title="Decisions">
        <p className="mb-2 text-[11px] leading-relaxed text-ink-3">
          Poll ballots are classification, not deliberation. A decision model
          answers them without burning a turn per voter; votes stay with the
          seats, since they need reasons.
        </p>
        <select
          className="field"
          value={config.decision.provider}
          disabled={disabled}
          onChange={(e) =>
            patch({
              decision: {
                ...config.decision,
                provider: e.target.value as "seat" | "jev",
              },
            })
          }
          title="who answers poll ballots"
        >
          <option value="seat">Each seat decides (own model)</option>
          <option value="jev">Decision model (Jev-style)</option>
        </select>
        {config.decision.provider === "jev" && (
          <>
            <input
              className="field font-mono !text-[12px]"
              value={config.decision.baseUrl}
              placeholder="decision endpoint URL"
              disabled={disabled}
              onChange={(e) =>
                patch({ decision: { ...config.decision, baseUrl: e.target.value } })
              }
            />
            <input
              className="field"
              value={config.decision.model}
              placeholder="model name, e.g. jev-1"
              disabled={disabled}
              onChange={(e) =>
                patch({ decision: { ...config.decision, model: e.target.value } })
              }
            />
            <input
              className="field font-mono !text-[12px]"
              type="password"
              value={config.decision.apiKey ?? ""}
              placeholder="api key (optional)"
              disabled={disabled}
              onChange={(e) =>
                patch({
                  decision: { ...config.decision, apiKey: e.target.value || null },
                })
              }
            />
            <div className="flex items-center gap-2">
              <span className="shrink-0 text-[11px] text-ink-3">timeout</span>
              <Slider
                label=""
                value={config.decision.timeoutSecs}
                min={5}
                max={120}
                step={5}
                disabled={disabled}
                onChange={(v) =>
                  patch({ decision: { ...config.decision, timeoutSecs: v } })
                }
                format={(v) => `${v}s`}
              />
            </div>
            <p className="mt-1 text-[11px] leading-relaxed text-ink-3">
              Typed question in, choice plus confidence out. A failed call falls
              back to the seat's own model and says so.
            </p>
          </>
        )}
      </Section>

      <Section title="Procedure">
        <div className="mb-2 flex gap-3">
          <Slider
            label="rounds"
            value={config.maxRounds}
            min={1}
            max={20}
            step={1}
            disabled={disabled}
            onChange={(v) => patch({ maxRounds: v })}
          />
          <Slider
            label="extra"
            value={config.maxExtraRounds}
            min={0}
            max={20}
            step={1}
            disabled={disabled}
            onChange={(v) => patch({ maxExtraRounds: v })}
          />
        </div>
        <p className="mb-2 text-[11px] leading-relaxed text-ink-3">
          Extra rounds are the summarizer's whole budget: between rounds it may
          ask for more, and you approve the count. Tool calls per turn and
          visible record scale with the board automatically — duplicates folded,
          looping capped.
        </p>
        <p className="mb-2 text-[11px] leading-relaxed text-ink-3">
          A saved setup that no longer loads can be cleared here and re-entered.
        </p>
        <button type="button" className="btn btn-ghost" onClick={onReset} disabled={disabled}>
          reset setup
        </button>
        <div className="flex flex-col gap-1.5">
          <Check
            checked={config.autoAdjourn}
            disabled={disabled}
            onChange={(v) => patch({ autoAdjourn: v })}
            label="stop early once every point is decided"
          />
          <Check
            checked={config.closingStatements}
            disabled={disabled}
            onChange={(v) => patch({ closingStatements: v })}
            label="closing addresses before the verdict"
          />
        </div>

        {/* The reference shelf. Off by default and worded so nobody enables it
            expecting the open web: it reads Wikipedia and Wiktionary, which is
            what a term lookup actually needs, and nothing else. */}
        <div className="mt-3 border-t border-line-2 pt-3">
          <Check
            checked={config.allowWeb}
            disabled={disabled}
            onChange={(v) => patch({ allowWeb: v })}
            label="let the advocate, critic and rationale look terms up"
            title="Reads Wikipedia and Wiktionary. Not a general web search: the engines that offer one without a key now answer with a bot challenge."
          />
          {config.allowWeb && (
            <>
              <div className="mt-2 grid grid-cols-2 gap-3">
                <Slider
                  label="pages / turn"
                  value={config.maxFetchesPerTurn}
                  min={1}
                  max={8}
                  step={1}
                  disabled={disabled}
                  onChange={(v) => patch({ maxFetchesPerTurn: v })}
                />
                <Slider
                  label="chars / page"
                  value={config.maxFetchChars}
                  min={500}
                  max={40000}
                  step={500}
                  disabled={disabled}
                  onChange={(v) => patch({ maxFetchChars: v })}
                />
              </div>
              <p className="mt-1.5 text-[11px] leading-relaxed text-ink-3">
                Reading happens inside the seat&apos;s turn: it looks something up,
                gets the text back, and argues from it in the same reply. Each page
                it opens is shown in the transcript with a link, so you can check the
                source. Retrieved text is handed to the model as reference material,
                never as instructions.
              </p>
            </>
          )}
        </div>
      </Section>
    </div>
  );
});

/** One model playing every seat: pick it once, deal it to the whole table. */
function OneModelRow({
  models,
  disabled,
  onApply,
}: {
  models: string[];
  disabled: boolean;
  onApply: (model: string) => void;
}) {
  const [pick, setPick] = useState("");
  return (
    <div>
      <div className="mb-1.5 text-[11px] font-medium text-ink-2">
        One model, every seat — the same mind arguing with itself.
      </div>
      <div className="flex items-center gap-1.5">
        <select
          className="field font-mono !text-[12px]"
          value={pick}
          disabled={disabled}
          onChange={(e) => setPick(e.target.value)}
          title="seats keep their endpoints; only the model name is dealt out"
        >
          <option value="">choose a model…</option>
          {models.map((m) => (
            <option value={m} key={m}>
              {m}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="btn"
          disabled={disabled || !pick}
          onClick={() => onApply(pick)}
          title="set every seat's model to this (endpoints untouched)"
        >
          deal
        </button>
      </div>
    </div>
  );
}
