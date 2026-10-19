import { useEffect, useRef, useState } from "react";
import { Check } from "@phosphor-icons/react/dist/csr/Check";
import { Palette } from "@phosphor-icons/react/dist/csr/Palette";
import { THEMES, type ThemeId } from "../themes";

/**
 * Theme picker.
 *
 * One control for the whole app, replacing the binary light/dark toggle that
 * used to be duplicated in two headers. A popover rather than a cycling button
 * because eight themes cannot be guessed at from an icon, and rather than a
 * `<select>` because each entry carries a swatch worth seeing.
 *
 * Applies on click and closes; the actual repaint is the `data-theme` attribute
 * on the root, so there is no flash between choosing and seeing.
 */
export function ThemePicker({ value, onChange }: {
  value: ThemeId;
  onChange: (id: ThemeId) => void;
}) {
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLDivElement>(null);
  const current = THEMES.find((t) => t.id === value) ?? THEMES[0]!;

  // Dismiss on outside press or Escape — a popover with no way out is a trap.
  // `pointerdown` covers mouse and touch alike; `mousedown` never fires on a
  // tap that the browser turns into a scroll, so the picker stayed open on
  // phones until something else was tapped.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        (wrap.current?.querySelector("button") as HTMLButtonElement | null)?.focus();
      }
    };
    document.addEventListener("pointerdown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const listRef = useRef<HTMLDivElement>(null);
  /* Menu semantics, honestly.
   *
   * This advertised `role="menu"` with `menuitemradio` children and implemented
   * none of it: no Arrow/Home/End handling, no roving focus. So opening it with
   * Enter left focus on the trigger, arrow keys did nothing, and Tab walked
   * *past* the last theme into the header behind the still-open popover. That is
   * worse than no menu at all — the roles promised keyboard behaviour the app
   * did not have, and the popover's dismissal rules (outside press, Escape) are
   * things nobody knows to try unprompted.
   *
 * Two ways to make the promise true, and this takes the smaller one:
 * implement the keys. `Escape` and outside-press already worked; adding arrows
 * and Home/End is a few lines, and keeps the roles that correctly describe
 * "one choice out of eight, arrow through them" — which a listbox of independent
 * buttons does not.
   */
  const move = (from: number, delta: number | "home" | "end") => {
    const items = Array.from(
      listRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitemradio"]') ?? [],
    );
    if (items.length === 0) return;
    const next =
      delta === "home" ? 0 : delta === "end" ? items.length - 1 : (from + delta + items.length) % items.length;
    items[next]?.focus();
  };

  return (
    <div ref={wrap} className="relative">
      <button
        type="button"
        className="btn btn-ghost !w-8 !px-0"
        onClick={() => setOpen((o) => !o)}
        title={`theme: ${current.label}`}
        aria-label={`theme: ${current.label}. Change theme`}
        aria-haspopup="menu"
        aria-expanded={open}
        onKeyDown={(e) => {
          // Opening by keyboard should land on a theme, not on the trigger with
          // a menu apparently open. Enter/Space already toggles; the arrow keys
          // open it the way a menu is expected to.
          if (!open && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
            e.preventDefault();
            const down = e.key === "ArrowDown";
            setOpen(true);
            // After the popover mounts, on the matching edge — Down opens at the
            // top like every other menu, Up at the bottom.
            queueMicrotask(() => {
              const items = listRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitemradio"]');
              if (!items || items.length === 0) return;
              (down ? items[0] : items[items.length - 1])?.focus();
            });
          }
        }}
      >
        <Palette size={14} />
      </button>

      {open && (
        <div
          ref={listRef}
          /* `max-h` + scroll: eight themes with two lines each is ~370px, which
             runs off the bottom of a short window and off the top of a phone's. */
          className="card rise absolute top-full right-0 z-50 mt-1 max-h-[min(60dvh,22rem)] w-64 overflow-y-auto p-1.5"
          style={{ boxShadow: "var(--shadow-pop)" }}
          role="menu"
          aria-label="choose a theme"
          onKeyDown={(e) => {
            const items = Array.from(
              listRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitemradio"]') ?? [],
            );
            const here = items.indexOf(document.activeElement as HTMLButtonElement);
            if (e.key === "ArrowDown") {
              e.preventDefault();
              move(here < 0 ? 0 : here, 1);
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              move(here < 0 ? 0 : here, -1);
            } else if (e.key === "Home") {
              e.preventDefault();
              move(here, "home");
            } else if (e.key === "End") {
              e.preventDefault();
              move(here, "end");
            } else if (e.key === "Tab") {
              // Tabbing out of a menu should close it. Otherwise it stays open
              // behind whatever the user just moved to, and its only remaining
              // escape routes are an outside press or Escape — both undiscoverable.
              setOpen(false);
            }
          }}
        >
          {THEMES.map((t) => {
            const on = t.id === value;
            return (
              <button
                key={t.id}
                type="button"
                role="menuitemradio"
                aria-checked={on}
                // Focus moved here must not scroll the popover away, and the
                // default menu behaviour (activating on Space) is not wanted:
                // choosing a theme closes the popover, which is what Enter does.
                onClick={() => {
                  onChange(t.id);
                  setOpen(false);
                  (wrap.current?.querySelector("button") as HTMLButtonElement | null)?.focus();
                }}
                className={`flex w-full items-start gap-2 rounded-lg px-2 py-1.5 text-left transition-colors hover:bg-surface-3 ${
                  on ? "bg-surface-3" : ""
                }`}
              >
                {/* The swatch: canvas, surface and primary, as three bars. */}
                <span
                  aria-hidden
                  className="mt-0.5 flex shrink-0 overflow-hidden rounded border border-line-2"
                >
                  {t.swatch.map((c) => (
                    <span key={c} className="h-4 w-2" style={{ background: c }} />
                  ))}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-1.5 text-[12.5px] font-medium text-ink">
                    {t.label}
                    {on && <Check size={12} weight="bold" className="text-primary" />}
                  </span>
                  <span className="block text-[11px] leading-snug text-ink-3">{t.note}</span>
                </span>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}