<script lang="ts" module>
  /** The width of the Divider's line, in px: the panes share what's left. */
  export const LINE = 1;
</script>

<script lang="ts">
  /**
   * A line between two panes that resizes one of them: drag it, or focus it and use the arrow keys.
   * Double-click returns the pane to its default size.
   */
  let {
    orientation,
    size,
    min,
    max,
    grow,
    label,
    onresize,
    onreset,
  }: {
    /** "vertical" sits between side-by-side panes and resizes a width; "horizontal" a height. */
    orientation: "vertical" | "horizontal";
    /** The pane's size now, in px. */
    size: number;
    min: number;
    max: number;
    /** 1 when moving the Divider right or down makes the pane bigger, -1 when it makes it smaller. */
    grow: 1 | -1;
    label: string;
    onresize: (px: number) => void;
    onreset: () => void;
  } = $props();

  const STEP = 16;

  let drag: { from: number; size: number } | null = null;

  const vertical = $derived(orientation === "vertical");

  // The smallest size wins when the window is too small for both limits.
  function limit(px: number): number {
    return Math.max(min, Math.min(px, max));
  }

  function at(e: PointerEvent): number {
    return vertical ? e.clientX : e.clientY;
  }

  function onpointerdown(e: PointerEvent) {
    if (e.button !== 0) return;
    // Stops text selection; the Divider is focused by hand instead.
    e.preventDefault();
    const el = e.currentTarget as HTMLElement;
    el.focus();
    el.setPointerCapture(e.pointerId);
    drag = { from: at(e), size };
    document.documentElement.dataset.resizing = vertical ? "col" : "row";
  }

  function onpointermove(e: PointerEvent) {
    if (!drag) return;
    const next = limit(drag.size + grow * (at(e) - drag.from));
    if (Math.round(next) !== Math.round(size)) onresize(next);
  }

  function end() {
    drag = null;
    delete document.documentElement.dataset.resizing;
  }

  function onkeydown(e: KeyboardEvent) {
    const keys = vertical ? ["ArrowLeft", "ArrowRight"] : ["ArrowUp", "ArrowDown"];
    const k = keys.indexOf(e.key);
    if (k < 0) return;
    e.preventDefault();
    onresize(limit(size + grow * (k === 0 ? -STEP : STEP)));
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions: a focusable separator is a widget (WAI-ARIA window splitter). -->
<div
  class="divider {orientation}"
  role="separator"
  tabindex="0"
  aria-orientation={orientation}
  aria-label={label}
  aria-valuenow={Math.round(size)}
  aria-valuemin={min}
  aria-valuemax={Math.max(min, Math.round(max))}
  {onpointerdown}
  {onpointermove}
  onpointerup={end}
  onpointercancel={end}
  onlostpointercapture={end}
  ondblclick={onreset}
  {onkeydown}
></div>

<style>
  .divider {
    flex: none;
    position: relative;
    z-index: 1;
    background: var(--border);
    touch-action: none;
    outline: none;
  }
  /* A wider grab area than the 1 px line. */
  .divider::before {
    content: "";
    position: absolute;
  }
  .vertical {
    width: 1px; /* LINE */
    cursor: col-resize;
  }
  .vertical::before {
    inset: 0 -3px;
  }
  .horizontal {
    height: 1px; /* LINE */
    cursor: row-resize;
  }
  .horizontal::before {
    inset: -3px 0;
  }
  .divider:hover,
  .divider:focus-visible {
    background: var(--accent);
  }
  .divider:focus-visible::before {
    background: color-mix(in srgb, var(--accent) 35%, transparent);
  }
</style>
