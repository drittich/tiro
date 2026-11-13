import type { Appearance } from "./api";

/** Font families shipped with Tiro (app.css), offered in the font pickers whether or not they are installed. */
export const BUNDLED_FONTS = ["Inter", "JetBrains Mono", "Source Serif 4"];

/** A font family name as a CSS string, so names with spaces or digits work. */
const family = (name: string) => `"${name.replace(/["\\]/g, "")}"`;

/** Sets the CSS variables app.css and the Notes editor read their fonts from, and the theme app.css colours follow. */
export function applyAppearance(appearance: Appearance) {
  const root = document.documentElement;
  root.style.setProperty("--text-font", family(appearance.textFont));
  root.style.setProperty("--text-size", `${appearance.textSize}px`);
  root.style.setProperty("--notes-font", family(appearance.notesFont));
  root.style.setProperty("--notes-size", `${appearance.notesSize}px`);
  if (appearance.theme === "system") delete root.dataset.theme;
  else root.dataset.theme = appearance.theme;
}
