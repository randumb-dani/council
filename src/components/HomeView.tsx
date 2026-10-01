import { useEffect, useMemo, useState } from "react";
import {
  ArrowRight,
  CalendarBlank,
  Scales,
  SpinnerGap,
  Trash,
} from "@phosphor-icons/react";
import {
  deleteDebate,
  listDebates,
  save,
  slug,
  type SavedDebate,
} from "../debates";

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
  theme,
  onToggleTheme,
}: {
  onConvene: () => void;
  onOpen: (id: string) => void;
  theme: "dark" | "light";
  onToggleTheme: () => void;
}) {
  const [debates, setDebates] = useState<SavedDebate[] | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
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

  return (
    <div className="flex h-full flex-col overflow-y-auto">
      <header className="flex h-[52px] shrink-0 items-center gap-4 border-b border-line bg-surface px-4">
        <div className="flex items-center gap-2 text-[11px] font-bold tracking-[0.16em] text-ink">
          <span className="flex size-5 items-center justify-center rounded-md bg-primary text-on-primary">
            <Scales size={13} weight="bold" />
          </span>
          COUNCIL
        </div>
        <div className="flex-1" />
        <button
          type="button"
          className="btn btn-ghost !w-8 !px-0"
          onClick={onToggleTheme}
          title={theme === "dark" ? "switch to light" : "switch to dark"}
          aria-label="toggle theme"
        >
          {theme === "dark" ? <MoonIcon /> : <SunIcon />}
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
                title="no verdict was reached — the debate was still running or was stopped"
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

function MoonIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor" aria-hidden>
      <path d="M6 1a.75.75 0 0 1 .26 1.46 5.5 5.5 0 0 0 5.28 6.86.75.75 0 0 1 1.02.9 7 7 0 1 1-7.5-7.5.75.75 0 0 1 .94-1.72Z" />
    </svg>
  );
}
function SunIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor" aria-hidden>
      <path d="M8 4a4 4 0 1 0 0 8 4 4 0 0 0 0-8Zm0 1.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5ZM7 0h2v2.5H7V0Zm0 13.5h2V16H7v-2.5ZM0 7h2.5v2H0V7Zm13.5 0H16v2h-2.5V7Z" />
    </svg>
  );
}