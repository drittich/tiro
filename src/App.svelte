<script lang="ts">
  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import { listen } from "@tauri-apps/api/event";
  import { api, EVENTS, type Place } from "./lib/api";
  import { applyAppearance } from "./lib/appearance";
  import { flushAllNotes } from "./lib/NotesEditor.svelte";
  import { live } from "./lib/live.svelte";
  import AskView from "./lib/AskView.svelte";
  import LiveView from "./lib/LiveView.svelte";
  import MeetingsView from "./lib/MeetingsView.svelte";
  import SettingsView from "./lib/SettingsView.svelte";

  type View = "live" | "meetings" | "ask" | "settings";

  // A recording already running at startup brings the Live view forward (see the effect below).
  let view = $state<View>("meetings");
  let openId = $state<string | null>(null);
  /** Where to open `openId`: a cited Passage. */
  let openAt = $state<Place | null>(null);
  /** The Meeting was opened from the Ask view, so going back returns there. */
  let fromAsk = $state(false);
  /** A question for the Ask view to start a new conversation with: the search box's text. */
  let question = $state<string | null>(null);
  /** Bumped to show the Meetings list again, even while a Meeting is open in it. */
  let listKey = $state(0);
  let version = $state("");

  function openMeeting(id: string, at: Place | null = null) {
    openId = id;
    openAt = at;
    fromAsk = view === "ask";
    view = "meetings";
  }

  function go(to: View) {
    if (to === "meetings") listKey++;
    openId = null;
    openAt = null;
    fromAsk = false;
    view = to;
  }

  function ask(text: string) {
    go("ask");
    question = text;
  }

  onMount(() => {
    live.init();
    api.getSettings().then(applyAppearance);
    getVersion().then((v) => (version = v)).catch(() => {});
    const unlisten = listen<View>(EVENTS.navigate, (e) => go(e.payload));
    // Before a Summary reads notes.md, and before Quit, save Notes still waiting on autosave.
    const unlistenFlush = listen<number>(EVENTS.flushNotes, async (e) => {
      await flushAllNotes();
      await api.notesFlushed(e.payload);
    });
    return () => {
      void unlisten.then((f) => f());
      void unlistenFlush.then((f) => f());
    };
  });

  // A recording started from the tray brings the Live view forward.
  $effect(() => {
    if (live.recording) view = "live";
  });
</script>

<div class="app">
  <nav aria-label="Main">
    <div class="brand">
      <span class="mark" aria-hidden="true">
        <svg width="16" height="16" viewBox="0 0 16 16">
          <path d="M3 3.5h10L6.5 13.5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </span>
      <span class="name">Tiro</span>
    </div>
    {#if live.recording}
      <button
        class="item recording"
        class:active={view === "live"}
        aria-current={view === "live" ? "page" : undefined}
        aria-label="Recording"
        title="Recording"
        onclick={() => go("live")}
      >
        <span class="dot pulse" aria-hidden="true"></span><span class="label">Recording</span>
      </button>
    {:else}
      <button
        class="item new"
        class:active={view === "live"}
        aria-current={view === "live" ? "page" : undefined}
        aria-label="New recording"
        title="New recording"
        onclick={() => go("live")}
      >
        <span class="dot" aria-hidden="true"></span><span class="label">New recording</span>
      </button>
    {/if}
    <button
      class="item"
      class:active={view === "meetings"}
      aria-current={view === "meetings" ? "page" : undefined}
      aria-label="Meetings"
      title="Meetings"
      onclick={() => go("meetings")}
    >
      <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
        <path d="M3 3h10M3 8h10M3 13h6" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" />
      </svg><span class="label">Meetings</span>
    </button>
    <button
      class="item"
      class:active={view === "ask"}
      aria-current={view === "ask" ? "page" : undefined}
      aria-label="Ask"
      title="Ask"
      onclick={() => go("ask")}
    >
      <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
        <path
          d="M2.5 3.5a1 1 0 011-1h9a1 1 0 011 1v6.5a1 1 0 01-1 1H7l-3 2.5V11h-.5a1 1 0 01-1-1z"
          stroke="currentColor"
          stroke-width="1.5"
          fill="none"
          stroke-linejoin="round"
        />
      </svg><span class="label">Ask</span>
    </button>
    <button
      class="item"
      class:active={view === "settings"}
      aria-current={view === "settings" ? "page" : undefined}
      aria-label="Settings"
      title="Settings"
      onclick={() => go("settings")}
    >
      <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
        <circle cx="8" cy="8" r="2.2" stroke="currentColor" stroke-width="1.5" fill="none" />
        <path
          d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
        />
      </svg><span class="label">Settings</span>
    </button>
    {#if version}<div class="version">Tiro v{version}</div>{/if}
  </nav>
  <main>
    {#if view === "live"}
      <LiveView onopen={openMeeting} onback={() => go("meetings")} />
    {:else if view === "meetings"}
      {#key listKey}
        <MeetingsView
          {openId}
          {openAt}
          onlive={() => go("live")}
          onask={ask}
          onreturn={fromAsk ? () => go("ask") : undefined}
        />
      {/key}
    {:else if view === "ask"}
      <AskView initial={question} onstarted={() => (question = null)} onopen={openMeeting} />
    {:else}
      <SettingsView />
    {/if}
  </main>
</div>

<style>
  .app {
    display: flex;
    height: 100vh;
  }
  nav {
    flex: none;
    width: 15.71rem;
    display: flex;
    flex-direction: column;
    gap: 0.43rem;
    padding: 1.43rem 1rem;
    border-right: 1px solid var(--border);
    background: var(--nav-bg);
    overflow-y: auto;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.71rem;
    padding: 0.29rem 0.57rem 1.29rem;
  }
  .mark {
    flex: none;
    width: 2.14rem;
    height: 2.14rem;
    border-radius: 8px;
    background: var(--primary-bg);
    color: var(--bg);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .name {
    font-family: var(--font-serif);
    font-size: 1.43rem;
    font-weight: 600;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 0.71rem;
    height: 2.86rem;
    padding: 0 0.71rem;
    border: none;
    border-radius: 8px;
    background: none;
    color: var(--fg-2);
    font-weight: 400;
    text-align: left;
    white-space: nowrap;
  }
  .item svg {
    flex: none;
  }
  .item.active,
  .item.active:hover:not(:disabled) {
    background: var(--nav-active);
    color: var(--fg);
    font-weight: 500;
    box-shadow: 0 1px 2px rgb(28 27 25 / 0.06);
  }
  /* New recording and Recording: a full-width button above the items. */
  .new,
  .recording {
    justify-content: center;
    gap: 0.57rem;
    height: 3.14rem;
    margin-bottom: 1rem;
    border-radius: 10px;
    font-weight: 500;
  }
  .new,
  .new:hover:not(:disabled),
  .new.active,
  .new.active:hover:not(:disabled) {
    background: var(--primary-bg);
    color: var(--primary-fg);
  }
  .new:hover:not(:disabled) {
    opacity: 0.88;
  }
  .recording,
  .recording:hover:not(:disabled),
  .recording.active,
  .recording.active:hover:not(:disabled) {
    background: var(--danger-bg);
    color: var(--danger-fg);
    font-weight: 600;
  }
  /* The Live view is shown: ring the button, as the other items get a fill. */
  .new.active,
  .recording.active {
    box-shadow: 0 0 0 2px var(--nav-bg), 0 0 0 4px var(--rec-dot);
  }
  .dot {
    flex: none;
    width: 0.57rem;
    height: 0.57rem;
    border-radius: 50%;
    background: var(--rec-dot);
  }
  .pulse {
    animation: pulse 1.6s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  .version {
    margin-top: auto;
    padding: 0.71rem;
    font-size: 0.86rem;
    color: var(--muted);
  }
  main {
    flex: 1;
    min-width: 0;
    min-height: 0;
  }
  /* Narrow windows: an icon rail, the names left to the tooltips and accessible labels. */
  @media (max-width: 720px) {
    nav {
      width: auto;
      align-items: center;
      padding: 1.14rem 0.57rem;
    }
    .brand {
      padding: 0 0 1rem;
    }
    .name,
    .label,
    .version {
      display: none;
    }
    .item {
      width: 2.86rem;
      justify-content: center;
      padding: 0;
    }
  }
</style>
