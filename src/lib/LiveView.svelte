<script lang="ts">
  import { live } from "./live.svelte";
  import NotesEditor from "./NotesEditor.svelte";
  import NotesSplit from "./NotesSplit.svelte";
  import TranscriptView from "./TranscriptView.svelte";

  let {
    onopen,
    onback,
  }: {
    /** Opens a Meeting in the Meetings view. */
    onopen?: (meetingId: string) => void;
    /** Shows the Meetings list. */
    onback?: () => void;
  } = $props();

  const title = $derived(live.meeting?.title ?? "No Meeting yet");
</script>

<section class="live">
  <header>
    <button class="back" onclick={() => onback?.()} aria-label="Back to Meetings" title="Back to Meetings">
      <svg width="18" height="18" viewBox="0 0 16 16" aria-hidden="true">
        <path d="M10 3L5 8l5 5" stroke="currentColor" stroke-width="1.6" fill="none" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </button>
    {#if live.recording}<span class="chip rec">Recording</span>{/if}
    <h2 class:in-notes={live.meeting !== null}>{title}</h2>
    {#if live.recording}
      <button class="primary" onclick={() => live.stop()} disabled={live.busy}>
        <span class="icon stop" aria-hidden="true"></span>Stop &amp; summarize
      </button>
    {:else}
      <button class="record" onclick={() => live.start()} disabled={live.busy}>
        <span class="icon start" aria-hidden="true"></span>Start recording
      </button>
    {/if}
  </header>

  {#if live.error || (live.recording && live.screenshotsProblem) || live.notice}
    <div class="callouts">
      {#if live.error}
        <div class="callout problem" role="alert">
          <span>{live.error}</span>
          <button class="link" onclick={() => (live.error = null)}>Dismiss</button>
        </div>
      {/if}
      {#if live.recording && live.screenshotsProblem}
        <div class="callout warn">{live.screenshotsProblem}</div>
      {/if}
      {#if live.notice}
        <div class="callout" class:ok={live.summaryReady}>
          <span>{live.notice}</span>
          {#if live.summaryReady && live.meeting}
            {@const id = live.meeting.id}
            <button class="link" onclick={() => onopen?.(id)}>Open the Summary</button>
          {/if}
        </div>
      {/if}
    </div>
  {/if}

  <NotesSplit notes={live.meeting ? notes : undefined}>
    {#snippet main()}
      <aside aria-labelledby="live-transcript">
        <h3 id="live-transcript">Live Transcript</h3>
        <TranscriptView
          segments={live.segments}
          follow
          stacked
          pending={live.recording ? "Transcribing the next chunk…" : undefined}
        />
      </aside>
    {/snippet}
  </NotesSplit>
  {#snippet notes()}
    {#if live.meeting}<NotesEditor meetingId={live.meeting.id} {title} />{/if}
  {/snippet}
</section>

<style>
  .live {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.57rem 1.14rem;
    padding: 1rem 1.71rem;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }
  .back {
    flex: none;
    width: 3.14rem;
    height: 3.14rem;
    padding: 0;
    border: none;
    border-radius: 10px;
    background: none;
    color: var(--fg-2);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .chip.rec {
    height: 2.57rem;
    padding: 0 1rem;
    gap: 0.71rem;
    font-size: 1rem;
  }
  .chip.rec::before {
    width: 0.71rem;
    height: 0.71rem;
    box-shadow: 0 0 0 4px color-mix(in srgb, var(--rec-dot) 22%, transparent);
  }
  h2 {
    flex: 1 1 0;
    min-width: 0;
    margin: 0;
    font-family: inherit;
    font-size: 1rem;
    font-weight: 500;
    color: var(--fg-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  header button.primary,
  header button.record {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 0.57rem;
    height: 2.86rem;
    padding: 0 1.29rem;
    white-space: nowrap;
  }
  .icon {
    flex: none;
    width: 0.71rem;
    height: 0.71rem;
    background: currentColor;
  }
  .icon.stop {
    border-radius: 2px;
  }
  .icon.start {
    border-radius: 50%;
  }
  .callouts {
    display: flex;
    flex-direction: column;
    gap: 0.57rem;
    padding: 0.86rem 1.71rem;
    border-bottom: 1px solid var(--border);
  }
  .callout {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 12px;
  }
  .callout .link,
  .callout .link:hover:not(:disabled) {
    flex: none;
    color: inherit;
    font-weight: 600;
    text-decoration: underline;
  }
  aside {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--aside-bg);
  }
  h3 {
    flex: none;
    margin: 0;
    padding: 1.29rem 1.43rem;
    border-bottom: 1px solid var(--border);
    font-size: 1rem;
    font-weight: 600;
  }
  @media (max-width: 600px) {
    header {
      padding: 0.57rem 1rem;
    }
    .callouts {
      padding: 0.57rem 1rem;
    }
    /* The Notes pane shows it. */
    h2.in-notes {
      visibility: hidden;
    }
  }
  /* Leave the Transcript and Notes room in a short window. */
  @media (max-height: 560px) {
    .callouts {
      max-height: 30vh;
      overflow-y: auto;
    }
  }
</style>
