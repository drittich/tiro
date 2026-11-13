/** A size in px the user set by dragging a Divider, remembered in localStorage for every Meeting. */
class StoredSize {
  /** null: the default layout. */
  value = $state<number | null>(null);

  constructor(private key: string) {
    try {
      const saved = Number.parseFloat(localStorage.getItem(key) ?? "");
      this.value = Number.isFinite(saved) && saved > 0 ? saved : null;
    } catch {
      this.value = null;
    }
  }

  set(px: number) {
    this.value = Math.round(px);
    try {
      localStorage.setItem(this.key, String(this.value));
    } catch {
      // Not remembered; the size still applies until the app closes.
    }
  }

  reset() {
    this.value = null;
    try {
      localStorage.removeItem(this.key);
    } catch {
      // Nothing saved to forget.
    }
  }
}

/** The Notes pane beside the Transcript or Summary, shared by the Live view and Meeting detail. */
export const notesWidth = new StoredSize("tiro.notesWidth");
/** The Notes pane under the Transcript or Summary, on narrow windows. */
export const notesHeight = new StoredSize("tiro.notesHeight");
/** The Screenshot in the panel above a Meeting's Transcript. */
export const screenshotPanelHeight = new StoredSize("tiro.screenshotPanelHeight");
