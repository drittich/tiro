<script lang="ts">
  import { tick } from "svelte";
  import { clock, screenshotUrl, trackLabel, type ScreenshotView, type Segment } from "./api";
  import Divider, { LINE } from "./Divider.svelte";
  import ScreenshotViewer from "./ScreenshotViewer.svelte";
  import { screenshotPanelHeight } from "./sizes.svelte";

  let {
    segments,
    follow = false,
    meetingId = "",
    screenshots = [],
    stacked = false,
    pending,
    reveal = null,
    revealMs = null,
    ondeleteshot,
  }: {
    segments: Segment[];
    follow?: boolean;
    meetingId?: string;
    screenshots?: ScreenshotView[];
    /** Each row's Track name and time above its text, for a narrow panel such as the Live view's. */
    stacked?: boolean;
    /** A working row after the last Segment, such as "Transcribing the next chunk…". */
    pending?: string;
    /** A Screenshot to scroll to, such as one picked in the Screenshots tab. */
    reveal?: ScreenshotView | null;
    /** A Meeting time in ms to scroll to, such as a search result's: the row of the Segment starting there. */
    revealMs?: number | null;
    /** Offered as "Delete" in the full-size viewer; see `ScreenshotViewer`'s `ondelete`. */
    ondeleteshot?: (shot: ScreenshotView) => Promise<ScreenshotView | null | undefined>;
  } = $props();

  const PANEL_KEY = "tiro.screenshotPanelCollapsed";
  const MIN_SHOT_HEIGHT = 64;
  /** Kept for the Transcript under the panel, however tall the Screenshot is made. */
  const MIN_TRANSCRIPT_HEIGHT = 120;
  /** How small a saved height may shrink when the window is too short for both it and the Transcript. */
  const SQUEEZED_SHOT_HEIGHT = 32;
  /** How long a row scrolled to from a search result stays marked. */
  const FOUND_MS = 2500;

  let scroller: HTMLElement | undefined = $state();
  let pinned = true;
  let rows: HTMLElement[] = [];
  /** The Screenshot in the panel: the latest one at or before the topmost row in view. */
  let current = $state(0);
  let collapsed = $state(loadCollapsed());
  let viewer: ScreenshotViewer | undefined = $state();
  let boxHeight = $state(0);
  let panelHeight = $state(0);
  let stageHeight = $state(0);

  type Row = { segment: Segment } | { shot: ScreenshotView; n: number };

  // Each Screenshot goes before the Segment at its `position` (placed by core, like the export).
  const list = $derived.by(() => {
    const out: Row[] = [];
    let n = 0;
    for (let i = 0; i <= segments.length; i++) {
      while (n < screenshots.length && screenshots[n].position <= i) out.push({ shot: screenshots[n], n: n++ });
      if (i < segments.length) out.push({ segment: segments[i] });
    }
    return out;
  });

  function loadCollapsed(): boolean {
    try {
      return localStorage.getItem(PANEL_KEY) === "1";
    } catch {
      return false;
    }
  }

  function toggle() {
    collapsed = !collapsed;
    try {
      localStorage.setItem(PANEL_KEY, collapsed ? "1" : "0");
    } catch {
      // Not remembered; the panel still works.
    }
  }

  /**
   * Where showing a Screenshot left the Transcript. Until the user scrolls away from it the panel keeps that
   * Screenshot, even when its row couldn't reach the top (too little Transcript below it) or new rows arrive.
   */
  let held: { top: number; atEnd: boolean } | null = null;

  function atEnd(el: HTMLElement): boolean {
    return el.scrollHeight - el.scrollTop - el.clientHeight < 1;
  }

  function hold() {
    if (scroller) held = { top: scroller.scrollTop, atEnd: atEnd(scroller) };
  }

  /** Whether the panel is keeping a shown Screenshot; lets go once the Transcript has moved from where it was left. */
  function holding(): boolean {
    if (held === null || !scroller) return false;
    if (Math.abs(scroller.scrollTop - held.top) < 1) return true;
    // Left at the end, a taller or shorter Transcript (the Screenshot resized, say) moves it without the user.
    if (held.atEnd && atEnd(scroller)) {
      held.top = scroller.scrollTop;
      return true;
    }
    held = null;
    return false;
  }

  let frame = 0;
  function onScroll() {
    if (!scroller || holding()) return;
    pinned = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 48;
    if (screenshots.length && !frame) frame = requestAnimationFrame(followScroll);
  }

  function followScroll() {
    frame = 0;
    if (!scroller || holding()) return;
    const top = scroller.scrollTop;
    let latest = 0;
    for (const [k, row] of list.entries()) {
      if ("shot" in row) latest = row.n;
      const el = rows[k];
      if (el && el.offsetTop + el.offsetHeight > top) break;
    }
    current = latest;
  }

  function view(shot: ScreenshotView) {
    viewer?.open(shot);
  }

  $effect(() => {
    void segments.length;
    void pending;
    if (follow && pinned) tick().then(() => scroller?.scrollTo({ top: scroller.scrollHeight }));
  });

  /** Puts Screenshot `n` in the panel and its row at the top of the Transcript, as near as it can scroll. */
  function show(n: number) {
    if (!scroller) return;
    current = Math.max(0, Math.min(n, screenshots.length - 1));
    pinned = false;
    // Held from now, so a scroll or reload before the Transcript moves doesn't put the panel back.
    hold();
    if (!scrolling) scrolling = requestAnimationFrame(scrollToCurrent);
  }

  // Once per frame, so dragging the slider through a long Meeting stays smooth.
  let scrolling = 0;
  function scrollToCurrent() {
    scrolling = 0;
    const k = list.findIndex((row) => "shot" in row && row.n === current);
    const el = rows[k];
    if (!scroller || !el) return;
    scroller.scrollTo({ top: el.offsetTop - parseFloat(getComputedStyle(scroller).paddingTop), behavior: "instant" });
    hold();
  }

  $effect(() => {
    void meetingId;
    held = null;
  });

  $effect(() => {
    void meetingId;
    void list;
    tick().then(followScroll);
  });

  // The Screenshot's row at the top of the Transcript, shown in the panel; once, not again when the Transcript reloads.
  let revealed: string | null = null;
  $effect(() => {
    const shot = reveal;
    const n = shot ? screenshots.findIndex((s) => s.file === shot.file) : -1;
    if (n < 0 || shot!.file === revealed) return;
    revealed = shot!.file;
    tick().then(() => show(n));
  });

  // Once per time, when the Segments have loaded; the row is marked for a moment so it can be found.
  let revealedMs: number | null = null;
  $effect(() => {
    const ms = revealMs;
    if (ms === null || ms === revealedMs || !segments.length) return;
    revealedMs = ms;
    tick().then(() => scrollToMs(ms));
  });

  function scrollToMs(ms: number) {
    let k = -1;
    for (const [i, row] of list.entries()) {
      if (!("segment" in row)) continue;
      const start = Math.round(row.segment.start * 1000);
      if (start === ms) {
        k = i;
        break;
      }
      if (start < ms) k = i;
    }
    const el = rows[k];
    if (!scroller || !el) return;
    pinned = false;
    held = null;
    const top = scroller.scrollTop + el.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
    scroller.scrollTo({ top: top - parseFloat(getComputedStyle(scroller).paddingTop), behavior: "instant" });
    el.classList.add("found");
    setTimeout(() => el.classList.remove("found"), FOUND_MS);
  }

  function onScrub(e: Event & { currentTarget: HTMLInputElement }) {
    show(e.currentTarget.valueAsNumber);
  }

  // A deleted last Screenshot leaves the panel on the one before it.
  $effect(() => {
    if (screenshots.length && current >= screenshots.length) current = screenshots.length - 1;
  });

  const inPanel = $derived(screenshots[current] ?? screenshots[0]);

  // The tallest Screenshot that leaves the bar above it and the Transcript below it room.
  const maxShotHeight = $derived(boxHeight - (panelHeight - stageHeight) - LINE - MIN_TRANSCRIPT_HEIGHT);
  // Clamped to the room there is now, without losing the saved height for a larger window. In a very short
  // window the Transcript keeps its room and the Screenshot goes below the smallest height it can be dragged to.
  const shotHeight = $derived.by(() => {
    const saved = screenshotPanelHeight.value;
    if (saved === null || !boxHeight) return saved;
    return Math.max(SQUEEZED_SHOT_HEIGHT, Math.min(saved, maxShotHeight));
  });
</script>

{#snippet transcript()}
  <div class="transcript" class:stacked bind:this={scroller} onscroll={onScroll}>
    {#each list as row, k (k)}
      {#if "shot" in row}
        <div class="segment shot" bind:this={rows[k]}>
          <span class="who"></span>
          <time data-nofind>{clock(row.shot.ms / 1000)}</time>
          <button class="thumb" onclick={() => view(row.shot)} aria-label="View screenshot at {clock(row.shot.ms / 1000)}">
            <img src={screenshotUrl(meetingId, row.shot)} alt="Screenshot at {clock(row.shot.ms / 1000)}" loading="lazy" />
          </button>
        </div>
      {:else}
        <div class="segment {row.segment.track}" bind:this={rows[k]}>
          <span class="who" data-nofind>{trackLabel(row.segment.track)}</span>
          <time data-nofind>{clock(row.segment.start)}</time>
          <p>{row.segment.text}</p>
        </div>
      {/if}
    {:else}
      <p class="empty" data-nofind>Nothing transcribed yet. The first lines appear about 30 seconds after recording starts.</p>
    {/each}
    {#if pending}
      <div class="pending" role="status" data-nofind>
        <span class="dots" aria-hidden="true"><span></span><span></span><span></span></span>{pending}
      </div>
    {/if}
  </div>
{/snippet}

{#if screenshots.length}
  <div class="with-screenshots" bind:clientHeight={boxHeight}>
    <div class="panel" class:collapsed bind:clientHeight={panelHeight}>
      <div class="bar">
        <span data-nofind>Screenshot {current + 1} of {screenshots.length} · {clock(inPanel.ms / 1000)}</span>
        <button class="link" onclick={toggle} aria-expanded={!collapsed}>{collapsed ? "Show" : "Hide"}</button>
      </div>
      {#if !collapsed}
        <div
          class="stage"
          bind:clientHeight={stageHeight}
          style:height={shotHeight === null ? undefined : `${shotHeight}px`}
          style:--shot-max={shotHeight === null ? undefined : `${shotHeight - 2}px` /* less .large's border */}
        >
          <button class="large" onclick={() => view(inPanel)} aria-label="View screenshot at {clock(inPanel.ms / 1000)}">
            <img src={screenshotUrl(meetingId, inPanel)} alt="Screenshot at {clock(inPanel.ms / 1000)}" />
          </button>
          <button class="step prev" onclick={() => show(current - 1)} disabled={current <= 0} aria-label="Previous screenshot">
            <svg width="18" height="18" viewBox="0 0 16 16" aria-hidden="true">
              <path d="M10 3L5 8l5 5" stroke="currentColor" stroke-width="1.6" fill="none" stroke-linecap="round" stroke-linejoin="round" />
            </svg>
          </button>
          <button
            class="step next"
            onclick={() => show(current + 1)}
            disabled={current >= screenshots.length - 1}
            aria-label="Next screenshot"
          >
            <svg width="18" height="18" viewBox="0 0 16 16" aria-hidden="true">
              <path d="M6 3l5 5-5 5" stroke="currentColor" stroke-width="1.6" fill="none" stroke-linecap="round" stroke-linejoin="round" />
            </svg>
          </button>
        </div>
        {#if screenshots.length > 1}
          <input
            class="scrub"
            type="range"
            min="0"
            max={screenshots.length - 1}
            step="1"
            value={current}
            oninput={onScrub}
            aria-label="Screenshot"
            aria-valuetext="Screenshot {current + 1} of {screenshots.length}, {clock(inPanel.ms / 1000)}"
          />
        {/if}
      {/if}
    </div>
    {#if !collapsed}
      <Divider
        orientation="horizontal"
        size={stageHeight}
        min={MIN_SHOT_HEIGHT}
        max={maxShotHeight}
        grow={1}
        label="Resize the Screenshot"
        onresize={(px) => screenshotPanelHeight.set(px)}
        onreset={() => screenshotPanelHeight.reset()}
      />
    {/if}
    {@render transcript()}
  </div>

  <ScreenshotViewer {meetingId} ondelete={ondeleteshot} bind:this={viewer} />
{:else}
  {@render transcript()}
{/if}

<style>
  .transcript {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 1.14rem;
    padding: 0.86rem 2.29rem 2.29rem;
  }
  .with-screenshots {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .with-screenshots .transcript {
    /* So rows' offsetTop is measured from the top of the scroller. */
    position: relative;
  }
  /* A mono time column, then the Track name in its colour above the text, as on the Meeting notes board. */
  .segment {
    flex: none;
    max-width: 54.29rem;
    display: grid;
    grid-template-columns: 4.57rem minmax(0, 1fr);
    gap: 0.14rem 0.86rem;
  }
  /* Set from script on the row a search result opened at. */
  .segment:global(.found) {
    border-radius: 8px;
    background: var(--accent-bg);
    box-shadow: 0 0 0 0.43rem var(--accent-bg);
  }
  .segment time {
    grid-column: 1;
    grid-row: 1 / span 2;
    padding-top: 0.21rem;
  }
  .segment .who {
    grid-column: 2;
    grid-row: 1;
  }
  .segment p,
  .segment .thumb {
    grid-column: 2;
    grid-row: 2;
  }
  .shot .who {
    display: none;
  }
  .shot .thumb {
    grid-row: 1 / span 2;
  }
  .who {
    font-weight: 600;
    font-size: 0.86rem;
  }
  .you .who {
    color: var(--you);
  }
  .others .who {
    color: var(--others);
  }
  time {
    color: var(--muted);
    font-family: var(--font-mono);
    font-size: 0.86rem;
    font-variant-numeric: tabular-nums;
  }
  p {
    margin: 0;
    color: var(--reading-fg);
    font-size: 1.07rem;
    line-height: 1.6;
  }
  .empty {
    color: var(--muted);
    padding: 1.71rem 0;
  }
  /* The Track name and time on one line, the text under them, as on the Recording board. */
  .stacked {
    display: flex;
    flex-direction: column;
    gap: 1.14rem;
    padding: 1.29rem 1.43rem;
  }
  .stacked .segment {
    max-width: none;
    grid-template-columns: auto 1fr;
    gap: 0.29rem 0.57rem;
  }
  .stacked .segment > * {
    grid-column: auto;
    grid-row: auto;
    padding-top: 0;
  }
  .stacked .segment p,
  .stacked .segment .thumb {
    grid-column: 1 / -1;
  }
  .stacked .who {
    font-size: 0.86rem;
  }
  .stacked time {
    font-size: 0.79rem;
  }
  .stacked .shot .who {
    display: none;
  }
  .stacked .segment p {
    font-size: 1rem;
    line-height: 1.55;
  }
  .stacked .empty {
    padding: 0;
  }
  .stacked .shot .thumb {
    grid-row: auto;
  }
  .pending {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.57rem;
    color: var(--muted);
    font-size: 0.86rem;
  }
  .shot {
    align-items: start;
  }
  .thumb,
  .large {
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 10px;
    overflow: hidden;
    background: none;
    line-height: 0;
    justify-self: start;
    max-width: 100%;
  }
  .thumb img {
    height: 72px;
    max-width: 100%;
    object-fit: contain;
  }
  .panel {
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 0.57rem;
    padding: 0.57rem 2.29rem 0.86rem;
  }
  /* Expanded, the Divider is the border. */
  .panel.collapsed {
    border-bottom: 1px solid var(--border);
  }
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    color: var(--muted);
    font-family: var(--font-mono);
    font-size: 0.79rem;
    font-variant-numeric: tabular-nums;
  }
  .bar .link {
    padding: 0;
    font-family: var(--text-font, "Inter"), "Inter", sans-serif;
    font-size: 0.86rem;
    font-weight: 600;
  }
  .stage {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 0;
  }
  /* Over the stage's edges, dark and ringed in white so they read on light and dark Screenshots alike. */
  .step,
  .step:hover:not(:disabled) {
    position: absolute;
    top: 50%;
    translate: 0 -50%;
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    padding: 0;
    border-radius: 50%;
    border: 1px solid rgb(255 255 255 / 0.6);
    background: rgb(0 0 0 / 0.55);
    color: #fff;
  }
  .step:hover:not(:disabled) {
    background: rgb(0 0 0 / 0.75);
  }
  .step:disabled {
    opacity: 0.25;
  }
  .prev {
    left: 4px;
  }
  .next {
    right: 4px;
  }
  .scrub {
    width: 100%;
    margin: 0;
    accent-color: var(--accent);
  }
  .large img {
    /* Until the user sets a height: leaves the Transcript most of the pane, down to the 400 px minimum window height. */
    max-height: var(--shot-max, clamp(64px, 24vh, 260px));
    max-width: 100%;
    object-fit: contain;
  }
  @media (max-width: 600px) {
    .transcript:not(.stacked) {
      padding-inline: 1.14rem;
    }
    .panel {
      padding-inline: 1.14rem;
    }
    .segment {
      grid-template-columns: 3.57rem minmax(0, 1fr);
      column-gap: 0.57rem;
    }
  }
</style>
