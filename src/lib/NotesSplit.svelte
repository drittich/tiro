<script lang="ts">
  import type { Snippet } from "svelte";
  import { MediaQuery } from "svelte/reactivity";
  import Divider, { LINE } from "./Divider.svelte";
  import { notesHeight, notesWidth } from "./sizes.svelte";

  /** The Transcript (or Summary) beside the Notes, stacked on narrow windows: Live view and Meeting detail. */
  let { main, notes }: { main: Snippet; notes?: Snippet } = $props();

  const MIN_NOTES_WIDTH = 240;
  const MIN_MAIN_WIDTH = 280;
  // Stacked at the 400 px minimum window height, the Transcript (or Summary) side needs room for its tabs and
  // the Screenshot panel as well, so the Notes give way first.
  const MIN_NOTES_HEIGHT = 96;
  const MIN_MAIN_HEIGHT = 160;

  // Matches the breakpoint below.
  const stacked = new MediaQuery("(max-width: 860px)");

  let splitWidth = $state(0);
  let splitHeight = $state(0);
  /** The Notes pane's rendered size. */
  let paneWidth = $state(0);
  let paneHeight = $state(0);

  const stored = $derived(stacked.current ? notesHeight : notesWidth);
  const min = $derived(stacked.current ? MIN_NOTES_HEIGHT : MIN_NOTES_WIDTH);
  const minMain = $derived(stacked.current ? MIN_MAIN_HEIGHT : MIN_MAIN_WIDTH);
  const max = $derived((stacked.current ? splitHeight : splitWidth) - minMain - LINE);
  // Clamped in CSS so a smaller window shrinks the Notes without losing the saved size.
  const basis = $derived(
    stored.value === null ? undefined : `0 0 clamp(${min}px, ${stored.value}px, calc(100% - ${minMain + LINE}px))`,
  );
</script>

<div class="split" bind:clientWidth={splitWidth} bind:clientHeight={splitHeight}>
  <div class="pane">
    {@render main()}
  </div>
  {#if notes}
    <Divider
      orientation={stacked.current ? "horizontal" : "vertical"}
      size={stacked.current ? paneHeight : paneWidth}
      {min}
      {max}
      grow={-1}
      label="Resize the Notes"
      onresize={(px) => stored.set(px)}
      onreset={() => stored.reset()}
    />
    <div class="pane notes" style:flex={basis} bind:clientWidth={paneWidth} bind:clientHeight={paneHeight}>
      {@render notes()}
    </div>
  {/if}
</div>

<style>
  .split {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .pane {
    flex: 3 1 0;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .pane.notes {
    flex: 2 1 0;
  }
  @media (max-width: 860px) {
    .split {
      flex-direction: column;
    }
  }
</style>
