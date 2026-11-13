<script lang="ts">
  import { clock, screenshotUrl, type ScreenshotView } from "./api";

  let {
    meetingId,
    onjump,
  }: {
    meetingId: string;
    /** Offered as "Jump to Transcript" when set: shows that Screenshot's place in the Transcript. */
    onjump?: (shot: ScreenshotView) => void;
  } = $props();

  let viewer: HTMLDialogElement | undefined = $state();
  let viewing = $state<ScreenshotView | null>(null);

  /** Shows a Screenshot full size. */
  export function open(shot: ScreenshotView) {
    viewing = shot;
    viewer?.showModal();
  }

  // Esc closes the dialog natively; so does a click anywhere but the image or the jump.
  function onViewerClick(e: MouseEvent) {
    if (!(e.target instanceof HTMLImageElement) && !(e.target instanceof HTMLButtonElement)) viewer?.close();
  }

  function jump() {
    const shot = viewing;
    viewer?.close();
    if (shot) onjump?.(shot);
  }
</script>

<dialog class="viewer" bind:this={viewer} onclick={onViewerClick} onclose={() => (viewing = null)}>
  {#if viewing}
    <img src={screenshotUrl(meetingId, viewing)} alt="Screenshot at {clock(viewing.ms / 1000)}" />
    <p>
      Screenshot at {clock(viewing.ms / 1000)} · Esc to close
      {#if onjump}· <button class="link" onclick={jump}>Jump to Transcript</button>{/if}
    </p>
  {/if}
</dialog>

<style>
  .viewer {
    max-width: none;
    max-height: none;
    width: 100vw;
    height: 100vh;
    margin: 0;
    padding: 16px;
    box-sizing: border-box;
    border: none;
    background: transparent;
    cursor: zoom-out;
  }
  .viewer[open] {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
  }
  .viewer::backdrop {
    background: rgb(0 0 0 / 0.85);
  }
  .viewer img {
    max-width: 100%;
    max-height: calc(100% - 2rem);
    object-fit: contain;
    cursor: default;
  }
  .viewer p {
    margin: 0;
    color: #e6e8eb;
    font-size: 0.85rem;
  }
  .viewer .link,
  .viewer .link:hover:not(:disabled) {
    padding: 0;
    color: inherit;
    font-size: inherit;
    font-weight: 600;
    text-decoration: underline;
    cursor: pointer;
  }
</style>
