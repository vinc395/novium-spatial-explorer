// Light / dark theme. "system" (the default) follows the operating system; an explicit
// choice is stored per browser and set as <html data-theme="light|dark">, which the
// stylesheet's tokens and the map's colours both follow.

export type ThemeChoice = "system" | "light" | "dark";
const KEY = "novium:theme";

export function loadTheme(): ThemeChoice {
  try {
    const t = localStorage.getItem(KEY);
    return t === "light" || t === "dark" ? t : "system";
  } catch {
    return "system";
  }
}

export function applyTheme(t: ThemeChoice) {
  const el = document.documentElement;
  if (t === "system") delete el.dataset.theme;
  else el.dataset.theme = t;
  try {
    if (t === "system") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, t);
  } catch {
    /* not remembered */
  }
}

export const systemDarkQuery = () =>
  typeof window !== "undefined" && window.matchMedia ? window.matchMedia("(prefers-color-scheme: dark)") : undefined;
