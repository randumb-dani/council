import { useEffect, useMemo, useState } from "react";
import { ArrowRight } from "@phosphor-icons/react/dist/csr/ArrowRight";
import { CalendarBlank } from "@phosphor-icons/react/dist/csr/CalendarBlank";
import { SpinnerGap } from "@phosphor-icons/react/dist/csr/SpinnerGap";
import { Trash } from "@phosphor-icons/react/dist/csr/Trash";
import { User } from "@phosphor-icons/react/dist/csr/User";
import {
  deleteDebate,
  listDebates,
  save,
  slug,
  type SavedDebate,
} from "../debates";
import { ThemePicker } from "./ThemePicker";
import { Wordmark } from "./Wordmark";
import { debateState } from "../bridge";
import type { ThemeId } from "../themes";
function when(ms: number): string {
  const d = new Date(ms);
  const today = new Date();
  const sameDay =
    d.getFullYear() === today.getFullYear() &&
    d.getMonth() === today.getMonth() &&
    d.getDate() === today.getDate();
  return sameDay
    ? `today ${d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}`
    : d.toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}
/**
 * The front door: what has been said, and the one button that starts something.
 *
 * Nothing is auto-opened. Restoring the last debate on launch sounds helpful
 * until the morning you mean to start a fresh one and find yesterday's council
 * already on screen; a library that lists is a library you can choose from.
 */
export function HomeView({
  onConvene,
  onOpen,
  onReturn,
  onTakeSeat,
  theme,
  onTheme,
}: {
  onConvene: () => void;
  onOpen: (id: string) => void;
  /** Back to the chamber while a council is in session. */
  onReturn: () => void;
  /** Into the council where the operator argues a seat themselves. */
  onTakeSeat: () => void;
  theme: ThemeId;
  onTheme: (id: ThemeId) => void;
}) {
  const [debates, setDebates] = useState<SavedDebate[] | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  // A council outlives the chamber view: the engine keeps running while the
  // library is on screen, and remounting the chamber re-adopts it from the
  // snapshot. Without this card the only way back was knowing the `#/new`
  // hash, while the convene button invited starting a second debate on top.
  const [live, setLive] = useState<{ motion: string; round: number } | null>(null);
  useEffect(() => {
    let cancelled = false;
    const check = () => {
      debateState()
        .then((snap) => {
          if (cancelled) return;
          setLive(
            snap && !snap.finished && (snap.points.length || snap.speeches.length)
              ? { motion: snap.motion, round: snap.round }
              : null,
          );
        })
        .catch(() => {
          if (!cancelled) setLive(null);
        });
    };
    check();
    // Debates save as they run, so a council convened after this mount shows
    // up without a reload.
    window.addEventListener("council:library", check);
    return () => {
      cancelled = true;
      window.removeEventListener("council:library", check);
    };
  }, []);
  // A failed export used to be indistinguishable from a dead button, which is
  // the one thing you cannot afford on the action that gets a debate off the
  // machine.
  const [exportError, setExportError] = useState<string | null>(null);
  // Read on mount and whenever the window says the store changed, so returning
  // from a debate shows it in the list rather than only after a reload.
  useEffect(() => {
    const read = () => setDebates(listDebates());
    read();
    window.addEventListener("council:library", read);
    window.addEventListener("storage", read);
    return () => {
      window.removeEventListener("council:library", read);
      window.removeEventListener("storage", read);
    };
  }, []);
  const saved = useMemo(() => debates ?? [], [debates]);
  // Constant 20, mirroring MAX_KEPT=20 in debates.ts. `libraryCapacity().max`
  // parses the whole library just to return that constant, so call it never;
  // update this if MAX_KEPT changes (libraryCapacity max is the source of truth).
  const libraryMax = 20;
  return (
    <div className="flex h-full flex-col overflow-y-auto">
      {/* Wraps, for the same reason the chamber header does.
            This row came to ~500px — wordmark, theme picker, and two calls to
            action with long labels — inside `overflow-y-auto` under a `body` with
            `overflow: hidden`. At 430px the overflow was clipped rather than
            scrolled, so the arrow and the tail of "convene a council" were simply
            gone. The front door's only two calls to action, cut off by the
            width of the window. */}
      <header className="flex min-h-[52px] shrink-0 flex-wrap items-center gap-x-4 gap-y-1 border-b border-line bg-surface px-4 py-1">
        <Wordmark />
        <div className="flex-1" />
        <ThemePicker value={theme} onChange={onTheme} />
        {/* Two councils, two buttons: the automatic one, and the one where you
            argue a seat yourself. They are different debates, not different
            settings — the engine waits for you in one and never in the other. */}
        <button
          type="button"
          className="btn"
          onClick={onTakeSeat}
          title="Argue one of the debating roles yourself — the council waits for you"
        >
          <User size={14} /> take a seat
        </button>
        <button type="button" className="btn btn-primary" onClick={onConvene}>
          convene a council <ArrowRight size={14} />
        </button>
      </header>
      <main className="mx-auto w-full max-w-3xl px-5 py-8">
        <h1 className="text-[19px] font-semibold tracking-tight text-ink">The library</h1>
        <p className="mt-1 text-[12.5px] leading-relaxed text-ink-3">
          Every debate is kept here as it runs, so nothing is lost when the window
          closes. Open one to read it, or take it away as Markdown or JSON.
        </p>
        {/* The trim, stated.
            The library sheds its oldest debate past twenty, silently: no notice,
            no count, nothing on the card. An operator accumulating debates over a
            week lost the oldest one with no signal and no way of knowing it had
            ever been there — and this module's whole reason for existing is
            against exactly that. Reaching the cap is ordinary, so this is a quiet
            line rather than a warning; but it is there. */}
        {saved.length > 0 && (
          <p className="mt-2 text-[11.5px] leading-relaxed text-ink-3">
            Keeping the {saved.length} newest, up to {libraryMax}. Anything older is
            dropped — take what you want to keep as Markdown or JSON.
          </p>
        )}
        {live && (
          <button
            type="button"
            onClick={onReturn}
            className="rise mt-4 flex w-full items-center gap-3 rounded-xl border border-primary/50 bg-primary-soft px-4 py-3 text-left"
            title="Back to the council currently in session"
          >
            <span className="size-2 shrink-0 rounded-full bg-primary live-dot" aria-hidden />
            <span className="min-w-0 flex-1">
              <span className="block text-[12px] font-semibold text-ink">
                A council is in session — return to it
              </span>
              <span className="block truncate text-[12px] text-ink-2">
                round {live.round} · {live.motion || "no motion set"}
              </span>
            </span>
            <ArrowRight size={14} className="shrink-0 text-ink-2" />
          </button>
        )}
        {debates === null ? (
          <div className="mt-10 flex items-center justify-center gap-2 text-ink-3">
            <SpinnerGap size={15} className="animate-spin" /> reading the library…
          </div>
        ) : saved.length === 0 ? (
          <div className="mt-10 rounded-xl border border-dashed border-line-2 px-6 py-12 text-center">
            <p className="text-[13px] font-medium text-ink-2">No debates yet.</p>
            <p className="mx-auto mt-1 max-w-[34ch] text-[12px] leading-relaxed text-ink-3">
              Convene one and it will appear here the moment it starts, unfinished
              or not.
            </p>
            <button type="button" className="btn btn-primary mt-4" onClick={onConvene}>
              convene the first one <ArrowRight size={14} />
            </button>
          </div>
        ) : (
          <>
            {exportError && (
              <p
                className="mt-3 rounded-lg border border-bad/60 bg-bad/5 px-3 py-2 text-[12px] text-bad"
                role="alert"
              >
                {exportError}
              </p>
            )}
          <ul className="mt-6 flex flex-col gap-2.5">
            {saved.map((d) => (
              <DebateCard
                key={d.id}
                debate={d}
                onOpen={() => onOpen(d.id)}
                onDelete={() => {
                  deleteDebate(d.id);
                  setDebates(listDebates());
                }}
                confirming={confirming === d.id}
                onConfirm={() => setConfirming(d.id)}
                onCancelConfirm={() => setConfirming(null)}
                onExported={(why) => setExportError(why)}
              />
            ))}
          </ul>
          </>
        )}
      </main>
    </div>
  );
}
function DebateCard({
  onExported,
  debate,
  onOpen,
  onDelete,
  confirming,
  onConfirm,
  onCancelConfirm,
}: {
  debate: SavedDebate;
  onOpen: () => void;
  onDelete: () => void;
  confirming: boolean;
  onConfirm: () => void;
  onCancelConfirm: () => void;
  /** Reports why an export failed, or null when it did not. */
  onExported: (why: string | null) => void;
}) {
  const s = debate.summary;
  const exportAs = async (format: "md" | "json", ext: "md" | "json", mime: string) => {
    const why = await save(
      `${slug(s.motion)}-${debate.id}.${ext}`,
      debate,
      format,
      mime,
    );
    onExported(why);
  };
  return (
    <li className="card rise p-3.5">
      <div className="flex items-start gap-3">
        <button
          type="button"
          onClick={onOpen}
          className="min-w-0 flex-1 cursor-pointer text-left"
        >
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span className="text-[13.5px] leading-snug font-semibold tracking-tight text-ink">
              {s.motion || "untitled motion"}
            </span>
            {s.unfinished && (
              <span
                className="status-pill ml-auto bg-primary-soft text-primary"
                title="This debate never reached an answer — it was still running when you left, or you stopped it"
              >
                unfinished
              </span>
            )}
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px] text-ink-3">
            <span className="inline-flex items-center gap-1">
              <CalendarBlank size={11} /> {when(debate.savedAt)}
            </span>
            <span>
              {s.roundsConcluded || s.rounds} round
              {Math.max(s.roundsConcluded, s.rounds) === 1 ? "" : "s"}
            </span>
            {s.points > 0 && (
              <span>
                <span className="text-ok tabular-nums">{s.carried}</span> carried ·{" "}
                <span className="text-bad tabular-nums">{s.fell}</span> fell
                {s.stalemate > 0 && (
                  <>
                    {" · "}
                    <span className="text-warn tabular-nums">{s.stalemate}</span> tied
                  </>
                )}
              </span>
            )}
            <span className="text-ink-3">{s.speeches} speeches</span>
          </div>
          {s.answer && (
            <p className="mt-1.5 line-clamp-2 border-l-2 border-line-2 pl-2 text-[12px] leading-relaxed text-ink-2">
              {s.answer}
            </p>
          )}
        </button>
        <div className="flex shrink-0 flex-col items-end gap-1.5">
          <button type="button" className="btn" onClick={onOpen}>
            read
          </button>
          <div className="flex gap-1">
            <button
              type="button"
              className="btn btn-ghost !px-2 text-[11px]"
              title="download the readable record"
              onClick={() => void exportAs("md", "md", "text/markdown")}
            >
              md
            </button>
            <button
              type="button"
              className="btn btn-ghost !px-2 text-[11px]"
              title="download everything, as JSON"
              onClick={() => void exportAs("json", "json", "application/json")}
            >
              json
            </button>
            {confirming ? (
              <button
                type="button"
                className="btn btn-danger !px-2 text-[11px]"
                title="click again to delete for good"
                onClick={onDelete}
              >
                sure?
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-ghost !px-2"
                title="delete this debate"
                onClick={onConfirm}
              >
                <Trash size={12} />
              </button>
            )}
          </div>
          {confirming && (
            <button
              type="button"
              className="text-[11px] text-ink-3 underline underline-offset-2"
              onClick={onCancelConfirm}
            >
              keep it
            </button>
          )}
        </div>
      </div>
    </li>
  );
}