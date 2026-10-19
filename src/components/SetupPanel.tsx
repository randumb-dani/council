import { memo, useEffect, useMemo, useRef, useState } from "react";
import { ArrowsClockwise } from "@phosphor-icons/react/dist/csr/ArrowsClockwise";
import { CaretRight } from "@phosphor-icons/react/dist/csr/CaretRight";
import { Plus } from "@phosphor-icons/react/dist/csr/Plus";
import { X } from "@phosphor-icons/react/dist/csr/X";
import { inTauri, listModels, useServedMode } from "../bridge";
import { keyStatus, setServerKey } from "../serverBridge";

import { seatGlyphs, seatInk, DELEGATE_PALETTE } from "../seats";
import {
  MAX_CUSTOM_DELEGATES,
  STORED_BOUNDS,
  freshEndpoint,
  planEndpointRemoval,
} from "../configStore";
import { MAX_IMAGES, downscaleImage } from "../images";
import {
  PROVIDER_BASE_URL,
  PROVIDER_EFFORTS,
  PROVIDER_LABEL,
  REASONING_EFFORT_HINT,
  ROLE_LABEL,
  canBeTakenByOperator,
  type DebateConfig,
  type DelegateConfig,
  type EndpointConfig,
  type ImageAttachment,
  type Provider,
} from "../types";

/** Extra seats a council may have. Must match the engine's own limit. */
const MAX_CUSTOM_SEATS = MAX_CUSTOM_DELEGATES;

/** At most this many images ride on the motion. Storage, prompts and the engine agree. */

const PALETTE = DELEGATE_PALETTE;


interface Props {
  config: DebateConfig;
  onChange: (next: DebateConfig) => void;
  disabled: boolean;
  /** Discard the stored setup and reload from defaults. */
  onReset: () => void;
}

/**
 * A collapsible block of setup.
 *
 * The setup panel is tall enough that the section you are not editing is noise,
 * and the motion, delegate and procedure blocks are long enough that scrolling
 * past them costs more than it saves. Each section starts collapsed, since the
 * one thing worth reaching for is nearly always Delegates.
 *
 * `initialOpen` exists for the exception. A state that fails validation wants to
 * be visible the moment it appears rather than folded away behind a click —
 * the summary line is what says so, and it is why the heading is a button with
 * the caret inside it rather than a bare toggle.
 *
 * The whole header is the control, so the hit target is the full width of the
 * row; `aria-expanded` is what a screen reader reads, and the caret is
 * `aria-hidden` because "expanded: true" already says which way it points.
 */
function Section({
  title,
  action,
  children,
  hint,
  initialOpen = false,
}: {
  title: string;
  action?: React.ReactNode;
  children: React.ReactNode;
  /** One line under the title when collapsed, so the section says what it holds. */
  hint?: string;
  initialOpen?: boolean;
}) {
  const [open, setOpen] = useState(initialOpen);
  // `initialOpen` is a signal, not just a starting value: the motion section
  // opens when there is no motion, so clearing the motion must reopen it
  // rather than leave the empty field folded away. Only ever opens — a
  // section the operator closed stays closed.
  const wasRequired = useRef(initialOpen);
  useEffect(() => {
    if (initialOpen && !wasRequired.current) setOpen(true);
    wasRequired.current = initialOpen;
  }, [initialOpen]);
  return (
    <section className="border-b border-line">
      <div className="flex items-center gap-1 px-3 py-3">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          className="group flex min-w-0 flex-1 items-center gap-1.5 text-left"
          title={open ? `Collapse ${title}` : `Show ${title}`}
        >
          <CaretRight
            size={12}
            aria-hidden
            className={`shrink-0 text-ink-3 transition-transform group-hover:text-ink-2 ${
              open ? "rotate-90" : ""
            }`}
          />
          <span className="min-w-0">
            <span className="section-title block">{title}</span>
            {!open && hint && (
              <span className="mt-0.5 block truncate text-[11px] text-ink-3">{hint}</span>
            )}
          </span>
        </button>
        {action}
      </div>
      {open && <div className="px-3 pb-4">{children}</div>}
    </section>
  );
}

/**
 * One delegate, collapsible to its own header.
 *
 * A full council is five rows of name, server, model, role line, style and
 * temperature — several screens of setup before anything can be run. The header
 * keeps the two facts that identify the row (the glyph and the name) plus the
 * model, which is what you are scanning for, and drops the rest until asked.
 *
 * A delegate missing a model is open by default: it is the one reason a
 * council will not start, and it should not be behind a disclosure.
 */
function DelegateRow({
  open,
  onToggle,
  children,
  header,
}: {
  open: boolean;
  onToggle: () => void;
  children: React.ReactNode;
  header: React.ReactNode;
}) {
  return (
    <div className="card mb-1.5 p-2">
      <div className="flex items-center gap-1.5">
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={open}
          className="group flex min-w-0 flex-1 items-center gap-1.5 text-left"
          title={open ? "Hide this delegate's settings" : "Show this delegate's settings"}
        >
          <CaretRight
            size={11}
            aria-hidden
            className={`shrink-0 text-ink-3 transition-transform group-hover:text-ink-2 ${
              open ? "rotate-90" : ""
            }`}
          />
          {header}
        </button>
      </div>
      {open && children}
    </div>
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
  // Wrapping is load-bearing, not cosmetic. `whitespace-nowrap` here set the
  // label's min-content width to the entire string, so a row like "let the
  // advocate, critic and rationale look terms up" could not shrink below its
  // full length — the flex row overflowed the sidebar's column and the whole
  // panel gained a horizontal scrollbar. Short labels still sit on one line,
  // because they fit; only the long ones wrap. `items-start` keeps the box
  // beside the first line rather than centring it against a two-line block.
  return (
    <label className="flex cursor-pointer items-start gap-1.5 text-[11.5px] text-ink-2">
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        className="mt-px size-3.5 shrink-0 accent-[var(--primary)]"
      />
      <span className="min-w-0" title={title}>
        {label}
      </span>
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
  title,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  disabled: boolean;
  onChange: (v: number) => void;
  format?: (v: number) => string;
  /**
   * Optional, because a bare number like `4000` rarely explains itself: the
   * label says what is being counted and this says why anyone would change it.
   */
  title?: string;
}) {
  return (
    <label className="min-w-0 flex-1" title={title}>
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

/**
 * A wall-clock reading for "checked N ago", on a timer.
 *
 * `ago()` used to call `Date.now()` during render, and nothing scheduled another
 * render — so the label was computed once and then froze, reading "just now" for
 * the rest of the session until some unrelated edit happened to re-render the
 * panel. A freshness claim that does not freshen is worse than none.
 *
 * Owned per label (see `CheckedAgo`), not per panel: the ticker used to live in
 * `SetupPanel` itself, re-rendering ~60 inputs every 20s for a few "checked N
 * ago" strings. Now only the labels tick, on the same 20s cadence.
 */
function useNow(intervalMs = 20_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), intervalMs);
    return () => window.clearInterval(t);
  }, [intervalMs]);
  return now;
}
/**
 * The "· checked N ago" suffix, ticking on its own 20s clock.
 *
 * Extracted so the interval re-renders only this label: `SetupPanel` holds ~60
 * inputs and is memoized, and waking all of them for a freshness string was
 * the whole tick's cost.
 */
function CheckedAgo({ at }: { at: number }) {
  const now = useNow();
  return <> · checked {ago(at, now)}</>;
}

/** How long ago something happened, said the way a person says it. */
function ago(ms: number, now: number): string {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  return h < 24 ? `${h}h ago` : `${Math.floor(h / 24)}d ago`;
}

/**
 * The server-side key slot for one endpoint.
 *
 * In served mode the browser must never hold the key — not in state, not in
 * storage, not on the wire. This field writes straight to the server's key
 * store and only ever reads back *whether* a key is stored. The value itself
 * has no read path by design.
 */
function ServerKeyField({
  endpointId,
  baseUrl,
  disabled,
}: {
  endpointId: string;
  /** The address this key belongs to. The server binds the key to it and will not
   *  send it anywhere else, so saving without it leaves a key that cannot probe. */
  baseUrl: string;
  disabled: boolean;
}) {
  const [stored, setStored] = useState<boolean | null>(null);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    let live = true;
    setStored(null);
    // The check is per endpoint, but the key is bound to the address: a new
    // address means a new binding, so a stale error must not survive the edit.
    // The typed draft stays — it is saved against whatever address is current.
    setError("");
    keyStatus([endpointId])
      .then((s) => {
        if (live) setStored(s[endpointId] === true);
      })
      .catch((e) => {
        if (live) setError(String(e instanceof Error ? e.message : e));
      });
    return () => {
      live = false;
    };
  }, [endpointId, baseUrl]);
  const save = async () => {
    if (!draft.trim() || busy) return;
    setBusy(true);
    setError("");
    try {
      setStored(await setServerKey(endpointId, draft, baseUrl));
      setDraft("");
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  };
  const forget = async () => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      setStored(await setServerKey(endpointId, null));
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="min-w-0">
      <div className="flex flex-wrap items-center gap-2">
        <input
          className="field min-w-0 flex-1 font-mono !text-[12px]"
          type="password"
          value={draft}
          placeholder={stored ? "key stored on the server — type to replace" : "api key, stored on the server"}
          disabled={disabled || busy}
          autoComplete="off"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void save();
          }}
        />
        <button type="button" className="btn" disabled={disabled || busy || !draft.trim()} onClick={() => void save()}>
          {stored ? "Replace" : "Store"}
        </button>
        {stored && (
          <button type="button" className="btn btn-ghost" disabled={disabled || busy} onClick={() => void forget()}>
            Forget
          </button>
        )}
      </div>
      <p className="mt-1 text-[11px] leading-relaxed text-ink-3" role="status">
        {error
          ? error
          : stored === null
            ? "Asking the server whether a key is stored…"
            : stored
              ? "A key is stored on this machine's server. Served debates use it; the browser never sees it."
              : "No key stored on the server for this endpoint."}
      </p>
    </div>
  );
}

/** A model picker backed by the endpoint's own list. Falls back to free text
 *  when the list was never fetched, and always allows a custom name. */
function ModelPicker({
  value,
  models,
  disabled,
  error,
  onChange,
}: {
  value: string;
  /** null when never fetched, an array once it has been. */
  models: string[] | null;
  disabled: boolean;
  /**
   * Why the fetch failed, if it did.
   *
   * Needed because `models` alone cannot tell "not asked yet" from "asked and
   * failed" — both are null. The old code said "checking the endpoint…" for
   * both, so a server that could not be reached left every one of its delegates
   * claiming a check was in progress that had ended, with the actual error
   * 400px up in the endpoint card, possibly inside a collapsed section.
   */
  error?: string;
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
          /* Three states, not two. `models === null` used to mean both "never
             asked" and "asked and failed", so a dead server left every delegate
             on it reading "checking the endpoint…" forever. */
          placeholder={
            error
              ? "model name — the server's list could not be fetched"
              : models === null
                ? "model name — checking the endpoint…"
                : "model name, e.g. unsloth/Qwen3-27B"
          }
          disabled={disabled}
          onChange={(e) => onChange(e.target.value)}
          aria-label="model name"
          /* `invalid` only for a real failure, so a deliberate "I typed my own"
             is not flagged as an error. */
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? "model-picker-error" : undefined}
        />
        {error && (
          <p id="model-picker-error" className="mt-1 text-[11px] leading-relaxed text-bad">
            Could not list this server&apos;s models: {error}. You can still type a name.
          </p>
        )}
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

  // Picks that arrive while another is being downscaled wait in line instead of
  // being dropped: the file input used to stay enabled during `busy` yet `add`
  // returned early on it, so the selection vanished silently. The refs carry
  // the latest list and the running flag, because neither the input handler
  // nor the paste listener re-renders between picks.
  const imagesRef = useRef(images);
  imagesRef.current = images;
  const queueRef = useRef<File[]>([]);
  const runningRef = useRef(false);

  const add = async (files: FileList | File[] | null) => {
    if (!files || disabled) return;
    const pending: File[] = Array.isArray(files) ? files : Array.from(files);
    const fresh = pending.filter((f) => f.type.startsWith("image/"));
    if (!fresh.length) return;
    queueRef.current.push(...fresh);
    if (runningRef.current) return;
    runningRef.current = true;
    setBusy(true);
    setError("");
    try {
      const failed: string[] = [];
      while (queueRef.current.length > 0) {
        const slots = MAX_IMAGES - imagesRef.current.length;
        if (slots <= 0) {
          queueRef.current = [];
          setError(`Only ${MAX_IMAGES} images fit — the rest were left out.`);
          break;
        }
        const batch = queueRef.current.splice(0, slots);
        const done: ImageAttachment[] = [];
        for (const f of batch) {
          try {
            if (f.size > 20 * 1024 * 1024) throw new Error(`"${f.name}" is over 20MB`);
            done.push(await downscaleImage(f));
          } catch (e) {
            failed.push(e instanceof Error ? e.message : String(e));
          }
        }
        if (done.length) onChange([...imagesRef.current, ...done].slice(0, MAX_IMAGES));
      }
      if (failed.length) setError(failed.join(" "));
    } finally {
      runningRef.current = false;
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
        <label className={`btn btn-ghost ${disabled || slotsLeft <= 0 ? "opacity-40" : ""}`}>
          {slotsLeft <= 0 ? "full" : busy ? "reading…" : "add images"}
          <input
            type="file"
            accept="image/*"
            multiple
            className="hidden"
            disabled={disabled || slotsLeft <= 0}
            onChange={(e) => {
              void add(e.target.files);
              e.target.value = "";
            }}
            aria-label="attach images for the council to argue over"
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
      {/* "Every seat" → "every delegate", and "vision-capable" named what
          actually happens, which is the part a user needs before attaching. */}
      <p className="mt-1 text-[11px] leading-relaxed text-ink-3">
        Add files above, or copy a screenshot and press Ctrl+V anywhere outside
        a text field. Every delegate receives them with its opening message. The
        models you pick must accept images — one that cannot will fail on its
        first turn rather than ignore them.
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
  const served = useServedMode();
  // `probe` outlives renders through `probeRef`, so it reads the ref mirror
  // rather than a stale `served` closure.
  const servedRef = useRef(served);
  servedRef.current = served;

  // Which delegate rows are expanded. A set, not a count: the panel holds a
  // dozen rows and the useful state is which ones, not how many.
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  // Reset is two-step, so the confirmation has to live somewhere: `onReset`
  // reloads the window and cannot be the thing that renders the prompt.
  const [confirmingReset, setConfirmingReset] = useState(false);
  // Which server's removal is armed. Null for the common case where no delegate
  // would move and the click is already unambiguous.
  const [pendingRemoval, setPendingRemoval] = useState<string | null>(null);
  // The last thing a provider switch silently dropped. Lives here rather than in
  // a notice because the panel has no notice channel, and the cleared controls
  // are disabled — so without this the operator sees a change with no trace.
  const [cleared, setCleared] = useState("");
  useEffect(() => {
    // Never left armed behind a later edit — an armed confirm that survives
    // whatever the operator did next is how you erase a setup you just finished
    // writing.
    setConfirmingReset(false);
  }, [config]);
  // Rows this effect opened on its own, so it can close them again once fixed.
  // A row the operator opened by hand is theirs: auto-closing it mid-edit
  // would yank the panel shut while they type.
  const autoOpened = useRef<Set<string>>(new Set());
  // A delegate with no model is the one thing that stops the debate starting,
  // so it is open rather than folded behind a click. Taken seats need no model
  // — the operator is the model — so they are left alone.
  useEffect(() => {
    setExpanded((prev) => {
      const next = new Set(prev);
      let changed = false;
      const fixed = new Set<string>();
      for (const d of config.delegates) {
        if (!d.model.trim() && d.human !== true) {
          if (!next.has(d.id)) {
            next.add(d.id);
            autoOpened.current.add(d.id);
            changed = true;
          }
        } else {
          fixed.add(d.id);
        }
      }
      for (const id of autoOpened.current) {
        if (fixed.has(id) && next.has(id)) {
          next.delete(id);
          changed = true;
        }
      }
      for (const id of [...autoOpened.current]) {
        if (!config.delegates.some((d) => d.id === id)) autoOpened.current.delete(id);
      }
      return changed ? next : prev;
    });
  }, [config.delegates]);

  // Every updater below reads through this ref rather than the render-closed
  // `config`: a slider and a checkbox landing in the same tick, or two rapid
  // clicks on "add", each saw the same stale roster and one update ate the
  // other (duplicate endpoint ids, lost toggles).
  const cfgRef = useRef(config);
  cfgRef.current = config;

  const patch = (p: Partial<DebateConfig>) => onChange({ ...cfgRef.current, ...p });

  const setEndpoint = (id: string, p: Partial<EndpointConfig>) =>
    patch({ endpoints: cfgRef.current.endpoints.map((e) => (e.id === id ? { ...e, ...p } : e)) });

  const setDelegate = (id: string, p: Partial<DelegateConfig>) =>
    patch({ delegates: cfgRef.current.delegates.map((d) => (d.id === id ? { ...d, ...p } : d)) });

  const probe = async (ep: EndpointConfig, silent: boolean) => {
    if (!inTauri() && !servedRef.current) {
      if (!silent) {
        setProbeError((e) => ({
          ...e,
          [ep.id]: "Browser preview — models list only inside the desktop app or served by the local server. Type the name instead.",
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

  // The list used to sit empty until "test connection" was pressed, so every
  // dropdown read stale. Refresh shortly after the address or key changes,
  // and once on mount.
  //
  // `provider` is in the key because choosing one rewrites the address. Without
  // it, switching to Gemini would leave the previous server's address in place
  // and probe that instead — the change would appear to do nothing.
  const autoKey = config.endpoints
    .map((e) => `${e.id}|${e.provider}|${e.baseUrl}|${e.apiKey ?? ""}`)
    .join("~");
  useEffect(() => {
    if (disabled || (!inTauri() && !servedRef.current)) return;
    const t = setTimeout(() => {
      for (const ep of cfgRef.current.endpoints) {
        if (ep.baseUrl.trim()) void probeRef.current(ep, true);
      }
    }, 800);
    return () => clearTimeout(t);
  }, [autoKey, disabled, served]);

  const addEndpoint = () => {
    const live = cfgRef.current.endpoints;
    // `freshEndpoint` derives its id from the clock: two clicks in the same
    // millisecond minted the same id twice. The suffix makes it unique even then.
    const ep = {
      ...freshEndpoint(live.length),
      id: `ep-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`,
    };
    patch({ endpoints: [...live, ep] });
  };

  const removalWarning = (id: string) => planEndpointRemoval(cfgRef.current, id).sentence;

  const removeEndpoint = (id: string) => {
    // The fallback must come from the list that survives the deletion. Using
    // the old first element re-points delegates at the endpoint being removed,
    // which then fails at turn time with "no endpoint configured".
    const live = cfgRef.current;
    const plan = planEndpointRemoval(live, id);
    const apply = () => {
      const endpoints = live.endpoints.filter((e) => e.id !== id);
      patch({
        endpoints,
        delegates: live.delegates.map((d) =>
          d.endpointId === id ? { ...d, endpointId: plan.fallback } : d,
        ),
      });
    };
    if (plan.sentence) {
      // Armed per server, so confirming one deletion cannot confirm another.
      if (pendingRemoval !== id) {
        setPendingRemoval(id);
        return;
      }
      setPendingRemoval(null);
    }
    apply();
  };

  // The engine refuses a council with more than this many extra seats. Keeping
  // the number here as well means the button goes quiet instead of letting you
  // build a setup that will not convene.
  const customSeats = config.delegates.filter((d) => d.seatRole === "custom").length;
  const atSeatLimit = customSeats >= MAX_CUSTOM_SEATS;

  const addDelegate = () => {
    if (atSeatLimit) return;
    const live = cfgRef.current.delegates;
    const i = live.length;
    // Numbering from the array length collides after a deletion: add a seat, add
    // another, delete the first, add again — and `i` lands on a label already in
    // use. The engine refuses duplicate labels, and `problems` never checked, so the
    // button read "start the debate with these settings" and the debate refused to
    // convene with an error the panel gave no hint of. First unused number instead.
    const taken = new Set(live.map((d) => d.label.toLowerCase()));
    let n = live.length + 1;
    while (taken.has(`delegate ${n}`)) n += 1;
    // And a colour nobody else has: `PALETTE[i % len]` cycles, and the entry that
    // collides with Advocate's is `PALETTE[0]`.
    const usedColors = new Set(live.map((d) => d.color.toLowerCase()));
    const color =
      PALETTE.find((c) => !usedColors.has(c.toLowerCase())) ??
      PALETTE[i % PALETTE.length];
    patch({
      delegates: [
        ...live,
        {
          id: `seat-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`,
          label: `Delegate ${n}`,
          model: "",
          endpointId: cfgRef.current.endpoints[0]?.id ?? "",
          color,
          seatRole: "custom",
          role: null,
          persona: null,
          temperature: 0.7,
        },
      ],
    });
  };

  const removeDelegate = (id: string) =>
    patch({ delegates: cfgRef.current.delegates.filter((d) => d.id !== id) });

  const applyModelToAll = (model: string) => {
    if (!model) return;
    patch({ delegates: cfgRef.current.delegates.map((d) => ({ ...d, model })) });
  };

  // Every model *all* endpoints currently list, for the one-model-everywhere row.
  // Intersection, not union. The union offered `qwen3-27b` because one endpoint had
  // it, and "use for all" then wrote that name to every delegate including the ones
  // on an endpoint that does not serve it — so a debate started, every field-level
  // check passed, and each of those seats failed on its first turn with a
  // model-not-found. A model offered for *all* delegates has to be one all endpoints
  // can serve. With a single endpoint this is that endpoint's list, unchanged.
  const lists = Object.values(models).filter((m): m is string[] => Array.isArray(m));
  const allModels = (
    lists.length === 0
      ? []
      : lists.reduce((acc, m) => acc.filter((x) => m.includes(x)))
  ).sort();
  // Seat marks, so a seat is identifiable before the debate starts — which is
  // exactly when the monochrome themes have nothing but the mark to go on.
  const glyphs = useMemo(
    () => seatGlyphs(config.delegates.map((d) => ({ id: d.id, role: d.seatRole }))),
    [config.delegates],
  );

  return (
    <div>
      <Section
        title="The motion — what the council is deciding"
        hint={config.motion.trim() || undefined}
        initialOpen={!config.motion.trim()}
      >
        <textarea
          className="field mb-2 px-3 py-2 text-[13.5px] leading-relaxed"
          rows={4}
          placeholder="What should the council decide? For example: which of these two prompts gives better organised output?"
          aria-label="the question the council is deciding"
          value={config.motion}
          disabled={disabled}
          onChange={(e) => patch({ motion: e.target.value })}
        />
        <textarea
          className="field"
          rows={2}
          placeholder="Instructions every delegate follows (optional)"
          aria-label="instructions every delegate follows"
          value={config.extraInstructions ?? ""}
          disabled={disabled}
          onChange={(e) => patch({ extraInstructions: e.target.value || null })}
        />
        {/* The record of the debate being resumed. Its own box, and its own field,
            because it used to be written over the operator's standing instructions
            above — and then saved, so the next unrelated debate inherited it too. */}
        {config.resumeRecord && (
          <details className="mt-2 rounded-md border border-line bg-surface-2 px-3 py-2">
            <summary className="cursor-pointer text-[12px] text-ink-2">
              Record of the unfinished debate you are resuming
            </summary>
            <p className="mt-1.5 text-[11.5px] leading-relaxed text-ink-3">
              Every delegate is handed this before their first turn. The board starts
              empty — this is the argument so far, not a board to carry over.
            </p>
            <textarea
              className="field mt-2 font-mono !text-[11.5px]"
              rows={8}
              aria-label="record of the unfinished debate being resumed"
              value={config.resumeRecord}
              disabled={disabled}
              onChange={(e) => patch({ resumeRecord: e.target.value || null })}
            />
            <button
              type="button"
              className="btn btn-ghost mt-1.5 !py-1 text-[11.5px]"
              onClick={() => patch({ resumeRecord: null })}
            >
              clear the record
            </button>
          </details>
        )}
        <ImagePicker
          images={config.images}
          disabled={disabled}
          onChange={(images) => patch({ images })}
        />
      </Section>

      <Section
        title="Model servers"
        hint={`${config.endpoints.length} configured`}
        initialOpen
        action={
          <button type="button" className="btn btn-ghost !px-2.5" onClick={addEndpoint} disabled={disabled} title="Add another model server">
            <Plus size={13} /> add
          </button>
        }
      >
        {cleared && (
          <p className="mb-1.5 text-[11px] leading-relaxed text-ink-3">{cleared}.</p>
        )}
        {config.endpoints.map((ep) => {
          const list = models[ep.id] ?? null;
          const err = probeError[ep.id] ?? "";
          const busy = !!checking[ep.id];
          const checked = lastChecked[ep.id];
          return (
            <div key={ep.id} className="card mb-1.5 p-2">
              <div className="mb-1.5 flex items-center gap-1.5">
                {/* A bare coloured dot is unreadable without its legend, and this
                    tooltip is the only place it existed. Each state now says what
                    it means and what to do. */}
                <span
                  className={`size-2 shrink-0 rounded-full ${
                    err ? "bg-bad" : list ? "bg-ok" : "bg-line-2"
                  } ${busy ? "live-dot" : ""}`}
                  title={
                    busy
                      ? "Checking this server for models…"
                      : err
                        ? `Could not reach this server: ${err}`
                        : list
                          ? `Reached the server — ${list.length} model${list.length === 1 ? "" : "s"} available.`
                          : "Not checked yet. Use check to ask the server which models it has loaded."
                  }
                />
                <input
                  className="field"
                  value={ep.name}
                  placeholder="a name for it"
                  disabled={disabled}
                  onChange={(e) => setEndpoint(ep.id, { name: e.target.value })}
                />
                {pendingRemoval === ep.id ? (
                  /* The second step sits where the first was, so the eye does
                     not have to find a banner elsewhere to see what it is about
                     to agree to. */
                  <div className="flex items-center gap-1.5">
                    <button
                      type="button"
                      className="btn btn-danger !px-2"
                      disabled={disabled}
                      onClick={() => removeEndpoint(ep.id)}
                      title="remove it and move its delegates"
                    >
                      remove
                    </button>
                    <button
                      type="button"
                      className="btn btn-ghost !px-2"
                      onClick={() => setPendingRemoval(null)}
                    >
                      <X size={13} />
                      <span className="sr-only">keep this server</span>
                    </button>
                  </div>
                ) : (
                  <button
                    type="button"
                    className="btn btn-ghost !px-2"
                    disabled={disabled}
                    onClick={() => removeEndpoint(ep.id)}
                    title="remove this model server"
                    aria-label="remove this model server"
                  >
                    <X size={13} />
                  </button>
                )}
              </div>
              {pendingRemoval === ep.id && removalWarning(ep.id) && (
                <p className="mt-1 text-[11px] leading-relaxed text-warn">
                  {removalWarning(ep.id)}
                </p>
              )}
              {/* Provider first, because it is what sets the address below and the
                  reasoning vocabulary further down. Changing it rewrites the
                  address to the documented one — which is a deliberate
                  overwrite, so it is only done when the address is still
                  untouched, and never on a custom or proxied address the user
                  has clearly typed. */}
              <select
                className="field"
                value={ep.provider}
                disabled={disabled}
                onChange={(e) => {
                  const provider = e.target.value as Provider;
                  const untouched =
                    !ep.baseUrl.trim() || ep.baseUrl === PROVIDER_BASE_URL[ep.provider];
                  // Named out loud, because the fields below are then disabled and
                  // the operator cannot see what was just thrown away. One select
                  // click erased a reasoning mode that took a dozen clicks to set,
                  // and the only evidence afterwards was a greyed-out control.
                  const cleared =
                    provider === "gemini" && ep.reasoningWire !== "serverDefault"
                      ? "the reasoning mode was reset — Gemini does not use the OpenAI setting"
                      : provider === "anthropic" &&
                          (ep.reasoningWire !== "serverDefault" ||
                            ep.reasoningEffort !== null ||
                            ep.thinking !== null)
                        ? "the reasoning settings were cleared — Anthropic ignores them in this release"
                        : "";
                  if (cleared) setCleared(cleared);
                  setEndpoint(ep.id, {
                    provider,
                    ...(untouched ? { baseUrl: PROVIDER_BASE_URL[provider] } : {}),
                    // Gemini spells reasoning differently and rejects the
                    // OpenAI shape outright, so an effort that was valid for the
                    // previous provider may not be for this one. Anthropic
                    // thinking controls are also display-only in this release,
                    // so clear all three rather than sending a stale mode.
                    ...(provider === "gemini" && ep.reasoningWire !== "serverDefault"
                      ? { reasoningWire: "serverDefault" as const }
                      : {}),
                    ...(provider === "anthropic" &&
                    (ep.reasoningWire !== "serverDefault" ||
                      ep.reasoningEffort !== null ||
                      ep.thinking !== null)
                      ? {
                          reasoningWire: "serverDefault" as const,
                          reasoningEffort: null,
                          thinking: null,
                        }
                      : {}),
                  });
                }}
                title="Which service this is. Sets the address below, which you can still change"
              >
                {(Object.keys(PROVIDER_LABEL) as Provider[]).map((p) => (
                  <option value={p} key={p}>
                    {PROVIDER_LABEL[p]}
                  </option>
                ))}
              </select>
              <input
                className="field font-mono !text-[12px]"
                value={ep.baseUrl}
                placeholder={PROVIDER_BASE_URL[ep.provider]}
                disabled={disabled}
                onChange={(e) => setEndpoint(ep.id, { baseUrl: e.target.value })}
              />
              {served ? (
                <ServerKeyField
                  endpointId={ep.id}
                  baseUrl={ep.baseUrl}
                  disabled={disabled}
                />
              ) : (
                <input
                  className="field font-mono !text-[12px]"
                  type="password"
                  value={ep.apiKey ?? ""}
                  /* "optional" on its own was the whole problem: it reads as
                     "this server will accept a request with no key", which is
                     true of llama.cpp and Ollama and false of Unsloth — whose
                     own settings advertise keyless access while every /v1 route
                     still wants a Bearer token. An operator leaves it blank,
                     gets `unauthenticated`, and concludes their setup is wrong.
                     Naming who it is optional for costs one line and removes
                     the trap. */
                  placeholder="api key — blank only if this server allows it"
                  disabled={disabled}
                  onChange={(e) => setEndpoint(ep.id, { apiKey: e.target.value || null })}
                />
              )}

              <div className="mb-2 flex flex-wrap items-center gap-x-3 gap-y-1.5">
                <Check
                  checked={ep.nativeTools}
                  disabled={disabled}
                  onChange={(v) => setEndpoint(ep.id, { nativeTools: v })}
                  label="let models call tools"
                  title="Lets a model look things up itself, instead of only writing text"
                />
                <Check
                  checked={ep.stream}
                  disabled={disabled}
                  onChange={(v) => setEndpoint(ep.id, { stream: v })}
                  label="stream replies"
                  title="Shows each reply as it is written, instead of all at once when it finishes. Turn this off if you are tunnelling through Cloudflare, which drops the connection that streaming depends on."
                />
                <Check
                  checked={ep.allowInvalidCerts}
                  disabled={disabled}
                  onChange={(v) => setEndpoint(ep.id, { allowInvalidCerts: v })}
                  label="accept self-signed certificate"
                  title="For a server on your own machine using an https certificate it issued itself. Leave this off for anything on the internet."
                />
                <Check
                  checked={ep.includeUsage}
                  disabled={disabled}
                  onChange={(v) => setEndpoint(ep.id, { includeUsage: v })}
                  label="report token usage"
                  title={
                    ep.provider === "anthropic"
                      ? "Anthropic reports usage with every response, so counts come from its final tally."
                      : "Ask the server to report token usage with each reply, so the counts in the rail are the server's own rather than estimated. The rail counts tokens, not words."
                  }
                />
                <Check
                  checked={ep.thinking === false}
                  disabled={disabled || ep.provider === "anthropic"}
                  onChange={(v) => setEndpoint(ep.id, { thinking: v ? false : null })}
                  label="hide thinking"
                  title={
                    ep.provider === "anthropic"
                      ? "Anthropic thinking is shown when returned, but its model-specific controls are not sent in this release."
                      : "Stops the model showing its working before it answers. Left off, whatever the model does by default applies — which for most current models means it thinks out loud."
                  }
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
                  disabled={disabled || ep.provider === "anthropic"}
                  onChange={(ev) =>
                    setEndpoint(ep.id, {
                      reasoningWire: ev.target.value as EndpointConfig["reasoningWire"],
                    })
                  }
                  title={
                    ep.provider === "anthropic"
                      ? "Anthropic thinking controls are model-specific and are not sent in this release."
                      : "How your server accepts this setting. Leave it on the first option unless a request fails — only some servers need the others."
                  }
                >
                  <option value="serverDefault">let the server decide</option>
                  <option value="chatTemplate">send as a template argument</option>
                  <option value="flat">send as a top-level field</option>
                </select>
                <input
                  className="field !w-28 !py-1 font-mono text-[11.5px]"
                  list={`reasoning-effort-levels-${ep.id}`}
                  placeholder={ep.reasoningWire === "serverDefault" ? "server's choice" : "medium"}
                  value={ep.reasoningEffort ?? ""}
                  disabled={
                    disabled ||
                    ep.provider === "anthropic" ||
                    ep.reasoningWire === "serverDefault"
                  }
                  onChange={(e) =>
                    setEndpoint(ep.id, { reasoningEffort: e.target.value.trim() || null })
                  }
                  title={
                    ep.provider === "anthropic"
                      ? "Anthropic thinking is shown when returned, but its model-specific effort controls are not sent in this release."
                      : `Type a level ${PROVIDER_LABEL[ep.provider]} accepts — ${PROVIDER_EFFORTS[ep.provider].join(", ")}. One it does not recognise fails the request rather than being ignored.`
                  }
                  aria-label="how much the model thinks before answering"
                />
                {/* The levels differ per provider — DeepSeek adds `max`, Gemini
                    uses its own names and rejects an OpenAI-shaped value — so
                    the list follows the picker rather than offering one fixed
                    set that is wrong for most of them. */}
                <datalist id={`reasoning-effort-levels-${ep.id}`}>
                  {PROVIDER_EFFORTS[ep.provider].map((level) => (
                    <option value={level} key={level} />
                  ))}
                </datalist>
                <span className="text-[11px] text-ink-3">{REASONING_EFFORT_HINT}</span>
              </div>

              <div className="flex items-center gap-2">
                <button
                  type="button"
                  className="btn btn-ghost"
                  disabled={disabled || busy}
                  onClick={() => probe(ep, false)}
                  title="Ask this server which models it currently has loaded"
                >
                  {busy ? <span className="spinner" /> : <ArrowsClockwise size={13} />}
                  {busy ? "checking…" : list ? "refresh" : "check"}
                </button>
                <Slider
                  label="timeout"
                  value={ep.timeoutSecs}
                  min={STORED_BOUNDS.endpointTimeoutSecs.min}
                  max={STORED_BOUNDS.endpointTimeoutSecs.max}
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
                    {checked ? <CheckedAgo at={checked} /> : null}
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
                  role="alert"
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
        hint={`${config.delegates.length} in the council`}
        initialOpen
        action={
          <button
            type="button"
            className="btn btn-ghost !px-2.5"
            onClick={addDelegate}
            disabled={disabled || atSeatLimit}
            title={
              atSeatLimit
                ? `A council can have at most ${MAX_CUSTOM_SEATS} delegates beyond the required five`
                : "Add another delegate to argue the motion"
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
        {/* The row above only offers models every server lists. When it is
            missing, say why — otherwise the panel just looks unfinished. */}
        {config.delegates.length >= 2 && allModels.length === 0 && (
          <p className="mb-2 text-[11.5px] text-ink-3">
            No model is listed by every server — pick per delegate below, or check
            the servers above.
          </p>
        )}
        {config.delegates.length < 2 && (
          <p className="mb-2 text-[11.5px] text-ink-3">
            A council needs at least two delegates — one to argue each side.
          </p>
        )}
        {config.delegates.map((d, i) => {
          // The fixed seats are fixed furniture: their labels never edit
          // and they cannot be removed. Everything else about the seat —
          // model, endpoint, voice — configures like any other.
          const essential = d.seatRole !== "custom";
          const slot = Math.min(i + 1, 12);
          return (
          <DelegateRow
            key={d.id}
            open={expanded.has(d.id)}
            onToggle={() =>
              setExpanded((prev) => {
                const next = new Set(prev);
                if (next.has(d.id)) next.delete(d.id);
                else next.add(d.id);
                return next;
              })
            }
            header={
              <>
                <span
                  className="avatar size-4 shrink-0 text-[9px]"
                  style={{
                    background: `var(--seat-${slot}, ${d.color})`,
                    color: seatInk(slot, d.color),
                  }}
                  aria-hidden
                >
                  {glyphs[d.id] ?? "?"}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[12.5px] font-medium text-ink">
                    {d.label}
                  </span>
                  {/* The model is what you are scanning a council for, so it
                      stays visible when the row is closed. Blank means the one
                      thing that will stop the debate starting. */}
                  {/* The model is what you are scanning a council for, so it
                      stays visible when the row is closed. Blank means the one
                      thing that will stop the debate starting — unless the seat
                      is yours, which needs neither. */}
                  <span
                    className={`block truncate font-mono text-[10.5px] ${
                      d.human === true
                        ? "text-primary"
                        : d.model.trim()
                          ? "text-ink-3"
                          : "text-warn"
                    }`}
                  >
                    {d.human === true
                      ? "you argue this role"
                      : d.model.trim() || "no model chosen yet"}
                  </span>
                </span>
                {d.human === true && (
                  <span className="shrink-0 rounded-pill bg-primary px-1.5 py-px text-[10px] tracking-wide text-on-primary uppercase">
                    you
                  </span>
                )}
                <span className="rounded-pill bg-surface-3 px-1.5 py-px text-[10px] tracking-wide text-ink-2 uppercase">
                  {ROLE_LABEL[d.seatRole]}
                </span>
              </>
            }
          >
            <div className="mb-1.5 flex items-center gap-1.5">
              {essential ? (
                <span className="field flex items-center justify-between !bg-surface-2" title="One of the five required delegates, so it can't be renamed or removed">
                  <span className="font-medium text-ink">{d.label}</span>
                  <span className="rounded-pill bg-surface-3 px-1.5 py-px text-[10px] tracking-wide text-ink-2 uppercase">
                    {ROLE_LABEL[d.seatRole]}
                  </span>
                </span>
              ) : (
                <input
                  className="field"
                  value={d.label}
                  placeholder="what to call them"
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
                  title="remove this delegate"
                  aria-label="remove this delegate"
                >
                  <X size={13} />
                </button>
              )}
            </div>
            {/* You can take an arguing seat yourself. The model and the server
                are then not requirements — the operator is the model — so they
                are hidden rather than merely ignored, which would otherwise look
                like the setup was broken. */}
            <Check
              checked={d.human === true}
              // Gated to the debating roles. This rendered for the Arbiter and the
              // Summarizer too, and `problems` then refused the whole configuration
              // with an error the field itself gave no hint of.
              disabled={disabled || !canBeTakenByOperator(d.seatRole)}
              onChange={(v) =>
                setDelegate(d.id, { human: v || undefined })
              }
              label="you argue this role"
              title={
                canBeTakenByOperator(d.seatRole)
                  ? "The council waits for you instead of calling a model. You write the speech and make the moves with the action picker."
                  : "Only the debating roles can be taken: an Arbiter ruling on an argument they took part in is not adjudication, and a Summarizer writing the result they argued for is not a record."
              }
            />
            {d.human === true && (
              <p className="-mt-1 mb-1.5 rounded-lg bg-primary-soft px-2.5 py-1.5 text-[11px] leading-relaxed text-ink-2">
                You hold this seat. The debate pauses until you answer, and the
                other seats read what you write. Skip passes your turn if you are
                away.
              </p>
            )}
            {d.human !== true && (
              <>
                <ModelPicker
                  value={d.model}
                  models={models[d.endpointId] ?? null}
                  error={probeError[d.endpointId] || undefined}
                  disabled={disabled}
                  onChange={(v) => setDelegate(d.id, { model: v })}
                />
                <select
                  className="field"
                  value={d.endpointId}
                  disabled={disabled}
                  onChange={(e) => setDelegate(d.id, { endpointId: e.target.value })}
                  title="Which server this delegate talks to"
                >
                  {config.endpoints.length === 0 && <option value="">no endpoints</option>}
                  {config.endpoints.map((ep) => (
                    <option value={ep.id} key={ep.id}>
                      {ep.name || ep.baseUrl}
                    </option>
                  ))}
                </select>
              </>
            )}
            <input
              className="field"
              value={d.role ?? ""}
              placeholder="what this delegate is for"
              disabled={disabled}
              onChange={(e) => setDelegate(d.id, { role: e.target.value || null })}
            />
            <input
              className="field"
              value={d.persona ?? ""}
              placeholder="its style or tone (optional)"
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
          </DelegateRow>
          );
        })}
      </Section>

      <Section title="Decisions" hint="Who settles it when the delegates disagree">
        {/* "classification, not deliberation" and "burning a turn per voter"
            described the implementation to nobody. What the user is choosing is
            which model tallies the votes, so that is what this now says. */}
        <p className="mb-2 text-[11px] leading-relaxed text-ink-3">
          When the delegates disagree on whether a point should stand, something
          has to settle it. You can let each delegate count its own votes, or point
          the counting at one separate model that answers for everyone. Votes
          always come with a reason either way.
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
          title="Which model settles a disagreement about whether a point stands"
        >
          <option value="seat">Each delegate decides for itself</option>
          <option value="jev">One separate model decides for everyone</option>
        </select>
        {config.decision.provider === "jev" && (
          <>
            <input
              className="field font-mono !text-[12px]"
              value={config.decision.baseUrl}
              placeholder="address of the deciding model"
              disabled={disabled}
              onChange={(e) =>
                patch({ decision: { ...config.decision, baseUrl: e.target.value } })
              }
            />
            <input
              className="field"
              value={config.decision.model}
              placeholder="model that settles the votes"
              disabled={disabled}
              onChange={(e) =>
                patch({ decision: { ...config.decision, model: e.target.value } })
              }
            />
            {served ? (
              <ServerKeyField
                endpointId="decision"
                baseUrl={config.decision.baseUrl}
                disabled={disabled}
              />
            ) : (
              <input
                className="field font-mono !text-[12px]"
                type="password"
                value={config.decision.apiKey ?? ""}
                placeholder="api key — blank only if this server allows it"
                disabled={disabled}
                onChange={(e) =>
                  patch({
                    decision: { ...config.decision, apiKey: e.target.value || null },
                  })
                }
              />
            )}
            <div className="flex items-center gap-2">
              <span className="shrink-0 text-[11px] text-ink-3">timeout</span>
              <Slider
                label=""
                value={config.decision.timeoutSecs}
                min={STORED_BOUNDS.decisionTimeoutSecs.min}
                max={STORED_BOUNDS.decisionTimeoutSecs.max}
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
              back to the delegate's own model and says so.
            </p>
          </>
        )}
      </Section>

      <Section
        title="Procedure"
        hint={`${config.maxRounds} rounds, up to ${config.maxExtraRounds} extra`}
      >
        {/* Each slider names its own ceiling in the label, because "rounds 20"
            reads as a count of rounds already spoken rather than the cap. */}
        <div className="mb-2 flex gap-3">
          <Slider
            label="rounds, max"
            value={config.maxRounds}
            min={STORED_BOUNDS.maxRounds.min}
            max={STORED_BOUNDS.maxRounds.max}
            step={1}
            disabled={disabled}
            onChange={(v) => patch({ maxRounds: v })}
          />
          <Slider
            label="extra rounds"
            value={config.maxExtraRounds}
            min={STORED_BOUNDS.maxExtraRounds.min}
            max={STORED_BOUNDS.maxExtraRounds.max}
            step={1}
            disabled={disabled}
            onChange={(v) => patch({ maxExtraRounds: v })}
          />
        </div>
        {/* Was three untranslated engineering phrases — "the summarizer's whole
            budget", "scale with the board", "duplicates folded, looping
            capped". None of that tells a user what the two numbers do or when
            they would want more. Now: what the number is, then what happens
            when the Summarizer asks. */}
        <p className="mb-2 text-[11px] leading-relaxed text-ink-3">
          Rounds is how many times every delegate gets to speak. Extra rounds is
          how many more the Summarizer may ask for once the planned rounds are
          used up — you approve each request, and it stops at the number you set
          here.
        </p>
        <p className="mb-2 text-[11px] leading-relaxed text-ink-3">
          Reset clears everything back to the starting setup, including any
          delegates or servers you added.
        </p>
          {/* Two-step, and the second step names what is going.
              `resetConfig` removes the whole store key and reloads: the motion,
              the extra instructions, every delegate you renamed, every server you
              added, and any images on them. One unconfirmed ghost button next to
              a readable checkbox was the only destructive action in the app that
              ignored the confirm idiom the library already uses. */}
          {confirmingReset ? (
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-[11px] leading-relaxed text-warn">
                Erase this setup and reload? Everything you have set up goes.
              </span>
              <button type="button" className="btn btn-danger !py-1 text-[11.5px]" onClick={onReset}>
                erase it
              </button>
              <button
                type="button"
                className="btn btn-ghost !py-1 text-[11.5px]"
                onClick={() => setConfirmingReset(false)}
              >
                keep it
              </button>
            </div>
          ) : (
            <button
              type="button"
              className="btn btn-ghost"
              onClick={() => setConfirmingReset(true)}
              disabled={disabled}
            >
              reset setup
            </button>
          )}
          <div className="flex flex-col gap-1.5">
            <Check
              checked={config.autoAdjourn}
              disabled={disabled}
              onChange={(v) => patch({ autoAdjourn: v })}
              label="stop early once every point is settled"
              title="Ends the debate as soon as nothing is left undecided, rather than running every round you planned."
            />
            <Check
              checked={config.closingStatements}
              disabled={disabled}
              onChange={(v) => patch({ closingStatements: v })}
              label="closing statements before the answer"
              title="Gives each delegate one last turn to sum up its case, after all the points have been argued."
            />
          </div>
      </Section>

      {/* Advanced.
          The form was 1600 lines with no split between what a run needs and what
          a run occasionally needs. Everything here is real and stays reachable — but it
          is a separate model to configure, a separate shelf to enable, and a separate
          button whose consequence is irreversible. It is now one closed section.

          The hint lists what is inside, because a closed section that only says
          "advanced" gives no reason to open it. */}
      <Section
        title="Advanced"
        hint="a separate model to settle votes, a Wikipedia reference shelf, and erasing this setup"
      >
        <Section title="Decisions" hint="Who settles it when the delegates disagree">
          {/* "classification, not deliberation" and "burning a turn per voter"
              described the implementation to nobody. What the user is choosing is
              which model tallies the votes, so that is what this now says. */}
          <p className="mb-2 text-[11px] leading-relaxed text-ink-3">
            When the delegates disagree on whether a point should stand, something
            has to settle it. You can let each delegate count its own votes, or point
            the counting at one separate model that answers for everyone. Votes
            always come with a reason either way.
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
            title="Which model settles a disagreement about whether a point stands"
          >
            <option value="seat">Each delegate decides for itself</option>
            <option value="jev">One separate model decides for everyone</option>
          </select>
          {config.decision.provider === "jev" && (
            <>
              <input
                className="field font-mono !text-[12px]"
                value={config.decision.baseUrl}
                placeholder="address of the deciding model"
                disabled={disabled}
                onChange={(e) =>
                  patch({ decision: { ...config.decision, baseUrl: e.target.value } })
                }
              />
              <input
                className="field"
                value={config.decision.model}
                placeholder="model that settles the votes"
                disabled={disabled}
                onChange={(e) =>
                  patch({ decision: { ...config.decision, model: e.target.value } })
                }
              />
              {served ? (
                <ServerKeyField
                  endpointId="decision"
                  baseUrl={config.decision.baseUrl}
                  disabled={disabled}
                />
              ) : (
                <input
                  className="field font-mono !text-[12px]"
                  type="password"
                  value={config.decision.apiKey ?? ""}
                  placeholder="api key — blank only if this server allows it"
                  disabled={disabled}
                  onChange={(e) =>
                    patch({
                      decision: { ...config.decision, apiKey: e.target.value || null },
                    })
                  }
                />
              )}
              <div className="flex items-center gap-2">
                <span className="shrink-0 text-[11px] text-ink-3">timeout</span>
                <Slider
                  label=""
                  value={config.decision.timeoutSecs}
                  min={STORED_BOUNDS.decisionTimeoutSecs.min}
                  max={STORED_BOUNDS.decisionTimeoutSecs.max}
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
                back to the delegate's own model and says so.
              </p>
            </>
          )}
        </Section>

        <div className="mt-3 border-t border-line-2 pt-3">
          {/* The reference shelf. Off by default and worded so nobody enables it
              expecting the open web: it reads Wikipedia and Wiktionary, which is
              what a term lookup actually needs, and nothing else. */}
          <div className="mt-3 border-t border-line-2 pt-3">
            <Check
              checked={config.allowWeb}
              disabled={disabled}
              onChange={(v) => patch({ allowWeb: v })}
              label="let the advocate, critic and rationale look terms up"
              title="Reads Wikipedia and Wiktionary. This is not a general web search — free search engines answer with a bot check these days."
            />
            {config.allowWeb && (
              <>
                <div className="mt-2 grid grid-cols-2 gap-3">
                  <Slider
                    label="pages per turn"
                    value={config.maxFetchesPerTurn}
                    min={1}
                    max={STORED_BOUNDS.maxFetchesPerTurn.max}
                    step={1}
                    disabled={disabled}
                    onChange={(v) => patch({ maxFetchesPerTurn: v })}
                  />
                  {/* Not "words": this is a character budget handed to the fetcher, so relabelling
                      it would be a small lie. Spelled out, and the readout grouped,
                      because a bare "4000" says nothing about what it measures. */}
                  <Slider
                    label="characters per page"
                    value={config.maxFetchChars}
                    min={STORED_BOUNDS.maxFetchChars.min}
                    max={STORED_BOUNDS.maxFetchChars.max}
                    step={500}
                    disabled={disabled}
                    onChange={(v) => patch({ maxFetchChars: v })}
                    format={(v) => (v >= 1000 ? `${Math.round(v / 1000)}k` : `${v}`)}
                    title="How much of each page to keep. Longer pages cost more to send to the model."
                  />
                </div>
                <p className="mt-1.5 text-[11px] leading-relaxed text-ink-3">
                  Reading happens inside the delegate&apos;s turn: it looks something up,
                  gets the text back, and argues from it in the same reply. Each page
                  it opens is shown in the transcript with a link, so you can check the
                  source. Retrieved text is handed to the model as reference material,
                  never as instructions.
                </p>
              </>
            )}
          </div>
        </div>
      </Section>
    </div>
  );
});

/** One model playing every delegate: pick it once, set it for the whole table. */
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
        One model, every delegate — the same mind arguing with itself.
      </div>
      <div className="flex items-center gap-1.5">
        <select
          className="field font-mono !text-[12px]"
          value={pick}
          disabled={disabled}
          onChange={(e) => setPick(e.target.value)}
          title="Every delegate stays on its own server — only the model name changes"
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
          // `pick` is state and `models` is a prop that moves under it: edit an
          // endpoint's URL, the debounced re-probe returns a list without the chosen
          // model, the select renders the blank placeholder — and "use for all" stayed
          // enabled, applying a model nobody can see.
          disabled={disabled || !pick || !models.includes(pick)}
          onClick={() => onApply(pick)}
          title="Set every delegate to this model, leaving each one's server alone"
        >
          use for all
        </button>
      </div>
    </div>
  );
}
