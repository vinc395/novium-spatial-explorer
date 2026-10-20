// A panel section whose heading folds it away. The open/closed state is remembered
// per browser (localStorage, best effort) and can also be set from elsewhere (e.g. a
// cell click folds the legend) through the store.

import { useState, type ReactNode } from "react";
import { setFolds, useStore } from "../store";

function remembered(id: string, fallback: boolean): boolean {
  try {
    const v = localStorage.getItem(`novium:open:${id}`);
    return v === null ? fallback : v === "1";
  } catch {
    return fallback;
  }
}

export function Collapsible({ id, title, children, defaultOpen = true }: { id: string; title: ReactNode; children: ReactNode; defaultOpen?: boolean }) {
  const set = useStore((s) => s.folds[id]);
  const [initial] = useState(() => remembered(id, defaultOpen));
  const open = set ?? initial;
  return (
    <section className={`collapsible${open ? "" : " closed"}`}>
      <h3>
        <button type="button" className="collapse-head" aria-expanded={open} onClick={() => setFolds({ [id]: !open })}>
          <span className="collapse-caret" aria-hidden="true">
            {open ? "▾" : "▸"}
          </span>
          <span>{title}</span>
        </button>
      </h3>
      {open && children}
    </section>
  );
}
