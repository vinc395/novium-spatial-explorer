// A compact picker for long, grouped option lists (hundreds of LR fields): a button
// showing the current choice opens a panel with collapsible groups and a filter.
// The group holding the current choice starts open; typing opens every group that
// has matches.

import { useEffect, useMemo, useRef, useState } from "react";

export interface PickerOption {
  value: string;
  label: string;
}

export interface PickerGroup {
  key: string;
  label: string;
  options: PickerOption[];
}

interface Props {
  /** accessible name of the trigger and the list, e.g. "Colour cells by" */
  label: string;
  value: string;
  /** label of the empty choice, e.g. "— single colour —" */
  none: string;
  groups: PickerGroup[];
  onChange: (value: string) => void;
}

export function SourcePicker({ label, value, none, groups, onChange }: Props) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const root = useRef<HTMLDivElement>(null);
  const total = groups.reduce((n, g) => n + g.options.length, 0);
  const current = groups.flatMap((g) => g.options).find((o) => o.value === value);
  const currentGroup = groups.find((g) => g.options.some((o) => o.value === value))?.key;

  // Every word of the query must occur in an option's label or value.
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const shown = useMemo(
    () =>
      groups
        .map((g) => ({
          ...g,
          options: words.length
            ? g.options.filter((o) => words.every((w) => o.label.toLowerCase().includes(w) || o.value.toLowerCase().includes(w)))
            : g.options,
        }))
        .filter((g) => g.options.length > 0),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [groups, query],
  );
  const matches = shown.reduce((n, g) => n + g.options.length, 0);

  const openPanel = () => {
    setExpanded(new Set(currentGroup ? [currentGroup] : []));
    setQuery("");
    setOpen(true);
  };
  const choose = (v: string) => {
    onChange(v);
    setOpen(false);
  };

  // Close on a click outside or Escape.
  useEffect(() => {
    if (!open) return;
    const down = (e: MouseEvent) => {
      if (root.current && !root.current.contains(e.target as Node)) setOpen(false);
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", down);
    document.addEventListener("keydown", key);
    return () => {
      document.removeEventListener("mousedown", down);
      document.removeEventListener("keydown", key);
    };
  }, [open]);

  const toggle = (k: string) =>
    setExpanded((s) => {
      const n = new Set(s);
      if (n.has(k)) n.delete(k);
      else n.add(k);
      return n;
    });

  return (
    <div className="picker" ref={root}>
      <button
        type="button"
        className="picker-trigger"
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        title={current?.label ?? none}
        onClick={() => (open ? setOpen(false) : openPanel())}
      >
        <span className="picker-value">{value ? (current?.label ?? value) : none}</span>
        <span className="picker-caret" aria-hidden="true">
          ▾
        </span>
      </button>
      {open && (
        <div className="picker-panel" role="listbox" aria-label={label}>
          {total > 12 && (
            <input
              type="search"
              autoFocus
              placeholder={`Filter ${total} options…`}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                const first = shown[0]?.options[0];
                if (e.key === "Enter" && first) choose(first.value);
              }}
              aria-label={`Filter ${label}`}
            />
          )}
          {words.length > 0 && (
            <p className="muted small picker-count">
              {matches} of {total} match
            </p>
          )}
          <button type="button" role="option" aria-selected={value === ""} data-source="" className="picker-option" onClick={() => choose("")}>
            {none}
          </button>
          {shown.map((g) => {
            const isOpen = words.length > 0 || expanded.has(g.key);
            return (
              <div key={g.key} role="group" aria-label={g.label} className="picker-group">
                <button type="button" className="picker-group-head" aria-expanded={isOpen} onClick={() => toggle(g.key)} disabled={words.length > 0}>
                  <span aria-hidden="true">{isOpen ? "▾" : "▸"}</span>
                  <span className="name">{g.label}</span>
                  <span className="muted">{g.options.length}</span>
                </button>
                {isOpen &&
                  g.options.map((o) => (
                    <button
                      type="button"
                      key={o.value}
                      role="option"
                      aria-selected={o.value === value}
                      data-source={o.value}
                      className="picker-option"
                      onClick={() => choose(o.value)}
                    >
                      {o.label}
                    </button>
                  ))}
              </div>
            );
          })}
          {words.length > 0 && matches === 0 && <p className="muted small">No option matches “{query}”.</p>}
        </div>
      )}
    </div>
  );
}
