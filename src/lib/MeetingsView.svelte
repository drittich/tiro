<script lang="ts" module>
  type Column = "title" | "date" | "status";

  // Kept outside the component so the sort and the search survive switching views.
  let sort = $state<{ column: Column; ascending: boolean }>({ column: "date", ascending: false });
  /** What is typed in the search box: keywords, quoted phrases and `tag:` filters. */
  let query = $state("");
</script>

<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { ask } from "@tauri-apps/plugin-dialog";
  import {
    api,
    EVENTS,
    placeLabel,
    screenshotUrl,
    STATUS_CHIP,
    tagTerm,
    type EmbeddingProgress,
    type Hit,
    type IndexProgress,
    type JobEvent,
    type Meeting,
    type MeetingListing,
    type Place,
    type SearchResults,
  } from "./api";
  import { live } from "./live.svelte";
  import MeetingDetail from "./MeetingDetail.svelte";

  let {
    openId = null,
    openAt: openPlace = null,
    onlive,
    onask,
    onreturn,
  }: {
    /** A Meeting to open straight away (e.g. from the Live view). */
    openId?: string | null;
    /** Where to open `openId`, as at a cited Passage. */
    openAt?: Place | null;
    /** Shows the Live view. */
    onlive?: () => void;
    /** Asks the search box's text as a question, in the Ask view. */
    onask?: (question: string) => void;
    /** Where going back from `openId` leads, when not to the list (e.g. the Ask view). */
    onreturn?: () => void;
  } = $props();

  let meetings = $state<MeetingListing[]>([]);
  let selected = $state<Meeting | null>(null);
  /** Where to open `selected`: the Passage of the search result it was opened from. */
  let openAt = $state<Place | null>(null);
  /** What the search found, and for which query; null while the box is empty. */
  let results = $state<SearchResults | null>(null);
  let resultsFor = $state("");
  let indexing = $state<IndexProgress>({ done: 0, total: 0 });
  let embedding = $state<EmbeddingProgress>({ done: 0, total: 0, problem: null });
  let searchBox: HTMLInputElement | undefined = $state();
  /** Tags offered for the word being typed, and the one picked with the arrow keys (-1 for none). */
  let suggesting = $state(false);
  let suggestion = $state(-1);
  let error = $state<string | null>(null);
  /** The Meeting whose title is being edited in the list, and the edit. */
  let renaming = $state<{ id: string; title: string } | null>(null);
  /** The microphone Tiro records You from, as the Ready card names it. */
  let micName = $state("System default input");
  /** Why asking is off, as the Ask button's tooltip; null when it is on. */
  let askOff = $state<string | null>(null);

  /** The `openId` already opened, so a later refresh leaves the Meeting shown as it is. */
  let opened: string | null = null;

  async function refresh() {
    try {
      meetings = await api.listMeetings();
      if (openId && openId !== opened) {
        selected = meetings.find((m) => m.id === openId) ?? selected;
        openAt = openPlace;
        opened = openId;
      }
    } catch (e) {
      error = String(e);
    }
  }

  async function loadMic() {
    const { mic } = await api.getSettings();
    if (!mic) return;
    micName = mic;
    const microphones = await api.listMicrophones().catch(() => []);
    micName = microphones.find((m) => m.name === mic)?.description ?? mic;
  }

  function changed(meeting: Meeting) {
    if (selected?.id === meeting.id) selected = meeting;
    // A plain Meeting (e.g. from a rename) keeps the card's Screenshot.
    meetings = meetings.map((m) => (m.id === meeting.id ? { ...m, ...meeting } : m));
  }

  // A new Summary may retitle the Meeting and drops the old Tags; tagging it, or tagging at startup, brings new ones.
  function onJobEvent(event: JobEvent) {
    const meeting = meetings.find((m) => m.id === event.meetingId) ?? (selected?.id === event.meetingId ? selected : null);
    if (!meeting) return;
    if (event.kind === "summarized") changed({ ...meeting, title: event.title || meeting.title, tags: null });
    if (event.kind === "tagged") changed({ ...meeting, tags: event.tags });
    // A new Summary is indexed, and new Tags may match the filters.
    if ((event.kind === "summarized" || event.kind === "tagged") && query.trim()) runSearch(query);
  }

  /** Adds this Tag to the search's Tag filters. */
  function filterBy(tag: string) {
    selected = null;
    openAt = null;
    if (!activeTags.some((t) => t.toLowerCase() === tag.toLowerCase())) {
      query = `${query.trim()} ${tagTerm(tag)}`.trim() + " ";
    }
  }

  function open(meeting: Meeting, at: Place | null = null) {
    openAt = at;
    selected = meeting;
  }

  const SEARCH_DEBOUNCE_MS = 200;
  const INDEXED_SEARCH_MS = 1000;
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  /** Separate from the typing's, so indexing never holds back the results of what is typed. */
  let indexedTimer: ReturnType<typeof setTimeout> | undefined;
  /** Bumped by each search, so an older answer arriving late is dropped. */
  let searchSeq = 0;

  // Results follow the typing, debounced. An empty box shows the normal list straight away.
  $effect(() => {
    const q = query;
    clearTimeout(searchTimer);
    if (!q.trim()) {
      searchSeq++;
      results = null;
      return;
    }
    searchTimer = setTimeout(() => runSearch(q), SEARCH_DEBOUNCE_MS);
  });

  async function runSearch(q: string) {
    const seq = ++searchSeq;
    try {
      const found = await api.search(q);
      if (seq !== searchSeq) return;
      results = found;
      resultsFor = q.trim();
      error = null;
    } catch (e) {
      if (seq === searchSeq) error = String(e);
    }
  }

  function clearSearch() {
    query = "";
    suggesting = false;
    searchBox?.focus();
  }

  /** Every Tag in use, most used first, spelled as first seen; matches `tags::vocabulary` in core. */
  const vocabulary = $derived.by(() => {
    const counts = new Map<string, { tag: string; n: number }>();
    for (const tag of meetings.flatMap((m) => m.tags ?? [])) {
      const entry = counts.get(tag.toLowerCase());
      if (entry) entry.n++;
      else counts.set(tag.toLowerCase(), { tag, n: 1 });
    }
    return [...counts.values()].sort((a, b) => b.n - a.n || collator.compare(a.tag, b.tag)).map((e) => e.tag);
  });

  /** The Tag filters the last search read from the query. */
  const activeTags = $derived(results?.tags ?? []);

  /** The word being typed at the end of the box: `tag:` with what follows it, or a bare word of two letters or more. */
  const typing = $derived.by(() => {
    const m = /(?:^|\s)(tag:"[^"]*|tag:\S*|[^\s"]{2,})$/i.exec(query);
    if (!m) return null;
    const token = m[1];
    const isTag = /^tag:/i.test(token);
    return { token, prefix: (isTag ? token.slice(4).replace(/^"/, "") : token).toLowerCase(), isTag };
  });

  const SUGGESTIONS = 6;
  const suggestions = $derived.by(() => {
    if (!suggesting || !typing) return [];
    const { prefix, isTag } = typing;
    const taken = (tag: string) => activeTags.some((t) => t.toLowerCase() === tag.toLowerCase());
    const fits = (tag: string) =>
      isTag ? tag.toLowerCase().includes(prefix) : tag.toLowerCase().split(/\s+/).some((w) => w.startsWith(prefix));
    return vocabulary.filter((t) => !taken(t) && fits(t)).slice(0, SUGGESTIONS);
  });

  /** Replaces the word being typed with a filter for this Tag. */
  function pickTag(tag: string) {
    if (!typing) return;
    query = `${query.slice(0, query.length - typing.token.length)}${tagTerm(tag)} `;
    suggesting = false;
    suggestion = -1;
    searchBox?.focus();
  }

  function onSearchKey(e: KeyboardEvent) {
    if (e.key === "ArrowDown" && suggestions.length) {
      e.preventDefault();
      suggesting = true;
      suggestion = (suggestion + 1) % suggestions.length;
    } else if (e.key === "ArrowUp" && suggestions.length) {
      e.preventDefault();
      suggestion = (suggestion - 1 + suggestions.length) % suggestions.length;
    } else if (e.key === "Enter" && suggestion >= 0 && suggestions[suggestion]) {
      e.preventDefault();
      pickTag(suggestions[suggestion]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      if (suggestions.length) suggesting = false;
      else clearSearch();
    }
  }

  async function saveRename() {
    if (!renaming) return;
    const { id, title } = renaming;
    renaming = null;
    if (title.trim() === meetings.find((m) => m.id === id)?.title) return;
    try {
      error = null;
      changed(await api.setTitle(id, title));
    } catch (e) {
      error = String(e);
    }
  }

  /** Asks first; returns why the Meeting could not be deleted, or null. */
  async function remove(meeting: Meeting): Promise<string | null> {
    const sure = await ask(`Delete “${meeting.title}”? Its Transcript, Summary, Notes and audio are removed for good.`, {
      title: "Delete Meeting",
      kind: "warning",
      okLabel: "Delete",
      cancelLabel: "Keep",
    });
    if (!sure) return null;
    try {
      await api.deleteMeeting(meeting.id);
    } catch (e) {
      return String(e);
    }
    meetings = meetings.filter((m) => m.id !== meeting.id);
    if (results?.meetings) results = { ...results, meetings: results.meetings.filter((r) => r.meetingId !== meeting.id) };
    if (selected?.id === meeting.id) selected = null;
    return null;
  }

  async function removeFromList(meeting: Meeting) {
    error = await remove(meeting);
  }

  const COLUMNS: { id: Column; label: string }[] = [
    { id: "title", label: "Title" },
    { id: "date", label: "Date" },
    { id: "status", label: "Status" },
  ];

  /** The same control reverses the order; another starts in its own direction (Date newest first). */
  function sortBy(column: Column) {
    sort = sort.column === column ? { column, ascending: !sort.ascending } : { column, ascending: column !== "date" };
  }

  const startTs = (m: Meeting) => new Date(m.started_at).getTime();
  const collator = new Intl.Collator(undefined, { sensitivity: "base", numeric: true });
  const hasTag = (m: Meeting, tag: string) => m.tags?.some((t) => t.toLowerCase() === tag.toLowerCase()) ?? false;
  const filtered = $derived(activeTags.length ? meetings.filter((m) => activeTags.every((t) => hasTag(m, t))) : meetings);
  /** The Meetings matching the keywords, best first, with their matching Passages; null without keywords. */
  const ranked = $derived.by(() => {
    if (!results?.meetings) return null;
    const byId = new Map(meetings.map((m) => [m.id, m]));
    return results.meetings.flatMap((r) => {
      const meeting = byId.get(r.meetingId);
      return meeting ? [{ meeting, hits: r.hits }] : [];
    });
  });
  const sorted = $derived.by(() => {
    const { column, ascending } = sort;
    const key = (a: MeetingListing, b: MeetingListing) =>
      column === "title" ? collator.compare(a.title, b.title)
      : column === "status" ? collator.compare(a.status, b.status)
      : startTs(a) - startTs(b);
    // Ties fall back to newest first.
    return [...filtered].sort((a, b) => (ascending ? key(a, b) : key(b, a)) || startTs(b) - startTs(a));
  });

  const dayFormat = new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric" });
  const dayYearFormat = new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric", year: "numeric" });
  const timeFormat = new Intl.DateTimeFormat(undefined, { timeStyle: "short" });
  const dayKey = (d: Date) => `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;

  /** "Today · Tue, Oct 20", "Yesterday · Mon, Oct 19", otherwise the day alone, with the year when it isn't this one. */
  function dayLabel(d: Date): string {
    const now = new Date();
    const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1);
    const day = (d.getFullYear() === now.getFullYear() ? dayFormat : dayYearFormat).format(d);
    if (dayKey(d) === dayKey(now)) return `Today · ${day}`;
    if (dayKey(d) === dayKey(yesterday)) return `Yesterday · ${day}`;
    return day;
  }

  /** Under the Date sort, the sorted Meetings in runs by day; under the others, one group with no heading. */
  const groups = $derived.by(() => {
    if (sort.column !== "date") return [{ key: "all", label: null, meetings: sorted }];
    const runs: { key: string; label: string | null; meetings: MeetingListing[] }[] = [];
    for (const m of sorted) {
      const d = new Date(m.started_at);
      const last = runs.at(-1);
      if (last?.key === dayKey(d)) last.meetings.push(m);
      else runs.push({ key: dayKey(d), label: dayLabel(d), meetings: [m] });
    }
    return runs;
  });

  /** The time under a day heading; the day and time where there are no headings. */
  const started = (m: Meeting) => {
    const d = new Date(m.started_at);
    return sort.column === "date" ? timeFormat.format(d) : `${dayLabel(d)} · ${timeFormat.format(d)}`;
  };

  const status = (m: Meeting) => STATUS_CHIP[m.status];

  const busy = (m: Meeting) => m.status === "recording" || m.status === "transcribing";

  /** A card shows this many Tags, then "+N" for the rest, so they stay on about one line. */
  const SHOWN_TAGS = 3;

  const shotCount = (m: MeetingListing) => `${m.screenshotCount} Screenshot${m.screenshotCount === 1 ? "" : "s"}`;

  $effect(() => {
    void openId;
    refresh();
  });

  onMount(() => {
    loadMic().catch(() => {});
    api.askOff().then((reason) => (askOff = reason)).catch((e) => (askOff = String(e)));
    api.indexingProgress().then((p) => (indexing = p)).catch(() => {});
    api.embeddingProgress().then((p) => (embedding = p)).catch(() => {});
    const unlisten = listen<JobEvent>(EVENTS.job, (e) => onJobEvent(e.payload));
    // Newly indexed or embedded Meetings may match; searched again once that pauses, not after every Meeting.
    const searchAgain = () => {
      if (query.trim()) {
        clearTimeout(indexedTimer);
        indexedTimer = setTimeout(() => runSearch(query), INDEXED_SEARCH_MS);
      }
    };
    const unlistenIndexing = listen<IndexProgress>(EVENTS.indexing, (e) => {
      indexing = e.payload;
      searchAgain();
    });
    const unlistenEmbedding = listen<EmbeddingProgress>(EVENTS.embedding, (e) => {
      embedding = e.payload;
      searchAgain();
    });
    return () => {
      void unlisten.then((f) => f());
      void unlistenIndexing.then((f) => f());
      void unlistenEmbedding.then((f) => f());
    };
  });
</script>

<!-- The Screenshot nearest five minutes in, with the count over it; the document icon when there are none. -->
{#snippet cardIcon(meeting: MeetingListing)}
  {#if meeting.thumbnail}
    <span class="thumb" aria-hidden="true">
      <img src={screenshotUrl(meeting.id, { file: meeting.thumbnail })} alt="" loading="lazy" decoding="async" />
      <span class="count mono">{meeting.screenshotCount}</span>
    </span>
  {:else}
    <span class="icon" aria-hidden="true">
      <svg width="18" height="18" viewBox="0 0 16 16">
        <path d="M4 2.5h6l2.5 2.5v8.5H4z" stroke="currentColor" stroke-width="1.4" fill="none" stroke-linejoin="round" />
        <path d="M6 8h5M6 10.5h3.5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
      </svg>
    </span>
  {/if}
{/snippet}

<!-- A Meeting's card; under a search result, the Passages that matched, each opening the Meeting there. -->
{#snippet card(meeting: MeetingListing, hits?: Hit[])}
  {@const s = status(meeting)}
  <div class="card">
    <div class="row">
      {#if renaming?.id === meeting.id}
        <div class="open">
          {@render cardIcon(meeting)}
          <span class="text">
            <!-- svelte-ignore a11y_autofocus -->
            <input
              class="rename"
              aria-label="Meeting title"
              autofocus
              bind:value={renaming.title}
              onblur={saveRename}
              onkeydown={(e) => {
                if (e.key === "Enter") saveRename();
                if (e.key === "Escape") renaming = null;
              }}
            />
            <span class="when mono">{started(meeting)}</span>
          </span>
          <span class="chip {s.chip}">{s.label}</span>
        </div>
      {:else}
        <button
          class="open"
          aria-describedby={meeting.thumbnail ? `shots-${meeting.id}` : undefined}
          onclick={() => open(meeting)}
        >
          {@render cardIcon(meeting)}
          <span class="text">
            <span class="name">{meeting.title}</span>
            <span class="when mono">{started(meeting)}</span>
          </span>
          <span class="chip {s.chip}">{s.label}</span>
        </button>
        {#if meeting.thumbnail}<span id="shots-{meeting.id}" hidden>{shotCount(meeting)}</span>{/if}
        <span class="actions">
          <button class="link" onclick={() => (renaming = { id: meeting.id, title: meeting.title })}>Rename</button>
          <button class="link delete" onclick={() => removeFromList(meeting)} disabled={busy(meeting)}>Delete</button>
        </span>
      {/if}
    </div>
    {#if meeting.tags?.length}
      {@const hidden = meeting.tags.slice(SHOWN_TAGS)}
      <div class="tags" role="group" aria-label="Tags">
        {#each meeting.tags.slice(0, SHOWN_TAGS) as tag (tag)}
          <button class="tag" title="Search the Meetings tagged {tag}" onclick={() => filterBy(tag)}>{tag}</button>
        {/each}
        {#if hidden.length}<span class="more" title={hidden.join(", ")}>+{hidden.length}</span>{/if}
      </div>
    {/if}
    {#if hits?.length}
      <ul class="hits" aria-label="Matches">
        {#each hits as hit, k (k)}
          <li>
            <button class="hit" onclick={() => open(meeting, hit.place)}>
              <span class="where">{placeLabel(hit.place)}</span>
              <span class="excerpt"
                >{#each hit.snippet as part, n (n)}{#if part.matched}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}</span
              >
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/snippet}

{#if selected}
  <MeetingDetail
    meeting={selected}
    at={openAt}
    onchange={changed}
    ondelete={remove}
    onback={() => {
      if (onreturn && selected?.id === openId) onreturn();
      else selected = null;
    }}
    ontag={filterBy}
  />
{:else}
  <section class="home">
    <div class="page">
      <section class="ready" aria-label={live.recording ? "Recording" : "Start a recording"}>
        <div class="intro">
          {#if live.recording}
            <h1><span class="chip rec">Recording</span> {live.meeting?.title ?? ""}</h1>
            <p>Tiro is recording. Open the Live view for the Transcript so far and your Notes.</p>
          {:else}
            <h1>Ready when you are</h1>
            <p>
              Tiro records you and everything playing through your speakers. Jot Notes as you go — Tiro fills in the
              rest after.
            </p>
            <div class="chips">
              <span class="chip" title="Microphone (You)">
                <svg width="12" height="12" viewBox="0 0 16 16" aria-hidden="true">
                  <rect x="5.5" y="1.5" width="5" height="8" rx="2.5" stroke="currentColor" stroke-width="1.5" fill="none" />
                  <path d="M3 8a5 5 0 0010 0M8 13v2" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" />
                </svg><span class="chip-text">{micName}</span>
              </span>
              <span class="chip warn">Tip: headphones give the cleanest Transcript</span>
            </div>
          {/if}
          {#if live.error}
            <div class="callout problem" role="alert">
              <span>{live.error}</span>
              <button class="link" onclick={() => (live.error = null)}>Dismiss</button>
            </div>
          {/if}
        </div>
        {#if live.recording}
          <button class="primary go" onclick={() => onlive?.()}>Open the Live view</button>
        {:else}
          <button class="record go" onclick={() => live.start()} disabled={live.busy}>
            <span class="white-dot" aria-hidden="true"></span>Start recording
          </button>
        {/if}
      </section>

      <div class="list-head">
        <h2>Meetings</h2>
        {#if meetings.length > 0 && !ranked}
          <div class="segmented sorts" role="group" aria-label="Sort Meetings">
            {#each COLUMNS as c (c.id)}
              <button
                class="sort"
                class:active={sort.column === c.id}
                aria-label={`Sort by ${c.label}`}
                aria-pressed={sort.column === c.id}
                onclick={() => sortBy(c.id)}
              >
                {c.label}{#if sort.column === c.id}<span class="arrow" aria-hidden="true">{sort.ascending ? "▲" : "▼"}</span>{/if}
              </button>
            {/each}
          </div>
        {/if}
      </div>

      {#if meetings.length > 0}
        <div class="search">
          <svg class="glass" width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
            <circle cx="7" cy="7" r="4.5" stroke="currentColor" stroke-width="1.5" fill="none" />
            <path d="M10.5 10.5L14 14" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
          </svg>
          <input
            type="search"
            role="combobox"
            aria-label="Search Meetings"
            aria-autocomplete="list"
            aria-expanded={suggestions.length > 0}
            aria-controls="tag-suggestions"
            aria-activedescendant={suggestion >= 0 && suggestions.length ? `tag-suggestion-${suggestion}` : undefined}
            placeholder={'Search keywords, "exact phrases" or tag:Name'}
            spellcheck="false"
            bind:this={searchBox}
            bind:value={query}
            oninput={() => {
              suggesting = true;
              suggestion = -1;
            }}
            onkeydown={onSearchKey}
            onblur={() => (suggesting = false)}
          />
          {#if query}
            <button class="clear" aria-label="Clear the search" title="Show every Meeting" onclick={clearSearch}>×</button>
          {/if}
          {#if suggestions.length}
            <ul class="suggestions" id="tag-suggestions" role="listbox" aria-label="Tags">
              {#each suggestions as tag, k (tag)}
                <li
                  id="tag-suggestion-{k}"
                  role="option"
                  aria-selected={k === suggestion}
                  class:active={k === suggestion}
                  onmousedown={(e) => {
                    e.preventDefault();
                    pickTag(tag);
                  }}
                >
                  <span class="tag">{tag}</span>
                </li>
              {/each}
            </ul>
          {/if}
          <button
            class="ask-button"
            onclick={() => onask?.(query.trim())}
            disabled={!!askOff || !query.trim()}
            title={askOff ?? "Ask this as a question, answered from the Meetings"}
          >
            Ask
          </button>
        </div>
        {#if indexing.done < indexing.total}
          <p class="indexing" role="status">Indexing {indexing.done} of {indexing.total} Meetings for Search…</p>
        {:else if embedding.done < embedding.total}
          <p class="indexing" role="status">Embedding {embedding.done} of {embedding.total} Meetings for Search by meaning…</p>
        {:else if embedding.problem}
          <p class="indexing" role="status">Search by meaning stopped: {embedding.problem}</p>
        {/if}
        {#if results?.meaning === "off"}
          <p class="indexing" role="status">
            Search by meaning is off: set an API key and an Embedding model in Settings. Keywords and Tags still work.
          </p>
        {:else if results?.meaning === "unavailable"}
          <p class="indexing" role="status">Search by meaning is unavailable right now, so these results match keywords only.</p>
        {/if}
      {/if}

      {#if error}<p class="callout problem" role="alert">{error}</p>{/if}
      {#if meetings.length === 0}
        <p class="empty">No Meetings recorded yet.</p>
      {:else if ranked}
        {#if ranked.length === 0}
          <p class="empty">No Meetings match “{resultsFor}”.</p>
        {:else}
          <section class="group" aria-label="Search results">
            {#each ranked as result (result.meeting.id)}
              {@render card(result.meeting, result.hits)}
            {/each}
          </section>
        {/if}
      {:else if filtered.length === 0}
        <p class="empty">No Meetings are tagged {activeTags.join(" and ")}.</p>
      {:else}
        {#each groups as group (group.key)}
          <section class="group" aria-label={group.label ?? "Meetings"}>
            {#if group.label}<h3>{group.label}</h3>{/if}
            {#each group.meetings as meeting (meeting.id)}
              {@render card(meeting)}
            {/each}
          </section>
        {/each}
      {/if}
    </div>
  </section>
{/if}

<style>
  .home {
    height: 100%;
    overflow-y: auto;
    padding: 2.86rem 3.43rem;
  }
  .page {
    max-width: 61.43rem;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 1.71rem;
  }
  .ready {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 1.43rem;
    padding: 1.71rem 2rem;
    border: 1px solid var(--border);
    border-radius: 16px;
    background: var(--surface);
  }
  .intro {
    flex: 1 1 22.86rem;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.43rem;
  }
  h1 {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.71rem;
    margin: 0;
    font-size: 1.86rem;
    overflow-wrap: anywhere;
  }
  h1 .chip {
    font-family: var(--text-font, "Inter"), "Inter", sans-serif;
  }
  .intro p {
    margin: 0;
    color: var(--fg-2);
    line-height: 1.5;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 0.57rem;
    margin-top: 0.57rem;
  }
  .chips .chip {
    max-width: 100%;
  }
  .chip-text {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .intro .callout {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    margin-top: 0.57rem;
  }
  .go {
    display: inline-flex;
    align-items: center;
    gap: 0.71rem;
    height: 3.43rem;
    padding: 0 1.57rem;
    border-radius: 12px;
    font-size: 1.07rem;
  }
  .white-dot {
    width: 0.71rem;
    height: 0.71rem;
    border-radius: 50%;
    background: #fff;
  }
  .list-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.86rem;
    margin-top: 0.57rem;
  }
  h2 {
    margin: 0;
    font-size: 1.57rem;
  }
  /* Smaller than the global segmented control. */
  .sorts {
    gap: 2px;
    padding: 3px;
    border-radius: 10px;
  }
  .sorts .sort {
    height: auto;
    padding: 0.36rem 0.86rem;
    border-radius: 8px;
    color: var(--muted);
    font-size: 0.93rem;
  }
  .arrow {
    margin-left: 6px;
    font-size: 0.7em;
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 0.57rem;
  }
  h3 {
    margin: 0 0 0.29rem;
    font-size: 0.86rem;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .card {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: 12px;
    background: var(--surface);
  }
  .card:hover {
    border-color: var(--input-border);
  }
  .row {
    position: relative;
    display: flex;
    align-items: center;
  }
  /* Under the title, lined up with it, so they never take its room. */
  .tags {
    --indent: calc(1.29rem + 4.57rem + 1.14rem);
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.43rem;
    margin-top: -0.57rem;
    padding: 0 1.29rem 1rem var(--indent);
  }
  .more {
    color: var(--muted);
    font-size: 0.79rem;
  }
  .search {
    position: relative;
    display: flex;
    align-items: center;
    margin-top: -0.86rem;
  }
  .search input {
    flex: 1;
    min-width: 0;
    height: 2.86rem;
    padding: 0 2.57rem 0 2.57rem;
    border-radius: 12px;
  }
  /* The box has its own clear button. */
  .search input::-webkit-search-cancel-button {
    display: none;
  }
  .glass {
    position: absolute;
    left: 0.93rem;
    color: var(--muted);
    pointer-events: none;
  }
  .search .clear {
    position: absolute;
    right: calc(0.71rem + var(--ask-width) + 0.57rem);
    color: var(--muted);
  }
  .search {
    --ask-width: 4.29rem;
    gap: 0.57rem;
  }
  .ask-button {
    flex: none;
    width: var(--ask-width);
    height: 2.86rem;
    border-radius: 12px;
  }
  .indexing {
    margin: -1.14rem 0 0 0.29rem;
    color: var(--muted);
    font-size: 0.86rem;
  }
  .suggestions {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: 10;
    min-width: 16rem;
    margin: 0;
    padding: 0.29rem;
    list-style: none;
    border: 1px solid var(--border);
    border-radius: 12px;
    background: var(--surface);
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.12);
  }
  .suggestions li {
    padding: 0.43rem 0.57rem;
    border-radius: 8px;
    cursor: pointer;
  }
  .suggestions li:hover,
  .suggestions li.active {
    background: var(--hover);
  }
  /* The Passages that matched, under the title like the Tags. */
  .hits {
    --indent: calc(1.29rem + 4.57rem + 1.14rem);
    display: flex;
    flex-direction: column;
    gap: 0.14rem;
    margin: -0.29rem 0 0;
    padding: 0 0.71rem 0.71rem calc(var(--indent) - 0.57rem);
    list-style: none;
  }
  .hit {
    display: flex;
    flex-direction: column;
    gap: 0.14rem;
    width: 100%;
    height: auto;
    padding: 0.43rem 0.57rem;
    border: none;
    border-radius: 8px;
    background: none;
    color: inherit;
    font-weight: 400;
    text-align: left;
  }
  .hit:hover:not(:disabled) {
    background: var(--hover);
  }
  .where {
    color: var(--muted);
    font-size: 0.79rem;
  }
  .excerpt {
    color: var(--fg-2);
    font-size: 0.93rem;
    line-height: 1.45;
  }
  .excerpt mark {
    border-radius: 3px;
    background: var(--accent-bg);
    color: var(--accent);
    font-weight: 600;
  }
  .clear {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1.43rem;
    height: 1.43rem;
    padding: 0;
    border: none;
    border-radius: 999px;
    background: none;
    color: inherit;
    font-size: 1rem;
    line-height: 1;
  }
  .open {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 1.14rem;
    padding: 1.14rem 1.29rem;
    border: none;
    border-radius: 12px;
    background: none;
    color: inherit;
    font-weight: 400;
    text-align: left;
  }
  .open:hover:not(:disabled) {
    background: none;
  }
  /* The same box as a thumbnail, so every card's title lines up. */
  .icon {
    flex: none;
    width: 4.57rem;
    height: 2.86rem;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--accent-bg);
    color: var(--accent);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  /* 16:10 like the Screenshots gallery, at the document icon's height. */
  .thumb {
    position: relative;
    flex: none;
    width: 4.57rem;
    height: 2.86rem;
    border: 1px solid var(--border);
    border-radius: 10px;
    overflow: hidden;
    background: var(--hover);
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    object-position: top;
    display: block;
  }
  /* Opaque, as the gallery's times are, so it reads over any Screenshot in either theme. */
  .count {
    position: absolute;
    right: 0.21rem;
    bottom: 0.21rem;
    min-width: 1.14rem;
    padding: 0 0.29rem;
    border-radius: 5px;
    background: var(--surface);
    color: var(--fg-2);
    font-size: 0.71rem;
    line-height: 1.14rem;
    text-align: center;
    font-variant-numeric: tabular-nums;
  }
  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.21rem;
  }
  .name {
    font-weight: 600;
    font-size: 1.07rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .when {
    color: var(--muted);
    font-size: 0.86rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rename {
    min-width: 0;
    padding: 0.29rem 0.57rem;
    font-weight: 600;
  }
  /* Over the status chip while the card is hovered or focused, so they take no room from the title. */
  .actions {
    position: absolute;
    top: 50%;
    right: 1.29rem;
    transform: translateY(-50%);
    display: flex;
    gap: 0.86rem;
    padding: 0.43rem 0 0.43rem 0.86rem;
    background: var(--surface);
    visibility: hidden;
  }
  .card:hover .actions,
  .card:focus-within .actions {
    visibility: visible;
  }
  .delete:not(:disabled) {
    color: var(--danger);
  }
  .empty {
    margin: 0;
    padding: 2.29rem 1.43rem;
    border: 1px dashed var(--border);
    border-radius: 12px;
    color: var(--muted);
    text-align: center;
  }
  @media (max-width: 720px) {
    .home {
      padding: 1.43rem 1.14rem;
    }
    .ready {
      padding: 1.29rem 1.43rem;
    }
    .open {
      gap: 0.86rem;
      padding: 0.86rem 1rem;
    }
    /* The thumbnail stays, smaller: it is how a card is told apart, and the title still ellipsizes. */
    .icon,
    .thumb {
      width: 3.43rem;
      height: 2.14rem;
      border-radius: 8px;
    }
    .actions {
      right: 1rem;
    }
    .tags {
      --indent: calc(1rem + 3.43rem + 0.86rem);
      padding-right: 1rem;
      padding-bottom: 0.86rem;
    }
    .hits {
      --indent: 1rem;
      padding-left: 0.43rem;
    }
  }
</style>
