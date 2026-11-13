<script lang="ts" module>
  /**
   * Saves from every editor run one after another, and loads wait for them, so an older save never lands after
   * a newer one and an editor opened as another closes (Live view to Meetings) sees the last edit.
   */
  let queue: Promise<unknown> = Promise.resolve();

  /** Every open editor's `flush`. */
  const editors = new Set<() => void>();

  /** Saves the Notes every open editor has waiting on autosave; resolves once they are written (or failed). */
  export async function flushAllNotes(): Promise<void> {
    for (const flush of editors) flush();
    let last: Promise<unknown>;
    do {
      last = queue;
      await last;
    } while (last !== queue);
  }
</script>

<script lang="ts">
  import { onDestroy, tick, untrack } from "svelte";
  import { api } from "./api";
  import { findInText, findRanges, paint, scrollToRange, unpaint, type FindOptions } from "./find";
  import FindBar from "./FindBar.svelte";
  import { renderMarkdown } from "./markdown";

  let {
    meetingId,
    title,
    column = false,
    revealLine = null,
  }: {
    /** The Meeting whose Notes (notes.md) this edits. Edits autosave shortly after typing stops. */
    meetingId: string;
    /** Shown large above a centred writing column, with a hint while the Notes are empty (the Live view). */
    title?: string;
    /** The centred writing column under a serif "Notes" heading, without the title or hint (the Meeting view). */
    column?: boolean;
    /** A line of the Notes to select and scroll to once they load, such as a search result's paragraph. */
    revealLine?: number | null;
  } = $props();

  const AUTOSAVE_MS = 800;
  /** Saved Notes are indexed for Search when the editor loses focus or closes, or after this long without a save. */
  const INDEX_AFTER_MS = 30_000;

  let text = $state("");
  /** The Meeting `text` belongs to; null while its Notes load, so nothing is saved over them. */
  let loadedId = $state<string | null>(null);
  let preview = $state(false);
  let status = $state<"saved" | "pending" | "saving" | "failed">("saved");
  let problem = $state<string | null>(null);

  const hint = $derived(title !== undefined && loadedId !== null && !text.trim());

  let timer: ReturnType<typeof setTimeout> | undefined;
  let dirty = false;
  /** The Meeting whose saved Notes are not indexed for Search yet. */
  let unindexed: string | null = null;
  let indexTimer: ReturnType<typeof setTimeout> | undefined;
  let textarea: HTMLTextAreaElement | undefined = $state();

  // Load the Notes whenever another Meeting is shown, saving any unsaved edits to the previous one first.
  $effect(() => {
    const id = meetingId;
    untrack(flush);
    index();
    loadedId = null;
    problem = null;
    status = "saved";
    const load = queue.then(() => api.notes(id));
    queue = load.catch(() => {});
    load
      .then((notes) => {
        if (id !== meetingId) return;
        text = notes;
        loadedId = id;
      })
      .catch((e) => {
        if (id === meetingId) problem = `Could not load Notes: ${e}`;
      });
  });

  editors.add(flush);
  onDestroy(() => {
    editors.delete(flush);
    flush();
    index();
  });

  // Closing the window only hides it; save straight away rather than leave the edit to a throttled timer.
  $effect(() => {
    const onHide = () => {
      if (!document.hidden) return;
      flush();
      index();
    };
    document.addEventListener("visibilitychange", onHide);
    return () => document.removeEventListener("visibilitychange", onHide);
  });

  function edited() {
    if (loadedId === null) return;
    dirty = true;
    status = "pending";
    clearTimeout(timer);
    timer = setTimeout(flush, AUTOSAVE_MS);
  }

  function flush() {
    clearTimeout(timer);
    if (!dirty || loadedId === null) return;
    dirty = false;
    const id = loadedId;
    const notes = text;
    unindexed = id;
    clearTimeout(indexTimer);
    indexTimer = setTimeout(index, INDEX_AFTER_MS);
    queue = queue.then(async () => {
      if (id === loadedId) status = "saving";
      try {
        await api.saveNotes(id, notes);
        if (id === loadedId && !dirty) status = "saved";
        problem = null;
      } catch (e) {
        if (id === loadedId) {
          status = "failed";
          dirty = true;
        }
        problem = String(e);
      }
    });
  }
  /** Indexes the saved Notes for Search, after any save still queued; not on every autosave. */
  function index() {
    clearTimeout(indexTimer);
    const id = unindexed;
    if (id === null) return;
    unindexed = null;
    queue = queue.then(() => api.indexMeeting(id).catch(() => {}));
  }

  // Selects the paragraph starting at that line, once, when the Notes have loaded.
  let revealed: { id: string; line: number } | null = null;
  $effect(() => {
    const line = revealLine;
    if (line === null || loadedId !== meetingId || !textarea) return;
    if (revealed?.id === loadedId && revealed.line === line) return;
    revealed = { id: loadedId, line };
    const lines = untrack(() => text).split("\n");
    const start = lines.slice(0, line).reduce((n, l) => n + l.length + 1, 0);
    let end = line;
    while (end + 1 < lines.length && lines[end + 1].trim()) end++;
    const stop = start + lines.slice(line, end + 1).join("\n").length;
    const el = textarea;
    el.scrollTop = (line / Math.max(lines.length, 1)) * el.scrollHeight - el.clientHeight / 3;
    el.focus({ preventScroll: true });
    el.setSelectionRange(start, stop);
  });

  let finding = $state(false);
  let findQuery = $state("");
  let findOptions = $state<FindOptions>({ caseSensitive: false, wholeWord: false });
  let foundAt = $state(0);
  let findBar: FindBar | undefined = $state();
  let previewEl: HTMLElement | undefined = $state();
  /** Behind the textarea, the same text laid out the same way, with the matches marked: a textarea can't highlight. */
  let mirror: HTMLElement | undefined = $state();
  let mirrorStyle = $state("");

  /** Opens Find in the Notes (Ctrl+F), or puts the cursor back in its query. */
  export async function find() {
    finding = true;
    await tick();
    findBar?.focus();
  }

  export function closeFind() {
    if (!finding) return;
    finding = false;
    unpaint();
  }

  /** Closed from the bar: back to editing, with the current match selected. */
  function endFind() {
    const match = preview ? undefined : matches[foundAt];
    closeFind();
    if (!textarea) return;
    textarea.focus();
    if (match) textarea.setSelectionRange(match.start, match.end);
  }

  const matches = $derived(finding && !preview ? findInText(text, findQuery, findOptions) : []);
  /** In the preview, the matches in the rendered Notes. */
  let ranges = $state.raw<Range[]>([]);
  const count = $derived(preview ? ranges.length : matches.length);

  // The text around and between the matches, for the mirror.
  const pieces = $derived.by(() => {
    const out: { text: string; match?: number }[] = [];
    let at = 0;
    matches.forEach((m, n) => {
      out.push({ text: text.slice(at, m.start) }, { text: text.slice(m.start, m.end), match: n });
      at = m.end;
    });
    // A trailing line break only takes up a line in a textarea.
    out.push({ text: text.slice(at) + (text.endsWith("\n") ? " " : "") });
    return out;
  });

  $effect(() => {
    if (!finding || !preview) return void (ranges = []);
    const query = findQuery;
    const options = { ...findOptions };
    tick().then(() => (ranges = previewEl ? findRanges(previewEl, query, options) : []));
  });

  // A new search starts at the first match. Edits to the Notes keep the current one.
  $effect(() => {
    void findQuery;
    void findOptions.caseSensitive;
    void findOptions.wholeWord;
    void preview;
    foundAt = 0;
  });

  $effect(() => {
    if (!finding) return;
    void count;
    if (foundAt >= count) foundAt = Math.max(0, count - 1);
  });

  $effect(() => {
    if (!finding || !preview) return;
    paint(ranges, foundAt);
    return unpaint;
  });

  $effect(() => {
    if (!finding) return;
    void findQuery;
    void findOptions.caseSensitive;
    void findOptions.wholeWord;
    void foundAt;
    void ranges;
    tick().then(revealMatch);
  });

  function revealMatch() {
    if (preview) {
      const range = ranges[foundAt];
      if (range) scrollToRange(range);
      return;
    }
    const mark = mirror?.querySelector<HTMLElement>("mark.current");
    if (!mark || !textarea) return;
    const top = mark.offsetTop;
    if (top < textarea.scrollTop || top + mark.offsetHeight > textarea.scrollTop + textarea.clientHeight)
      textarea.scrollTop = top - textarea.clientHeight / 3;
    syncMirror();
  }

  // The mirror follows the textarea's size and font.
  $effect(() => {
    if (!finding || preview || !textarea) return;
    const el = textarea;
    const observer = new ResizeObserver(() => {
      const s = getComputedStyle(el);
      mirrorStyle = [
        `left: ${el.offsetLeft + el.clientLeft}px`,
        `top: ${el.offsetTop + el.clientTop}px`,
        `width: ${el.clientWidth}px`,
        `height: ${el.clientHeight}px`,
        `padding: ${s.paddingTop} ${s.paddingRight} ${s.paddingBottom} ${s.paddingLeft}`,
        `font-family: ${s.fontFamily}`,
        `font-size: ${s.fontSize}`,
        `line-height: ${s.lineHeight}`,
        `letter-spacing: ${s.letterSpacing}`,
        `tab-size: ${s.tabSize}`,
      ].join("; ");
      tick().then(syncMirror);
    });
    observer.observe(el);
    return () => observer.disconnect();
  });

  function syncMirror() {
    if (mirror && textarea) mirror.scrollTop = textarea.scrollTop;
  }
</script>

<section class="notes" class:page={title !== undefined || column} class:column={title === undefined && column}>
  <header>
    <h3>Notes</h3>
    <span class="status" class:problem={status === "failed"}>
      {#if loadedId === null && !problem}Loading…
      {:else if status === "pending" || status === "saving"}Saving…
      {:else if status === "failed"}Not saved
      {:else}Saved{/if}
    </span>
    <button class="link" onclick={() => (preview = !preview)} disabled={loadedId === null}>
      {preview ? "Edit" : "Preview"}
    </button>
  </header>
  {#if title !== undefined}<h2 class="title">{title}</h2>{/if}
  {#if finding}
    <div class="find-row">
      <FindBar bind:this={findBar} bind:query={findQuery} bind:options={findOptions} bind:current={foundAt} {count} onclose={endFind} />
    </div>
  {/if}
  {#if problem}<p class="problem" role="alert">{problem}</p>{/if}
  {#if preview}
    <article class="preview markdown" bind:this={previewEl}>
      {#if text.trim()}
        {@html renderMarkdown(text)}
      {:else}
        <p class="empty">No Notes yet.</p>
      {/if}
    </article>
  {:else}
    <textarea
      aria-label="Notes (Markdown)"
      placeholder={title === undefined
        ? "Write Notes in Markdown. They take priority over the Transcript in the Summary."
        : "Start typing…"}
      spellcheck="true"
      disabled={loadedId === null}
      class:hinted={hint}
      bind:value={text}
      bind:this={textarea}
      oninput={edited}
      onscroll={syncMirror}
      onblur={() => {
        flush();
        index();
      }}
    ></textarea>
    {#if finding && matches.length}
      <!-- prettier-ignore -->
      <div class="mirror" aria-hidden="true" style={mirrorStyle} bind:this={mirror}>{#each pieces as piece, n (n)}{#if piece.match === undefined}{piece.text}{:else}<mark class:current={piece.match === foundAt}>{piece.text}</mark>{/if}{/each}</div>
    {/if}
    {#if hint}
      <p class="hint">
        Write rough notes here, in Markdown. When you stop, the Summary is written from them and the Transcript, and
        your Notes take priority.
      </p>
    {/if}
  {/if}
</section>

<style>
  .notes {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    height: 100%;
    /* For the find mirror. */
    position: relative;
  }
  header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 16px;
    border-bottom: 1px solid var(--border);
  }
  h3 {
    margin: 0;
    font-size: 0.95rem;
  }
  .status {
    color: var(--muted);
    font-size: 0.85rem;
    margin-right: auto;
  }
  .problem {
    color: var(--danger);
  }
  p.problem {
    margin: 6px 16px 0;
  }
  textarea {
    flex: 1;
    min-height: 0;
    resize: none;
    border: none;
    outline: none;
    padding: 12px 16px;
    background: none;
    color: inherit;
    /* Set from Settings by applyFonts. */
    font-family: var(--notes-font, "Noto Sans Mono"), ui-monospace, monospace;
    font-size: var(--notes-size, 14px);
    line-height: 1.5;
    /* Over the find mirror. */
    position: relative;
    z-index: 1;
  }
  .find-row {
    padding: 6px 16px;
    border-bottom: 1px solid var(--border);
  }
  .page .find-row {
    padding-block: 0 0.57rem;
    border-bottom: none;
  }
  .mirror {
    position: absolute;
    z-index: 0;
    margin: 0;
    overflow: hidden;
    white-space: pre-wrap;
    overflow-wrap: break-word;
    color: transparent;
    pointer-events: none;
  }
  .mirror mark {
    border-radius: 2px;
    background: var(--find-bg);
    color: transparent;
  }
  .mirror mark.current {
    background: var(--find-current-bg);
  }
  .preview {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 8px 16px 24px;
  }
  .empty {
    color: var(--muted);
  }
  /* The Recording board's writing column: a large serif title over the editor, on the surface colour. */
  .page {
    container-type: inline-size;
    overflow-y: auto;
    background: var(--surface);
    /* A 720 px column, centred. */
    --gutter: max(clamp(1.14rem, 5cqi, 4rem), calc((100cqi - 51.43rem) / 2));
    --edge: clamp(1.14rem, 6cqi, 3.43rem);
  }
  .page > * {
    flex: none;
    padding-inline: var(--gutter);
  }
  .page header {
    padding-top: var(--edge);
    padding-bottom: 0.29rem;
    border-bottom: none;
  }
  .page h3 {
    font-size: 0.86rem;
    font-weight: 400;
    color: var(--muted);
  }
  /* With no title above it, the heading is the title. */
  .column h3 {
    font-family: var(--font-serif);
    font-size: 1.29rem;
    font-weight: 600;
    color: var(--fg);
  }
  .column header {
    padding-bottom: 0.86rem;
  }
  .title {
    margin: 0;
    padding-bottom: 1.14rem;
    font-size: clamp(1.5rem, 7cqi, 2.43rem);
    line-height: 1.2;
    overflow-wrap: anywhere;
  }
  .page textarea,
  .page .preview {
    flex: 1 0 8rem;
    margin-inline: var(--gutter);
    padding: 1.29rem 0;
    border-top: 1px solid var(--border);
    border-radius: 0;
    line-height: 1.75;
  }
  /* The accent rule above the editor is the focus cue. */
  .page textarea:focus-visible {
    box-shadow: none;
    border-top-color: var(--accent);
  }
  /* Empty, the hint sits right under the editor rather than at the foot of the pane. */
  .page textarea.hinted {
    flex: 0 0 5rem;
  }
  .page p.problem {
    margin: 0 0 0.57rem;
  }
  .page .hint {
    margin: 0 var(--gutter) var(--edge);
    padding: 1rem 1.14rem;
    border: 1px dashed var(--input-border);
    border-radius: 12px;
    color: var(--fg-2);
    font-size: 0.93rem;
    line-height: 1.5;
  }
</style>
