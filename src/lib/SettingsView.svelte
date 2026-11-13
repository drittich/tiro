<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { ask } from "@tauri-apps/plugin-dialog";
  import { api, EVENTS, formatUsd, type Microphone, type SettingsView, type SpendSummary, type Theme } from "./api";
  import { applyAppearance, BUNDLED_FONTS } from "./appearance";

  // ZDR-eligible models from the spike (issue 01). Any OpenRouter transcription model ID can be typed in.
  const KNOWN_MODELS = ["elevenlabs/scribe-v2", "openai/whisper-large-v3-turbo", "deepgram/nova-3", "assemblyai/universal-3-6-pro"];

  let view = $state<SettingsView | null>(null);
  let sttModel = $state("");
  let mic = $state("");
  let microphones = $state<Microphone[]>([]);
  let apiKey = $state("");
  let displayName = $state("");
  let summaryModel = $state("");
  let embeddingModel = $state("");
  let askModel = $state("");
  let summaryPrompt = $state("");
  let autostart = $state(false);
  let meetingScreenshots = $state(false);
  let screenshotsProblem = $state<string | null>(null);
  let textFont = $state("");
  let textSize = $state(15);
  let notesFont = $state("");
  let notesSize = $state(14);
  let theme = $state<Theme>("system");
  let fonts = $state<string[]>([]);
  let message = $state<{ ok: boolean; text: string } | null>(null);
  let spend = $state<SpendSummary | null>(null);

  // Everything the Save button writes, so the save bar can tell when something has changed.
  const snapshot = () =>
    JSON.stringify([
      sttModel, mic, displayName, summaryModel, embeddingModel, askModel, summaryPrompt, autostart, textFont, textSize, notesFont, notesSize, theme,
    ]);
  let saved = $state("");
  const dirty = $derived(view !== null && (apiKey !== "" || snapshot() !== saved));
  const customPrompt = $derived(view !== null && summaryPrompt !== view.defaultSummaryPrompt);

  $effect(() => {
    api.getSettings().then((v) => {
      view = v;
      sttModel = v.sttModel;
      mic = v.mic ?? "";
      displayName = v.displayName;
      summaryModel = v.summaryModel;
      embeddingModel = v.embeddingModel;
      askModel = v.askModel;
      summaryPrompt = v.summaryPrompt ?? v.defaultSummaryPrompt;
      autostart = v.autostart;
      meetingScreenshots = v.meetingScreenshots;
      ({ textFont, textSize, notesFont, notesSize, theme } = v);
      saved = snapshot();
    });
    api
      .listFonts()
      .catch(() => [])
      .then((installed) => (fonts = [...new Set([...BUNDLED_FONTS, ...installed])].sort((a, b) => a.localeCompare(b))));
    api.listMicrophones().then((m) => (microphones = m)).catch(() => (microphones = []));
  });

  const loadSpend = () => api.spendSummary().then((s) => (spend = s));

  // Kept current while a Meeting records or summarizes in the background.
  $effect(() => {
    loadSpend();
    const unlisten = listen(EVENTS.spend, loadSpend);
    return () => void unlisten.then((f) => f());
  });

  const monthName = (year: number, month: number) =>
    new Date(year, month - 1).toLocaleDateString(undefined, { month: "long", year: "numeric" });
  const isThisMonth = (year: number, month: number) => {
    const now = new Date();
    return year === now.getFullYear() && month === now.getMonth() + 1;
  };

  // Keep a saved microphone selectable even while it is unplugged.
  const missingMic = $derived(mic !== "" && !microphones.some((m) => m.name === mic));

  /** Saved at once, apart from the Save button. Turning it on needs the user's consent first. */
  async function toggleScreenshots(event: Event) {
    const checkbox = event.currentTarget as HTMLInputElement;
    const enabled = checkbox.checked;
    checkbox.checked = meetingScreenshots;
    screenshotsProblem = null;
    try {
      if (enabled) {
        const accepted = await ask(
          "Tiro will save a screenshot of the meeting app's window whenever its content changes, while a Meeting is recording.\n\n" +
            "Anything drawn in that window is captured: shared screens, video, chat, and notifications the app shows itself. " +
            "Other windows that cover it are not captured.\n\n" +
            "Screenshots are stored in the Meeting folder alongside the Transcript, and are deleted with the Meeting.",
          { title: "Capture meeting screenshots?", kind: "warning", okLabel: "Capture screenshots", cancelLabel: "Don't capture" },
        );
        if (!accepted) return;
      }
      meetingScreenshots = (await api.setMeetingScreenshots(enabled)).meetingScreenshots;
    } catch (e) {
      screenshotsProblem = String(e);
    }
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    message = null;
    try {
      await api.saveSettings(sttModel, mic || null, apiKey || null);
      await api.saveSummarySettings(summaryModel, summaryPrompt, displayName, embeddingModel, askModel);
      await api.setAutostart(autostart);
      view = await api.saveAppearance({ textFont, textSize, notesFont, notesSize, theme });
      ({ textFont, notesFont } = view);
      applyAppearance(view);
      apiKey = "";
      saved = snapshot();
      message = { ok: true, text: "Saved. Fonts and the theme apply now; other changes apply to the next recording and the next Summary." };
    } catch (e) {
      message = { ok: false, text: String(e) };
    }
  }
</script>

<form class="settings" onsubmit={save}>
  <div class="scroll">
    <div class="page">
      <h1>Settings</h1>

      <section class="card" aria-labelledby="s-recording">
        <header>
          <h2 id="s-recording">Recording</h2>
          <p>What Tiro listens to, and when it runs.</p>
        </header>
        <div class="field">
          <label for="mic">Microphone (You)</label>
          <select id="mic" bind:value={mic}>
            <option value="">System default input</option>
            {#each microphones as m (m.name)}<option value={m.name}>{m.description}</option>{/each}
            {#if missingMic}<option value={mic}>{mic} (not connected)</option>{/if}
          </select>
          <p class="callout warn tip">
            The Others Track always records what plays through the default output. Use headphones if you can: without
            them the mic hears the remote voices too. Tiro echo-cancels the mic when PipeWire allows it and drops You
            words that repeat Others, but headphones give the cleanest Transcript.
          </p>
        </div>

        <div class="checks">
          <label class="check">
            <span class="text">
              <span class="label">Start Tiro when I log in</span>
              <span class="hint">Starts in the tray. Adds or removes <code>~/.config/autostart/tiro.desktop</code>.</span>
            </span>
            <input type="checkbox" bind:checked={autostart} />
          </label>

          <div class="check-row">
            <label class="check">
              <span class="text">
                <span class="label">Capture screenshots of the meeting window</span>
                <span class="hint">Saved as soon as you change it. Needs KWin authorization, which the installed RPM provides.</span>
              </span>
              <input type="checkbox" checked={meetingScreenshots} onchange={toggleScreenshots} />
            </label>
            {#if screenshotsProblem}<p class="problem">{screenshotsProblem}</p>{/if}
          </div>
        </div>
      </section>

      <section class="card" aria-labelledby="s-openrouter">
        <header>
          <h2 id="s-openrouter">OpenRouter</h2>
          <p>Transcription and Summaries run through your account. Only zero-data-retention models work with it.</p>
        </header>
        <div class="field">
          <label for="key">API key</label>
          <input id="key" type="password" bind:value={apiKey} placeholder={view?.keyProblem ? "sk-or-…" : "•••••••• (set — type to replace)"} autocomplete="off" />
          {#if view}
            {#if view.keyProblem}<p class="problem">{view.keyProblem}</p>{/if}
            <p class="hint">Stored only in <code>{view.keyPath}</code> with mode 0600.</p>
          {/if}
        </div>

        <div class="pair">
          <div class="field">
            <label for="model">Speech-to-text model</label>
            <input id="model" class="mono" list="models" bind:value={sttModel} autocomplete="off" spellcheck="false" />
            <datalist id="models">
              {#each KNOWN_MODELS as model}<option value={model}></option>{/each}
            </datalist>
          </div>

          <div class="field">
            <label for="summary-model">Summary model</label>
            <input id="summary-model" class="mono" bind:value={summaryModel} autocomplete="off" spellcheck="false" />
            <p class="hint">Any OpenRouter chat model ID.</p>
          </div>
        </div>

        <div class="field">
          <label for="embedding-model">Embedding model</label>
          <input id="embedding-model" class="mono" bind:value={embeddingModel} autocomplete="off" spellcheck="false" />
          <p class="hint">
            Any OpenRouter embedding model ID, for Search by meaning. Leave it blank to search by keywords and Tags only.
            A new model embeds every Meeting again.
          </p>
        </div>

        <div class="field">
          <label for="ask-model">Ask model</label>
          <input id="ask-model" class="mono" bind:value={askModel} autocomplete="off" spellcheck="false" />
          <p class="hint">Any OpenRouter chat model ID, to answer questions asked of the Meetings. Leave it blank to turn Ask off.</p>
        </div>
      </section>

      <section class="card" aria-labelledby="s-spend">
        <header>
          <h2 id="s-spend">Spend</h2>
          <p>What OpenRouter has charged for Tiro's requests, deleted Meetings included.</p>
        </header>
        {#if spend && spend.months.length}
          {@const first = spend.months[spend.months.length - 1]}
          <dl class="months">
            {#each spend.months as m (`${m.year}-${m.month}`)}
              <div>
                <dt>{monthName(m.year, m.month)}{#if isThisMonth(m.year, m.month)}<span class="so-far">so far</span>{/if}</dt>
                <dd>{formatUsd(m.total)}</dd>
              </div>
            {/each}
            <div class="total">
              <dt>Total since {monthName(first.year, first.month)}</dt>
              <dd>{formatUsd(spend.total)}</dd>
            </div>
          </dl>
        {:else if spend}
          <p class="hint">Spend is recorded from now on. Nothing has been spent yet.</p>
        {/if}
      </section>

      <section class="card" aria-labelledby="s-summaries">
        <header>
          <h2 id="s-summaries">Summaries</h2>
          <p>How a Meeting is written up once it ends.</p>
        </header>
        <div class="field">
          <label for="name">Your name</label>
          <input id="name" bind:value={displayName} placeholder="You" autocomplete="off" />
          <p class="hint">Replaces “You” in Summaries and exports, and names you as the owner of your action items.</p>
        </div>

        <details class="field" open={customPrompt}>
          <summary>
            Summary prompt <span class="state">{customPrompt ? "Customized" : "Default"}</span>
          </summary>
          <textarea id="summary-prompt" aria-label="Summary prompt" bind:value={summaryPrompt} rows="14" spellcheck="false"></textarea>
          <p class="hint">
            Placeholders: <code>{"{{title}}"}</code>, <code>{"{{date}}"}</code>, <code>{"{{duration}}"}</code>,
            <code>{"{{display_name}}"}</code>, <code>{"{{length}}"}</code> (target words), <code>{"{{notes}}"}</code>,
            <code>{"{{transcript}}"}</code>. Keep the <code>Title:</code> line so the Meeting gets a title.
          </p>
          {#if customPrompt}
            <button type="button" class="link" onclick={() => view && (summaryPrompt = view.defaultSummaryPrompt)}>Reset to default</button>
          {/if}
        </details>
      </section>

      <section class="card" aria-labelledby="s-appearance">
        <header>
          <h2 id="s-appearance">Appearance</h2>
          <p>Leave a font blank for the default.</p>
        </header>
        <div class="field">
          <label for="theme">Theme</label>
          <select id="theme" bind:value={theme}>
            <option value="system">Follow system</option>
            <option value="light">Light</option>
            <option value="dark">Dark</option>
          </select>
        </div>

        <div class="field">
          <label for="text-font">Text font</label>
          <div class="font">
            <input id="text-font" list="fonts" bind:value={textFont} autocomplete="off" spellcheck="false" />
            <input type="number" aria-label="Text size (px)" min="8" max="32" bind:value={textSize} />
            <span>px</span>
          </div>
          <p class="hint">Used for Summaries, the Transcript and the rest of the app. Other text sizes scale with it.</p>
        </div>

        <div class="field">
          <label for="notes-font">Notes font</label>
          <div class="font">
            <input id="notes-font" list="fonts" bind:value={notesFont} autocomplete="off" spellcheck="false" />
            <input type="number" aria-label="Notes size (px)" min="8" max="32" bind:value={notesSize} />
            <span>px</span>
          </div>
          <p class="hint">Used while editing Notes; the Preview uses the text font.</p>
        </div>
        <datalist id="fonts">
          {#each fonts as font}<option value={font}></option>{/each}
        </datalist>
      </section>
    </div>
  </div>

  <footer>
    <div class="save">
      {#if message && !(message.ok && dirty)}
        <span class={message.ok ? "ok" : "problem"} role={message.ok ? "status" : "alert"}>{message.text}</span>
      {:else if dirty}
        <span class="pending">Unsaved changes</span>
      {/if}
      <button class="primary" type="submit" disabled={!dirty}>Save</button>
    </div>
  </footer>
</form>

<style>
  .settings {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 2.86rem 3.43rem 3.43rem;
  }
  .page {
    max-width: 51.43rem;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 2rem;
  }
  h1 {
    margin: 0;
    font-size: 2.14rem;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 1.29rem;
    padding: 1.71rem;
    border: 1px solid var(--border);
    border-radius: 16px;
    background: var(--surface);
  }
  header h2 {
    margin: 0 0 0.29rem;
    font-size: 1.14rem;
  }
  header p {
    margin: 0;
    color: var(--muted);
    line-height: 1.5;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.43rem;
    min-width: 0;
  }
  .field > label,
  .label,
  summary {
    font-weight: 500;
  }
  .field input:not([type="checkbox"]),
  .field select {
    height: 3.14rem;
    padding-block: 0;
    min-width: 0;
  }
  input.mono {
    font-size: 0.93rem;
  }
  .tip {
    margin: 0.29rem 0 0;
    font-size: 0.93rem;
  }
  /* Two models side by side, stacked when narrow. */
  .pair {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
    gap: 1.14rem;
  }
  .checks {
    display: flex;
    flex-direction: column;
    margin: -0.29rem 0;
  }
  .checks > * + * {
    border-top: 1px solid var(--border);
  }
  /* The label and its description on the left, the checkbox on the right. */
  .check {
    display: flex;
    align-items: center;
    gap: 1.14rem;
    padding: 1.14rem 0;
    cursor: pointer;
  }
  .check .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.21rem;
  }
  .check input {
    flex: none;
    margin: 0;
    width: 1.43rem;
    height: 1.43rem;
  }
  .check-row .problem {
    margin: -0.57rem 0 1.14rem;
  }
  .months {
    display: flex;
    flex-direction: column;
    margin: -0.29rem 0;
  }
  .months div {
    display: flex;
    justify-content: space-between;
    gap: 1.14rem;
    padding: 0.71rem 0;
  }
  .months div + div {
    border-top: 1px solid var(--border);
  }
  .months dt,
  .months dd {
    margin: 0;
  }
  .months dd {
    font-variant-numeric: tabular-nums;
  }
  .months .so-far {
    margin-left: 0.3em;
    color: var(--muted);
  }
  .months .total {
    font-weight: 600;
  }
  .hint {
    margin: 0;
    color: var(--muted);
    font-size: 0.93rem;
    line-height: 1.5;
  }
  .problem {
    margin: 0;
    color: var(--danger);
  }
  .ok {
    color: var(--ok);
  }
  .pending {
    color: var(--muted);
  }
  .font {
    display: flex;
    align-items: center;
    gap: 0.57rem;
  }
  .font input:first-child {
    flex: 1;
    min-width: 0;
  }
  .font input[type="number"] {
    flex: none;
    width: 5.71rem;
  }
  .font span {
    color: var(--muted);
  }
  summary {
    cursor: pointer;
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  summary .state {
    font-weight: 400;
    font-size: 0.86rem;
    color: var(--muted);
  }
  /* WebKitGTK doesn't lay out <details> content as flex, so space and size its children directly. */
  details[open] summary {
    margin-bottom: 0.71rem;
  }
  details .hint {
    margin-top: 0.43rem;
  }
  details .link {
    display: block;
    margin-top: 0.43rem;
    font-size: 0.93rem;
  }
  code {
    font-family: var(--font-mono);
    font-size: 0.86em;
    word-break: break-all;
  }
  textarea {
    font: 0.93rem/1.5 var(--font-mono);
    resize: vertical;
    display: block;
    width: 100%;
    min-height: 12rem;
  }
  footer {
    padding: 0.86rem 3.43rem;
    border-top: 1px solid var(--border);
    background: var(--surface);
  }
  .save {
    max-width: 51.43rem;
    margin: 0 auto;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 1.14rem;
  }
  .save span {
    font-size: 0.93rem;
  }
  .save button {
    flex: none;
    height: 3.14rem;
    padding: 0 1.57rem;
  }
  @media (max-width: 720px) {
    .scroll {
      padding: 1.43rem 1.14rem 2.29rem;
    }
    .page {
      gap: 1.43rem;
    }
    .card {
      padding: 1.29rem 1.14rem;
    }
    footer {
      padding: 0.71rem 1.14rem;
    }
  }
</style>
