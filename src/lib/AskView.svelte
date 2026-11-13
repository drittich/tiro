<script lang="ts" module>
  import type { Source } from "./api";

  /** A question and its answer as the view shows it. */
  interface Exchange {
    question: string;
    /** The answer so far, its citations marked. */
    text: string;
    /** The Passages it cites, once it is done. */
    sources: Source[];
    /** Every Passage it may cite, from before its text comes, so citations work while it streams. */
    inView: Source[];
    state: "answering" | "done" | "failed";
    error?: string;
  }

  // Kept outside the component so the conversation survives switching views; it lasts until a new one is started
  // or Tiro quits, as the backend's does.
  let exchanges = $state<Exchange[]>([]);
</script>

<script lang="ts">
  import { onMount, tick } from "svelte";
  import { api, CITE_CLOSE, CITE_OPEN, sourceLabel, type Place } from "./api";
  import { escape, renderMarkdown } from "./markdown";

  let {
    initial = null,
    onstarted,
    onopen,
  }: {
    /** A question to start a new conversation with, as from the search box's Ask button. */
    initial?: string | null;
    /** `initial` has been asked. */
    onstarted?: () => void;
    /** Opens a Meeting at a cited Passage. */
    onopen: (meetingId: string, at: Place) => void;
  } = $props();

  let draft = $state("");
  /** Why asking is off; null when it is on, undefined until known. */
  let off = $state<string | null | undefined>(undefined);
  let scroller: HTMLElement | undefined = $state();
  let box: HTMLTextAreaElement | undefined = $state();

  const busy = $derived(exchanges.some((e) => e.state === "answering"));
  /** Every Passage an answer could cite, by number, to label and open citations. */
  const cited = $derived(new Map(exchanges.flatMap((e) => [...e.inView, ...e.sources]).map((s) => [s.number, s])));

  const CITATION = new RegExp(`${CITE_OPEN}(\\d+)${CITE_CLOSE}`, "g");

  /** The answer as HTML, each citation a small numbered button labelled with its Passage. */
  function answerHtml(text: string): string {
    return renderMarkdown(text).replace(CITATION, (_, n: string) => {
      const source = cited.get(Number(n));
      const label = source ? sourceLabel(source) : `Passage ${n}`;
      return `<button type="button" class="cite" data-n="${n}" title="${escape(label)}" aria-label="${escape(label)}">${n}</button>`;
    });
  }

  function openCitation(event: MouseEvent) {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button.cite");
    const source = button && cited.get(Number(button.dataset.n));
    if (source) onopen(source.meetingId, source.place);
  }

  async function scrollDown() {
    await tick();
    scroller?.scrollTo({ top: scroller.scrollHeight });
  }

  async function send(question: string) {
    question = question.trim();
    if (!question || busy || off) return;
    draft = "";
    exchanges.push({ question, text: "", sources: [], inView: [], state: "answering" });
    const exchange = exchanges[exchanges.length - 1];
    const startedIn = exchanges;
    void scrollDown();
    try {
      const answer = await api.ask(question, (event) => {
        if (exchanges !== startedIn) return;
        if (event.kind === "passages") exchange.inView = event.sources;
        else exchange.text = event.text;
      });
      exchange.text = answer.text;
      exchange.sources = answer.sources;
      exchange.state = "done";
    } catch (e) {
      exchange.state = "failed";
      exchange.error = String(e);
    }
    void scrollDown();
    box?.focus();
  }

  async function newConversation() {
    await api.newConversation();
    exchanges = [];
    draft = "";
    box?.focus();
  }

  /** Asks a failed question again, in its place. */
  function retry(exchange: Exchange) {
    exchanges = exchanges.filter((e) => e !== exchange);
    void send(exchange.question);
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      void send(draft);
    }
  }

  onMount(async () => {
    off = await api.askOff().catch((e) => String(e));
    if (initial?.trim() && !off) {
      if (exchanges.length) await newConversation();
      void send(initial);
    } else if (initial?.trim()) {
      draft = initial;
    }
    onstarted?.();
    void scrollDown();
    box?.focus();
  });
</script>

<section class="ask">
  <header>
    <h1>Ask</h1>
    <button class="link" onclick={newConversation} disabled={busy || exchanges.length === 0}>New conversation</button>
  </header>

  <div class="scroll" bind:this={scroller}>
    <div class="page">
      {#if off}
        <p class="callout warn" role="status">{off} Search still works.</p>
      {/if}
      {#if exchanges.length === 0}
        <div class="intro">
          <p>Ask a question about your Meetings, like “What did we decide about MCP permissions?”</p>
          <p>
            Answers come from the Meetings' Passages Search finds for the question, with links to each one. Add
            <code>tag:Name</code> to draw only on Meetings with that Tag. The conversation isn't kept: a new one, or
            quitting Tiro, discards it.
          </p>
        </div>
      {/if}
      {#each exchanges as exchange, k (k)}
        <article class="exchange">
          <p class="question">{exchange.question}</p>
          {#if exchange.state === "failed"}
            <div class="callout problem" role="alert">
              <span>{exchange.error}</span>
              <button class="link" onclick={() => retry(exchange)} disabled={busy}>Try again</button>
            </div>
          {:else if exchange.text}
            <!-- Citations are buttons, so the keyboard reaches them; this only hears their clicks. -->
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="markdown answer" aria-live={exchange.state === "answering" ? "polite" : undefined} onclick={openCitation}>
              {@html answerHtml(exchange.text)}
            </div>
          {:else}
            <p class="working" role="status">
              <span class="dots" aria-hidden="true"><span></span><span></span><span></span></span>Reading the Meetings…
            </p>
          {/if}
          {#if exchange.sources.length}
            <ol class="sources" aria-label="Sources">
              {#each exchange.sources as source (source.number)}
                <li>
                  <button class="source" onclick={() => onopen(source.meetingId, source.place)}>
                    <span class="n mono">{source.number}</span>{sourceLabel(source)}
                  </button>
                </li>
              {/each}
            </ol>
          {/if}
        </article>
      {/each}
    </div>
  </div>

  <form
    class="composer"
    onsubmit={(e) => {
      e.preventDefault();
      void send(draft);
    }}
  >
    <textarea
      bind:this={box}
      bind:value={draft}
      rows="2"
      aria-label="Question"
      placeholder={exchanges.length ? "Ask a follow-up…" : "Ask about your Meetings…"}
      disabled={!!off}
      onkeydown={onKey}
    ></textarea>
    <button class="primary" type="submit" disabled={!!off || busy || !draft.trim()} title={off ?? undefined}>Ask</button>
  </form>
</section>

<style>
  .ask {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  header {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    padding: 1.71rem 3.43rem 1rem;
    border-bottom: 1px solid var(--border);
  }
  h1 {
    margin: 0;
    font-size: 1.86rem;
  }
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 1.71rem 3.43rem;
  }
  .page {
    max-width: 50rem;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 2rem;
  }
  .intro p {
    margin: 0 0 0.71rem;
    color: var(--fg-2);
    line-height: 1.6;
  }
  .intro code {
    font-family: var(--font-mono);
    font-size: 0.9em;
  }
  .exchange {
    display: flex;
    flex-direction: column;
    gap: 0.57rem;
  }
  .question {
    align-self: flex-end;
    max-width: 85%;
    margin: 0;
    padding: 0.71rem 1rem;
    border-radius: 14px 14px 4px 14px;
    background: var(--accent-bg);
    color: var(--fg);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .answer :global(p:first-child) {
    margin-top: 0;
  }
  .answer :global(button.cite) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 1.36rem;
    height: 1.21rem;
    margin: 0 0.07rem;
    padding: 0 0.29rem;
    border: none;
    border-radius: 6px;
    background: var(--accent-bg);
    color: var(--accent);
    font-size: 0.75rem;
    font-weight: 600;
    vertical-align: 0.15em;
  }
  .answer :global(button.cite:hover) {
    text-decoration: underline;
  }
  .working {
    display: flex;
    align-items: center;
    gap: 0.57rem;
    margin: 0;
    color: var(--muted);
  }
  .callout {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    margin: 0;
  }
  .sources {
    display: flex;
    flex-direction: column;
    gap: 0.14rem;
    margin: 0;
    padding: 0.57rem 0 0;
    border-top: 1px solid var(--border);
    list-style: none;
  }
  .source {
    display: flex;
    align-items: baseline;
    gap: 0.57rem;
    width: 100%;
    height: auto;
    padding: 0.29rem 0.43rem;
    border: none;
    border-radius: 8px;
    background: none;
    color: var(--fg-2);
    font-size: 0.86rem;
    font-weight: 400;
    text-align: left;
  }
  .source:hover:not(:disabled) {
    background: var(--hover);
  }
  .n {
    color: var(--accent);
    font-weight: 600;
  }
  .composer {
    flex: none;
    display: flex;
    align-items: flex-end;
    gap: 0.71rem;
    padding: 1rem 3.43rem 1.43rem;
    border-top: 1px solid var(--border);
  }
  .composer textarea {
    flex: 1;
    min-width: 0;
    resize: none;
  }
  .composer button {
    height: 2.86rem;
    padding: 0 1.43rem;
  }
  @media (max-width: 720px) {
    header,
    .scroll,
    .composer {
      padding-left: 1.14rem;
      padding-right: 1.14rem;
    }
  }
</style>
