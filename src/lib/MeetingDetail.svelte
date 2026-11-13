<script lang="ts">
  import { tick, untrack } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { save } from "@tauri-apps/plugin-dialog";
  import {
    api,
    clock,
    EVENTS,
    formatUsd,
    screenshotUrl,
    SPEND_KIND_LABEL,
    STATUS_CHIP,
    type JobEvent,
    type Meeting,
    type MeetingSpend,
    type Place,
    type ScreenshotView,
    type ScreenshotsMode,
    type Segment,
    type Summary,
  } from "./api";
  import { findRanges, paint, scrollToRange, unpaint, type FindOptions } from "./find";
  import FindBar from "./FindBar.svelte";
  import { renderMarkdown } from "./markdown";
  import NotesEditor from "./NotesEditor.svelte";
  import NotesSplit from "./NotesSplit.svelte";
  import ScreenshotViewer from "./ScreenshotViewer.svelte";
  import TranscriptView from "./TranscriptView.svelte";

  let {
    meeting,
    onchange,
    ondelete,
    onback,
    ontag,
    at = null,
  }: {
    meeting: Meeting;
    /** Where to open the Meeting, such as a search result's Passage. */
    at?: Place | null;
    onchange?: (meeting: Meeting) => void;
    ondelete?: (meeting: Meeting) => Promise<string | null>;
    /** Shows the Meetings list. */
    onback?: () => void;
    /** Adds this Tag to the Meetings list's search. */
    ontag?: (tag: string) => void;
  } = $props();

  let tab = $state<"summary" | "transcript" | "screenshots">("summary");
  let segments = $state<Segment[]>([]);
  let screenshots = $state<ScreenshotView[]>([]);
  let viewer: ScreenshotViewer | undefined = $state();
  /** The Screenshot the Transcript scrolls to when it opens from the gallery. */
  let reveal = $state<ScreenshotView | null>(null);
  let exportChoice: HTMLDialogElement | undefined = $state();
  let exportMode = $state<ScreenshotsMode>("folder");
  let summary = $state<Summary | null>(null);
  /** What the Meeting has cost; null until loaded. */
  let spend = $state<MeetingSpend | null>(null);
  let spendOpen = $state(false);
  let spendChip: HTMLElement | undefined = $state();
  const spendBreakdown = $derived(spend?.byKind.map((k) => `${SPEND_KIND_LABEL[k.kind]}: ${formatUsd(k.cost)}`).join("\n") ?? "");
  let title = $state("");
  let working = $state<string | null>(null);
  let message = $state<{ ok: boolean; text: string } | null>(null);
  /** The header menu that is open, if any; only one is at a time, so `menu` is its element. */
  let openMenu = $state<"export" | "more" | null>(null);
  let menuButtons = $state<{ export?: HTMLButtonElement; more?: HTMLButtonElement }>({});
  let menu: HTMLElement | undefined = $state();
  const menuButton = $derived(openMenu && menuButtons[openMenu]);

  /** Where `at` asks for, until each part has been shown. */
  let revealMs = $state<number | null>(null);
  let revealSection = $state<number | null>(null);
  let revealLine = $state<number | null>(null);
  let summaryScroll: HTMLElement | undefined = $state();
  const meetingId = $derived(meeting.id);

  /** Find (Ctrl+F) in the Summary, Transcript or Screenshots tab; the Notes have their own. */
  let finding = $state(false);
  let findQuery = $state("");
  let findOptions = $state<FindOptions>({ caseSensitive: false, wholeWord: false });
  let found = $state.raw<Range[]>([]);
  let foundAt = $state(0);
  let findBar: FindBar | undefined = $state();
  let mainPane: HTMLElement | undefined = $state();
  let notesPane: HTMLElement | undefined = $state();
  let notesEditor: NotesEditor | undefined = $state();

  // Ctrl+F finds in the Notes when they have focus, otherwise in the tab shown beside them.
  $effect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey || e.key.toLowerCase() !== "f") return;
      if (exportChoice?.open) return;
      e.preventDefault();
      if (notesPane?.contains(document.activeElement)) {
        closeFind();
        notesEditor?.find();
      } else {
        notesEditor?.closeFind();
        openFind();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  });

  async function openFind() {
    finding = true;
    await tick();
    findBar?.focus();
  }

  function closeFind() {
    if (!finding) return;
    finding = false;
    found = [];
    unpaint();
  }

  // Found again, from the first match, as the query, the options or what the tab shows changes.
  $effect(() => {
    if (!finding) return;
    const query = findQuery;
    const options = { ...findOptions };
    void tab;
    void summary;
    void segments;
    void screenshots;
    tick().then(() => {
      const panel = mainPane?.querySelector("[role=tabpanel]");
      found = panel && query ? findRanges(panel, query, options) : [];
      foundAt = 0;
    });
  });

  $effect(() => {
    if (!finding) return;
    paint(found, foundAt);
    const range = found[foundAt];
    if (range) scrollToRange(range);
    return unpaint;
  });

  $effect(() => {
    const place = at;
    void meetingId;
    untrack(() => {
      revealMs = place?.source === "transcript" ? place.ms : null;
      revealSection = place?.source === "summary" ? place.section : null;
      revealLine = place?.source === "notes" ? place.line : null;
      if (place?.source === "transcript" || place?.source === "summary") {
        reveal = null;
        tab = place.source;
      }
    });
  });

  // The section's heading at the top of the Summary, once it has loaded.
  $effect(() => {
    const section = revealSection;
    if (section === null || !summary || tab !== "summary" || !summaryScroll) return;
    revealSection = null;
    const scroller = summaryScroll;
    tick().then(() => {
      const heading = scroller.querySelectorAll("h1, h2, h3, h4, h5, h6")[section - 1];
      if (heading) scroller.scrollTo({ top: heading.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop });
    });
  });

  // Reload whenever another Meeting is shown.
  $effect(() => {
    const id = meeting.id;
    title = meeting.title;
    message = null;
    api.transcript(id).then((s) => (segments = s));
    api.summary(id).then((s) => (summary = s));
    spend = null;
    spendOpen = false;
    loadSpend(id);
  });

  async function loadSpend(id: string) {
    try {
      const s = await api.meetingSpend(id);
      if (id === meeting.id) spend = s;
    } catch {
      // The chip stays hidden.
    }
  }

  // Grows as Chunks are transcribed, and with each Summary and Tags request.
  $effect(() => {
    const unlisten = listen<string | null>(EVENTS.spend, (e) => {
      if (e.payload === meeting.id) loadSpend(meeting.id);
    });
    return () => void unlisten.then((f) => f());
  });

  // An outside click or Escape closes the breakdown.
  $effect(() => {
    if (!spendOpen) return;
    const onDown = (e: PointerEvent) => {
      if (!spendChip?.contains(e.target as Node)) spendOpen = false;
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") spendOpen = false;
    };
    document.addEventListener("pointerdown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  });

  // Screenshots are shown, and offered in the export, only once the Meeting has stopped.
  $effect(() => {
    const id = meeting.id;
    screenshots = [];
    if (stopped) loadScreenshots(id);
  });

  async function loadScreenshots(id: string) {
    try {
      const shots = await api.screenshots(id);
      if (id === meeting.id) screenshots = shots;
    } catch (e) {
      message = { ok: false, text: String(e) };
    }
  }

  $effect(() => {
    const unlisten = listen<JobEvent>(EVENTS.job, (e) => onJobEvent(e.payload));
    return () => void unlisten.then((f) => f());
  });

  function onJobEvent(event: JobEvent) {
    if (event.meetingId !== meeting.id) return;
    switch (event.kind) {
      case "retrying":
        working = "Retrying failed Chunks…";
        break;
      case "retried":
        working = null;
        api.transcript(meeting.id).then((s) => (segments = s));
        if (stopped) loadScreenshots(meeting.id);
        refreshMeeting();
        break;
      case "summarizing":
        working = "Summarizing…";
        break;
      case "summarized":
        working = null;
        summary = event.summary;
        break;
      case "failed":
        working = null;
        message = { ok: false, text: event.message };
        break;
    }
  }

  function jumpToTranscript(shot: ScreenshotView) {
    reveal = shot;
    tab = "transcript";
  }

  function showTab(next: typeof tab) {
    reveal = null;
    revealMs = null;
    tab = next;
  }

  async function refreshMeeting() {
    const all = await api.listMeetings();
    const fresh = all.find((m) => m.id === meeting.id);
    if (fresh) onchange?.(fresh);
  }

  async function saveTitle(next: string) {
    if (next.trim() === meeting.title) return;
    try {
      const updated = await api.setTitle(meeting.id, next);
      title = updated.title;
      onchange?.(updated);
    } catch (e) {
      title = meeting.title;
      message = { ok: false, text: String(e) };
    }
  }

  async function resummarize() {
    message = null;
    try {
      summary = await api.summarize(meeting.id);
    } catch (e) {
      message = { ok: false, text: String(e) };
    }
  }

  async function retry() {
    message = null;
    try {
      const report = await api.retryFailedChunks(meeting.id);
      const recovered = `Recovered ${report.recovered} Chunk${report.recovered === 1 ? "" : "s"}.`;
      message = report.remaining
        ? { ok: false, text: `${recovered} ${report.remaining} still failed${report.error ? `: ${report.error}` : "."}` }
        : { ok: true, text: `${recovered} Re-summarize to include them.` };
    } catch (e) {
      message = { ok: false, text: String(e) };
    }
  }

  async function exportMarkdown() {
    message = null;
    if (!screenshots.length) return saveExport(null);
    try {
      exportMode = (await api.getSettings()).exportScreenshots;
    } catch {
      // Keep the last choice made in this window.
    }
    exportChoice?.showModal();
  }

  /** The choice offered before the Save dialog, since GTK's dialog can't carry it. */
  function chooseExport(e: SubmitEvent) {
    e.preventDefault();
    exportChoice?.close();
    saveExport(exportMode);
  }

  function exportSummary() {
    message = null;
    saveExport(null, true);
  }

  async function saveExport(mode: ScreenshotsMode | null, summaryOnly = false) {
    try {
      const path = await save({
        defaultPath: await api.exportDefaultPath(meeting.id),
        filters: [{ name: "Markdown", extensions: ["md"] }],
      });
      if (!path) return; // Cancelled: nothing is written and the remembered folder stays.
      message = { ok: true, text: `Exported to ${await api.exportMeeting(meeting.id, path, mode, summaryOnly)}` };
    } catch (e) {
      message = { ok: false, text: String(e) };
    }
  }

  function showMenu(name: "export" | "more") {
    openMenu = name;
    tick().then(() => menuItems()[0]?.focus());
  }

  function closeMenu(refocus = true) {
    const button = menuButton;
    openMenu = null;
    if (refocus) button?.focus();
  }

  function toggleMenu(name: "export" | "more") {
    if (openMenu === name) closeMenu();
    else showMenu(name);
  }

  /** The items that can take focus: enabled, and shown at this width. */
  const menuItems = () =>
    [...(menu?.querySelectorAll<HTMLButtonElement>("[role=menuitem]:not(:disabled)") ?? [])].filter((b) => b.offsetParent);

  /** Arrow keys, Home and End move through the items; Tab moves on as usual and closes the menu when it leaves. */
  function onMenuKey(e: KeyboardEvent) {
    const items = menuItems();
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    const to =
      e.key === "ArrowDown" ? (at + 1) % items.length
      : e.key === "ArrowUp" ? (at - 1 + items.length) % items.length
      : e.key === "Home" ? 0
      : e.key === "End" ? items.length - 1
      : null;
    if (e.key === "Escape") {
      e.preventDefault();
      closeMenu();
    } else if (to !== null && items.length) {
      e.preventDefault();
      items[to].focus();
    }
  }

  /** Arrow Down opens the menu too; Escape closes it while focus is still here (every item disabled). */
  function onMenuButtonKey(e: KeyboardEvent, name: "export" | "more") {
    if (e.key === "ArrowDown" && openMenu !== name) {
      e.preventDefault();
      showMenu(name);
    } else if (e.key === "Escape" && openMenu === name) {
      e.preventDefault();
      closeMenu();
    }
  }

  function onMenuFocusOut(e: FocusEvent) {
    const next = e.relatedTarget as Node | null;
    if (next && !menu?.contains(next) && next !== menuButton) closeMenu(false);
  }

  /** Runs a menu item's action once the menu has closed. */
  function pick(action: () => void) {
    closeMenu();
    action();
  }

  // An outside click closes the menu.
  $effect(() => {
    if (!openMenu) return;
    const onDown = (e: PointerEvent) => {
      const target = e.target as Node;
      if (!menu?.contains(target) && !menuButton?.contains(target)) closeMenu(false);
    };
    document.addEventListener("pointerdown", onDown);
    return () => document.removeEventListener("pointerdown", onDown);
  });

  async function remove() {
    message = null;
    const problem = await ondelete?.(meeting);
    if (problem) message = { ok: false, text: problem };
  }

  const dateFormat = new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric" });
  const dateYearFormat = new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric", year: "numeric" });
  const timeFormat = new Intl.DateTimeFormat(undefined, { timeStyle: "short" });

  const startedLabel = $derived.by(() => {
    const d = new Date(meeting.started_at);
    const date = (d.getFullYear() === new Date().getFullYear() ? dateFormat : dateYearFormat).format(d);
    return `${date} · ${timeFormat.format(d)}`;
  });

  /** "31 min", "1 h 5 min", from the start and end times; null until the Meeting has ended. */
  const duration = $derived.by(() => {
    if (!meeting.ended_at) return null;
    const minutes = Math.round((new Date(meeting.ended_at).getTime() - new Date(meeting.started_at).getTime()) / 60000);
    if (!(minutes >= 0)) return null;
    if (minutes < 1) return "< 1 min";
    const h = Math.floor(minutes / 60);
    return h ? `${h} h ${minutes % 60} min` : `${minutes} min`;
  });

  const status = $derived(STATUS_CHIP[meeting.status]);
  const stopped = $derived(meeting.status === "complete" || meeting.status === "incomplete");
</script>

<section class="detail">
  <header class="bar">
    <button class="back" onclick={() => onback?.()}>
      <svg width="18" height="18" viewBox="0 0 16 16" aria-hidden="true">
        <path d="M10 3L5 8l5 5" stroke="currentColor" stroke-width="1.6" fill="none" stroke-linecap="round" stroke-linejoin="round" />
      </svg>Meetings
    </button>
    <div class="actions">
      {#if meeting.status === "incomplete"}
        <button class="action wide-only" onclick={retry} disabled={!!working}>
          <svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M13 8a5 5 0 11-1.5-3.6M13 2.5v2.5h-2.5" stroke="currentColor" stroke-width="1.4" fill="none" stroke-linecap="round" stroke-linejoin="round" />
          </svg>Retry failed Chunks
        </button>
      {/if}
      {#if !summary}
        <button class="action" onclick={resummarize} disabled={!!working || !stopped}>
          <svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M3 3.5h10M3 6.5h10M3 9.5h6M3 12.5h4" stroke="currentColor" stroke-width="1.4" fill="none" stroke-linecap="round" />
          </svg>Summarize
        </button>
      {/if}
      <div class="more">
        <button
          class="action"
          aria-haspopup="menu"
          aria-expanded={openMenu === "export"}
          bind:this={menuButtons.export}
          onclick={() => toggleMenu("export")}
          onkeydown={(e) => onMenuButtonKey(e, "export")}
        >
          <svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M8 2v8M4.5 6.5L8 10l3.5-3.5M3 13.5h10" stroke="currentColor" stroke-width="1.4" fill="none" stroke-linecap="round" stroke-linejoin="round" />
          </svg>Export
          <svg width="10" height="10" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M3.5 6l4.5 4.5L12.5 6" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round" stroke-linejoin="round" />
          </svg>
        </button>
        {#if openMenu === "export"}
          <div class="menu" role="menu" aria-label="Export" tabindex="-1" bind:this={menu} onkeydown={onMenuKey} onfocusout={onMenuFocusOut}>
            <button role="menuitem" onclick={() => pick(exportMarkdown)}>Export everything…</button>
            <button role="menuitem" onclick={() => pick(exportSummary)}>Export summary only…</button>
          </div>
        {/if}
      </div>
      <div class="more">
        <button
          class="action more-button"
          aria-label="More actions"
          title="More actions"
          aria-haspopup="menu"
          aria-expanded={openMenu === "more"}
          bind:this={menuButtons.more}
          onclick={() => toggleMenu("more")}
          onkeydown={(e) => onMenuButtonKey(e, "more")}
        >
          <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
            <circle cx="3.5" cy="8" r="1.3" fill="currentColor" /><circle cx="8" cy="8" r="1.3" fill="currentColor" /><circle
              cx="12.5"
              cy="8"
              r="1.3"
              fill="currentColor"
            />
          </svg>
        </button>
        {#if openMenu === "more"}
          <div class="menu" role="menu" aria-label="More actions" tabindex="-1" bind:this={menu} onkeydown={onMenuKey} onfocusout={onMenuFocusOut}>
            {#if summary}
              <button role="menuitem" onclick={() => pick(resummarize)} disabled={!!working || !stopped}>Re-summarize</button>
            {/if}
            {#if meeting.status === "incomplete"}
              <button role="menuitem" class="narrow-only" onclick={() => pick(retry)} disabled={!!working}>Retry failed Chunks</button>
            {/if}
            <button role="menuitem" class="delete" onclick={() => pick(remove)} disabled={!!working || !stopped}>Delete…</button>
          </div>
        {/if}
      </div>
    </div>
  </header>

  <div class="head">
    <input
      class="title"
      aria-label="Meeting title"
      bind:value={title}
      onchange={() => saveTitle(title)}
      onkeydown={(e) => e.key === "Enter" && (e.currentTarget as HTMLInputElement).blur()}
    />
    <div class="chips">
      <span class="chip outline">{startedLabel}</span>
      {#if duration}<span class="chip outline">{duration}</span>{/if}
      <span class="chip {status.chip}">{status.label}</span>
      {#if spend?.byKind.length}
        <span class="spend" bind:this={spendChip}>
          <button
            class="chip outline"
            title={spendOpen ? "Spend" : `Spend\n${spendBreakdown}`}
            aria-expanded={spendOpen}
            onclick={() => (spendOpen = !spendOpen)}>{formatUsd(spend.total)}</button
          >
          {#if spendOpen}
            <div class="spend-breakdown" role="dialog" aria-label="Spend by kind">
              {#each spend.byKind as k (k.kind)}
                <span>{SPEND_KIND_LABEL[k.kind]}</span><span class="amount">{formatUsd(k.cost)}</span>
              {/each}
            </div>
          {/if}
        </span>
      {/if}
      {#each meeting.tags ?? [] as tag (tag)}
        <button class="tag" title="Search the Meetings tagged {tag}" onclick={() => ontag?.(tag)}>{tag}</button>
      {/each}
    </div>
    {#if working}
      <div class="callout working" role="status">
        <span class="dots" aria-hidden="true"><span></span><span></span><span></span></span>{working}
      </div>
    {/if}
    {#if message}
      <div class="callout {message.ok ? 'ok' : 'problem'}" role={message.ok ? "status" : "alert"}>
        <span>{message.text}</span>
        <button class="link" onclick={() => (message = null)}>Dismiss</button>
      </div>
    {/if}
  </div>

  <NotesSplit>
    {#snippet main()}
      <div class="main-host" bind:this={mainPane}>
        <div class="tabs-row">
          <div class="segmented" role="tablist" aria-label="Meeting views">
            <button role="tab" aria-selected={tab === "summary"} onclick={() => showTab("summary")}>Summary</button>
            <button role="tab" aria-selected={tab === "transcript"} onclick={() => showTab("transcript")}>Transcript</button>
            {#if screenshots.length || tab === "screenshots"}
              <button role="tab" aria-selected={tab === "screenshots"} onclick={() => showTab("screenshots")}>Screenshots</button>
            {/if}
          </div>
          {#if finding}
            <FindBar bind:this={findBar} bind:query={findQuery} bind:options={findOptions} bind:current={foundAt} count={found.length} onclose={closeFind} />
          {/if}
        </div>

        {#if tab === "summary"}
          <div class="summary-scroll" role="tabpanel" aria-label="Summary" bind:this={summaryScroll}>
            <article class="summary markdown">
              {#if summary}
                {@html renderMarkdown(summary.markdown)}
              {:else if working}
                <p class="empty" data-nofind>{working}</p>
              {:else}
                <p class="empty" data-nofind>No Summary yet.</p>
              {/if}
            </article>
          </div>
        {:else if tab === "transcript"}
          <div class="transcript-tab" role="tabpanel" aria-label="Transcript">
            <TranscriptView {segments} meetingId={meeting.id} {screenshots} {reveal} {revealMs} />
          </div>
        {:else}
          <div class="gallery-scroll" role="tabpanel" aria-label="Screenshots">
            {#if screenshots.length}
              <ul class="gallery">
                {#each screenshots as shot (shot.file)}
                  <li>
                    <button class="shot" onclick={() => viewer?.open(shot)} aria-label="View screenshot at {clock(shot.ms / 1000)}">
                      <img src={screenshotUrl(meeting.id, shot)} alt="" loading="lazy" />
                      <time data-nofind>{clock(shot.ms / 1000)}</time>
                    </button>
                  </li>
                {/each}
              </ul>
            {:else}
              <p class="empty" data-nofind>No Screenshots.</p>
            {/if}
          </div>
          <ScreenshotViewer meetingId={meeting.id} onjump={jumpToTranscript} bind:this={viewer} />
        {/if}
      </div>
    {/snippet}
    {#snippet notes()}
      <div class="notes-host" bind:this={notesPane}>
        <NotesEditor meetingId={meeting.id} column {revealLine} bind:this={notesEditor} />
      </div>
    {/snippet}
  </NotesSplit>

  <dialog class="export-choice" bind:this={exportChoice}>
    <form onsubmit={chooseExport}>
      <h2>Export .md</h2>
      <fieldset>
        <legend>This Meeting has {screenshots.length} screenshot{screenshots.length === 1 ? "" : "s"}.</legend>
        <label><input type="radio" bind:group={exportMode} value="none" /> Without screenshots</label>
        <label><input type="radio" bind:group={exportMode} value="folder" /> In a folder next to the .md</label>
        <label><input type="radio" bind:group={exportMode} value="embedded" /> Embedded in the .md</label>
      </fieldset>
      <div class="choice-actions">
        <button type="button" onclick={() => exportChoice?.close()}>Cancel</button>
        <button type="submit" class="primary">Choose file…</button>
      </div>
    </form>
  </dialog>
</section>

<style>
  .detail {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.86rem;
    padding: 1rem 1.71rem;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }
  .back {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 0.43rem;
    height: 3.14rem;
    padding: 0 0.71rem;
    border: none;
    background: none;
    color: var(--fg-2);
  }
  .actions {
    flex: 1;
    min-width: 0;
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: 0.57rem;
  }
  .action {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 0.57rem;
    height: 2.86rem;
    padding: 0 1rem;
    white-space: nowrap;
  }
  .action.more-button {
    width: 3.14rem;
    padding: 0;
    justify-content: center;
  }
  .more {
    position: relative;
  }
  .menu {
    position: absolute;
    right: 0;
    top: calc(100% + 6px);
    z-index: 10;
    min-width: 13rem;
    display: flex;
    flex-direction: column;
    padding: 0.29rem;
    border: 1px solid var(--border);
    border-radius: 12px;
    background: var(--surface);
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.14);
  }
  .menu button {
    border: none;
    border-radius: 8px;
    background: none;
    padding: 0.57rem 0.86rem;
    text-align: left;
    white-space: nowrap;
  }
  .menu button:focus-visible {
    outline-offset: -2px;
  }
  .menu .delete:not(:disabled) {
    color: var(--danger);
  }
  .narrow-only {
    display: none;
  }
  .head {
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 0.86rem;
    padding: 2rem 2.29rem 1.14rem;
  }
  .title {
    width: 100%;
    margin-left: -0.43rem;
    padding: 0.14rem 0.43rem;
    border: 1px solid transparent;
    background: none;
    color: inherit;
    font-family: var(--font-serif);
    font-size: 2.57rem;
    font-weight: 600;
    line-height: 1.2;
    text-overflow: ellipsis;
  }
  .title:hover {
    border-color: var(--border);
  }
  .callout {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 0.43rem 12px;
  }
  .callout .link,
  .callout .link:hover:not(:disabled) {
    color: inherit;
    font-weight: 600;
    text-decoration: underline;
  }
  .links {
    display: flex;
    gap: 1rem;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.57rem;
  }
  .chip.outline {
    background: var(--surface);
    border: 1px solid var(--border);
  }
  .spend {
    position: relative;
  }
  .spend button.chip {
    cursor: pointer;
    font-variant-numeric: tabular-nums;
  }
  .spend-breakdown {
    position: absolute;
    left: 0;
    top: calc(100% + 6px);
    z-index: 10;
    display: grid;
    grid-template-columns: auto auto;
    gap: 0.43rem 1.71rem;
    padding: 0.71rem 0.86rem;
    border: 1px solid var(--border);
    border-radius: 12px;
    background: var(--surface);
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.14);
    font-size: 0.93rem;
    white-space: nowrap;
  }
  .spend-breakdown .amount {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .callout.working {
    justify-content: flex-start;
    align-items: center;
    gap: 0.57rem;
    color: var(--fg-2);
  }
  .main-host {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .notes-host {
    display: contents;
  }
  .tabs-row {
    flex: none;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.57rem 1.14rem;
    padding: 0 2.29rem 0.57rem;
  }
  .summary-scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0.57rem 2.29rem 2.29rem;
  }
  .summary {
    /* About 68 characters a line. */
    max-width: 68ch;
  }
  .summary > :global(:first-child) {
    margin-top: 0.57rem;
  }
  .empty {
    color: var(--muted);
    padding: 1.71rem 1.43rem;
    border: 1px dashed var(--border);
    border-radius: 12px;
    font-size: 1rem;
  }
  .transcript-tab {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .gallery-scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0.57rem 2.29rem 2.29rem;
  }
  /* Two across, as on the Meeting notes board; one in a narrow pane. */
  .gallery {
    list-style: none;
    margin: 0.57rem 0 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(max(min(100%, 16rem), calc((100% - 1.14rem) / 2)), 1fr));
    gap: 1.14rem;
    max-width: 68rem;
  }
  .gallery .shot {
    position: relative;
    display: block;
    width: 100%;
    aspect-ratio: 16 / 10;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 12px;
    overflow: hidden;
    background: var(--hover);
    cursor: zoom-in;
  }
  .gallery img {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
  }
  .gallery time {
    position: absolute;
    left: 0.57rem;
    bottom: 0.57rem;
    padding: 0.14rem 0.43rem;
    border-radius: 6px;
    background: var(--surface);
    color: var(--fg-2);
    font-family: var(--font-mono);
    font-size: 0.86rem;
    font-variant-numeric: tabular-nums;
  }
  .export-choice {
    max-width: min(28rem, calc(100vw - 32px));
  }
  .export-choice h2 {
    margin: 0 0 12px;
    font-size: 1.29rem;
  }
  .export-choice fieldset {
    border: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.43rem;
  }
  .export-choice legend {
    color: var(--muted);
    padding: 0;
    margin-bottom: 0.57rem;
  }
  .export-choice label {
    display: flex;
    align-items: center;
    gap: 0.71rem;
    min-height: 3.14rem;
    padding: 0 0.86rem;
    border: 1px solid var(--border);
    border-radius: 10px;
    cursor: pointer;
  }
  .export-choice label:has(:checked) {
    border-color: var(--accent);
    background: var(--accent-bg);
  }
  .export-choice input {
    margin: 0;
  }
  .choice-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 1.43rem;
  }
  @media (max-width: 600px) {
    .bar {
      padding: 0.57rem 1rem;
      gap: 0.43rem;
    }
    .back {
      padding: 0 0.29rem;
    }
    .wide-only {
      display: none;
    }
    .narrow-only {
      display: block;
    }
    .head {
      padding: 1.14rem 1.14rem 0.86rem;
    }
    .title {
      font-size: 1.57rem;
    }
    .tabs-row,
    .summary-scroll,
    .gallery-scroll {
      padding-inline: 1.14rem;
    }
  }
  /* Leave the Summary, Transcript and Notes room in a short window. */
  @media (max-height: 600px) {
    .head {
      padding-top: 1rem;
      gap: 0.57rem;
    }
    .title {
      font-size: 1.71rem;
    }
  }
</style>
