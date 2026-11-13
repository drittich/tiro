<script lang="ts">
  import type { FindOptions } from "./find";

  /** Find in a pane (Ctrl+F): the query and options, and which match is current. The pane finds and highlights. */
  let {
    query = $bindable(""),
    options = $bindable({ caseSensitive: false, wholeWord: false }),
    current = $bindable(0),
    count,
    onclose,
  }: {
    query?: string;
    options?: FindOptions;
    /** The current match's index; Enter and the arrows step through the matches, wrapping round. */
    current?: number;
    /** How many matches there are. */
    count: number;
    /** Escape, or the close button. */
    onclose?: () => void;
  } = $props();

  let input: HTMLInputElement | undefined = $state();

  /** Puts the cursor in the query, selected so typing replaces it. */
  export function focus() {
    input?.focus();
    input?.select();
  }

  function step(by: number) {
    if (count) current = (current + by + count) % count;
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose?.();
    } else if (e.key === "Enter" && e.target === input) {
      e.preventDefault();
      step(e.shiftKey ? -1 : 1);
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions: Escape closes Find from any of its controls. -->
<div class="find" role="search" aria-label="Find" {onkeydown}>
  <input
    aria-label="Find"
    placeholder="Find"
    spellcheck="false"
    class:none={!!query && !count}
    bind:this={input}
    bind:value={query}
  />
  <span class="count" aria-live="polite">
    {#if query}{count ? `${current + 1} of ${count}` : "No matches"}{/if}
  </span>
  <button class="icon" onclick={() => step(-1)} disabled={!count} aria-label="Previous match" title="Previous match (Shift+Enter)">
    <svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true">
      <path d="M3.5 10L8 5.5l4.5 4.5" stroke="currentColor" stroke-width="1.6" fill="none" stroke-linecap="round" stroke-linejoin="round" />
    </svg>
  </button>
  <button class="icon" onclick={() => step(1)} disabled={!count} aria-label="Next match" title="Next match (Enter)">
    <svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true">
      <path d="M3.5 6l4.5 4.5L12.5 6" stroke="currentColor" stroke-width="1.6" fill="none" stroke-linecap="round" stroke-linejoin="round" />
    </svg>
  </button>
  <label><input type="checkbox" bind:checked={options.caseSensitive} />Match case</label>
  <label><input type="checkbox" bind:checked={options.wholeWord} />Whole word</label>
  <button class="icon" onclick={() => onclose?.()} aria-label="Close Find" title="Close (Escape)">
    <svg width="12" height="12" viewBox="0 0 16 16" aria-hidden="true">
      <path d="M3.5 3.5l9 9M12.5 3.5l-9 9" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round" />
    </svg>
  </button>
</div>

<style>
  .find {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.43rem 0.57rem;
    font-size: 0.93rem;
  }
  input:not([type]) {
    width: 12rem;
    min-width: 0;
    flex: 0 1 12rem;
    padding: 0.36rem 0.71rem;
    border-radius: 8px;
  }
  input.none {
    border-color: var(--danger);
  }
  .count {
    min-width: 5.5rem;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .icon {
    display: grid;
    place-items: center;
    width: 2.14rem;
    height: 2.14rem;
    padding: 0;
    border: none;
    border-radius: 8px;
    background: none;
    color: var(--fg-2);
  }
  label {
    display: inline-flex;
    align-items: center;
    gap: 0.36rem;
    color: var(--fg-2);
    white-space: nowrap;
    cursor: pointer;
  }
</style>
