import { Channel, invoke } from "@tauri-apps/api/core";

export type Track = "you" | "others";

/** One line of transcript.jsonl (ADR-0005). Times are seconds from the start of the Meeting. */
export interface Segment {
  track: Track;
  start: number;
  end: number;
  text: string;
  speaker?: number;
}

export type MeetingStatus = "recording" | "transcribing" | "complete" | "incomplete";

export interface Meeting {
  id: string;
  dir: string;
  title: string;
  app: string;
  started_at: string;
  ended_at: string | null;
  status: MeetingStatus;
  /** Its Tags; null until it has been tagged, empty when nothing fit. */
  tags: string[] | null;
}

/** Matches `MeetingListing` in src-tauri/src/commands.rs: a Meeting as the Meetings list shows it. */
export interface MeetingListing extends Meeting {
  /** The file name of the Screenshot its card shows, if it has any. */
  thumbnail: string | null;
  screenshotCount: number;
}

/** Matches `PipelineEvent` in crates/core/src/pipeline.rs. */
export type PipelineEvent =
  | { kind: "segments"; meetingId: string; segments: Segment[] }
  | { kind: "retrying"; meetingId: string; track: Track; message: string; delaySecs: number }
  | { kind: "error"; meetingId: string | null; message: string }
  | { kind: "finished"; meetingId: string };

/** Matches `JobEvent` in src-tauri/src/jobs.rs. */
export type JobEvent =
  | { kind: "retrying"; meetingId: string }
  | { kind: "retried"; meetingId: string; report: RetryReport }
  | { kind: "summarizing"; meetingId: string }
  /** `title` is the Meeting's title now, which the Summary may have set. */
  | { kind: "summarized"; meetingId: string; summary: Summary; title: string }
  | { kind: "tagged"; meetingId: string; tags: string[] }
  | { kind: "failed"; meetingId: string; message: string };

/** Matches `ScreenshotsEvent` in src-tauri/src/screenshots.rs. */
export interface ScreenshotsEvent {
  meetingId: string;
  /** Why no screenshots are being taken; null once they are. */
  problem: string | null;
}

/** Matches `ScreenshotView` in src-tauri/src/commands.rs. */
export interface ScreenshotView {
  file: string;
  /** Meeting time in ms; `ms / 1000` is on the Segments' timeline. */
  ms: number;
  /** The index of the shown Segment it goes before (the Segment count when it goes last). */
  position: number;
}

/** Matches `ScreenshotsMode` in crates/core/src/export.rs. */
export type ScreenshotsMode = "none" | "folder" | "embedded";

export interface Summary {
  /** The title the Summary gave, already applied unless the user had renamed the Meeting; null once stored. */
  suggestedTitle: string | null;
  /** The contents of summary.md. */
  markdown: string;
}

export interface RetryReport {
  recovered: number;
  remaining: number;
  error: string | null;
}

/** Matches `SpendKind` in crates/core/src/spend.rs; "other" is a kind written by a later version. */
export type SpendKind = "transcription" | "summary" | "tags" | "embedding" | "answer" | "other";

/** Matches `MeetingSpend` in crates/core/src/spend.rs. Amounts are USD. */
export interface MeetingSpend {
  total: number;
  /** Only the kinds with spend, in a fixed order. */
  byKind: { kind: SpendKind; cost: number }[];
}

/** Matches `SpendSummary` in crates/core/src/spend.rs. Amounts are USD. */
export interface SpendSummary {
  /** Every month with spend, in local time, newest first. */
  months: { year: number; month: number; total: number }[];
  total: number;
}

export const SPEND_KIND_LABEL: Record<SpendKind, string> = {
  transcription: "Transcription",
  summary: "Summary",
  tags: "Tags",
  embedding: "Embedding",
  answer: "Ask",
  other: "Other",
};

/** USD with two decimals, or four when under a cent and not zero. */
export function formatUsd(amount: number): string {
  return `$${amount.toFixed(amount !== 0 && Math.abs(amount) < 0.01 ? 4 : 2)}`;
}

/** Matches `Place` in crates/core/src/search.rs: where in its Meeting a Passage is. */
export type Place =
  | { source: "title" }
  /** Section 0 is the text before the first heading; section n starts at the n-th heading. */
  | { source: "summary"; section: number; heading: string | null }
  /** The paragraph starting at this line of notes.md, from 0. */
  | { source: "notes"; line: number }
  /** Meeting time in ms: the start of the window's first Segment. */
  | { source: "transcript"; ms: number };

/** Matches `Fragment` in crates/core/src/search.rs: a piece of a snippet; matched text is highlighted. */
export interface Fragment {
  text: string;
  matched: boolean;
}

/** Matches `Hit` in crates/core/src/search.rs: a Passage that matched, with its snippet. */
export interface Hit {
  meetingId: string;
  place: Place;
  snippet: Fragment[];
  /** Higher is a better match. */
  score: number;
}

/** Matches `MeetingResult` in crates/core/src/search.rs: a Meeting in the results, with its best Hits. */
export interface MeetingResult {
  meetingId: string;
  score: number;
  hits: Hit[];
}

/** Matches `SearchResults` in crates/core/src/search.rs. */
export interface SearchResults {
  /** The Tag filters in the query: a Meeting must have all of them. */
  tags: string[];
  /** Best first; null when the query has no keywords or phrases, so the list shows the Tagged Meetings in its own order. */
  meetings: MeetingResult[] | null;
  /** Whether keyword matches were merged with matches by meaning: "off" without a key or an Embedding model,
   * "unavailable" when the query couldn't be embedded. */
  meaning: "on" | "off" | "unavailable";
}

/** Matches `IndexProgress` in crates/core/src/search.rs; done equals total once indexing has finished. */
export interface IndexProgress {
  done: number;
  total: number;
}

/** Matches `EmbeddingProgress` in src-tauri/src/embedding.rs; done equals total once embedding has finished. */
export interface EmbeddingProgress {
  done: number;
  total: number;
  /** Why embedding stopped, until a Meeting is embedded again. */
  problem: string | null;
}

/** Where a search result's snippet came from: "Summary · Decisions", "Notes", "Transcript at 12:34". */
export function placeLabel(place: Place): string {
  switch (place.source) {
    case "title":
      return "Title";
    case "summary":
      return place.heading ? `Summary · ${place.heading}` : "Summary";
    case "notes":
      return "Notes";
    case "transcript":
      return `Transcript at ${clock(place.ms / 1000)}`;
  }
}

/** Matches `Source` in crates/core/src/ask.rs: a Passage an answer was written from, numbered for citing. */
export interface Source {
  /** Unique in its conversation, from 1. */
  number: number;
  meetingId: string;
  title: string;
  startedAt: string;
  place: Place;
}

/** Matches `Answer` in crates/core/src/ask.rs. */
export interface Answer {
  /** Markdown, each citation marked as CITE_OPEN, the Passage number, CITE_CLOSE. */
  text: string;
  /** The Passages it cites, in the order first cited. */
  sources: Source[];
}

/** Matches `AskEvent` in crates/core/src/ask.rs: what an answer being written streams. */
export type AskEvent =
  /** Before any text: every Passage the answer may cite. */
  | { kind: "passages"; sources: Source[] }
  /** The whole answer so far, its citations marked. */
  | { kind: "text"; text: string };

/** Match `CITE_OPEN` and `CITE_CLOSE` in crates/core/src/ask.rs. */
export const CITE_OPEN = "\uE000";
export const CITE_CLOSE = "\uE001";

const sourceDay = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short" });

/** A Passage as citations label it, the way Search results do: "Weekly sync, 14 Oct: Transcript at 12:34". */
export function sourceLabel(source: Source): string {
  return `${source.title}, ${sourceDay.format(new Date(source.startedAt))}: ${placeLabel(source.place)}`;
}

/** The query text for a Tag filter, quoted when it has spaces. */
export const tagTerm = (tag: string) => (/[\s"]/.test(tag) ? `tag:"${tag.replace(/"/g, "")}"` : `tag:${tag}`);

export interface SettingsView {
  sttModel: string;
  /** PipeWire node name; null follows the system default input. */
  mic: string | null;
  summaryModel: string;
  /** The model Passages and queries are embedded with for meaning search; blank turns it off. */
  embeddingModel: string;
  /** The model that answers questions asked of the Meetings; blank turns asking off. */
  askModel: string;
  /** null uses the built-in template. */
  summaryPrompt: string | null;
  defaultSummaryPrompt: string;
  displayName: string;
  keyPath: string;
  keyProblem: string | null;
  /** Whether ~/.config/autostart/tiro.desktop exists. */
  autostart: boolean;
  /** Font family and px size of the app's text, including Summaries and the Transcript. */
  textFont: string;
  textSize: number;
  /** Font family and px size of the Notes editor. */
  notesFont: string;
  notesSize: number;
  /** Light or dark colours, or whichever the system prefers. */
  theme: Theme;
  /** Whether a recording Meeting captures screenshots of the meeting app's window; off until consented to. */
  meetingScreenshots: boolean;
  /** How the last export with Screenshots included them; preselected next time. */
  exportScreenshots: ScreenshotsMode;
}

/** Matches `Theme` in src-tauri/src/settings.rs. */
export type Theme = "system" | "light" | "dark";

/** The fonts and the theme, saved and applied together. */
export type Appearance = Pick<SettingsView, "textFont" | "textSize" | "notesFont" | "notesSize" | "theme">;

export interface Microphone {
  name: string;
  description: string;
}

export const api = {
  startRecording: () => invoke<Meeting>("start_recording"),
  stopRecording: () => invoke<void>("stop_recording"),
  activeMeeting: () => invoke<Meeting | null>("active_meeting"),
  listMeetings: () => invoke<MeetingListing[]>("list_meetings"),
  transcript: (meetingId: string) => invoke<Segment[]>("transcript", { meetingId }),
  /** The Meeting's Screenshots by time, placed in the Transcript `transcript` returns. */
  screenshots: (meetingId: string) => invoke<ScreenshotView[]>("meeting_screenshots", { meetingId }),
  getSettings: () => invoke<SettingsView>("get_settings"),
  saveSettings: (sttModel: string, mic: string | null, apiKey: string | null) =>
    invoke<SettingsView>("save_settings", { sttModel, mic, apiKey }),
  listMicrophones: () => invoke<Microphone[]>("list_microphones"),
  saveSummarySettings: (
    summaryModel: string,
    summaryPrompt: string,
    displayName: string,
    embeddingModel: string,
    askModel: string,
  ) => invoke<SettingsView>("save_summary_settings", { summaryModel, summaryPrompt, displayName, embeddingModel, askModel }),
  setAutostart: (enabled: boolean) => invoke<SettingsView>("set_autostart", { enabled }),
  setMeetingScreenshots: (enabled: boolean) => invoke<SettingsView>("set_meeting_screenshots", { enabled }),
  /** Installed font families, sorted. */
  listFonts: () => invoke<string[]>("list_fonts"),
  saveAppearance: (appearance: Appearance) => invoke<SettingsView>("save_appearance", { ...appearance }),
  summary: (meetingId: string) => invoke<Summary | null>("meeting_summary", { meetingId }),
  summarize: (meetingId: string) => invoke<Summary>("summarize", { meetingId }),
  /** What the Meeting's OpenRouter requests have cost. */
  meetingSpend: (meetingId: string) => invoke<MeetingSpend>("meeting_spend", { meetingId }),
  /** Spend per month and to date, deleted Meetings included. */
  spendSummary: () => invoke<SpendSummary>("spend_summary"),
  retryFailedChunks: (meetingId: string) => invoke<RetryReport>("retry_failed_chunks", { meetingId }),
  setTitle: (meetingId: string, title: string) => invoke<Meeting>("set_title", { meetingId, title }),
  /** Deletes the Meeting's folder; refused while it is recording, transcribing, or being retried or summarized. */
  deleteMeeting: (meetingId: string) => invoke<void>("delete_meeting", { meetingId }),
  /** Where the export's Save dialog starts: `<title>.md` in the last export's folder, or in Downloads. */
  exportDefaultPath: (meetingId: string) => invoke<string>("export_default_path", { meetingId }),
  /**
   * Writes the Markdown export to `path`, remembers its folder for next time, and returns the path. `screenshots`
   * is the choice offered when the Meeting has Screenshots (remembered too); null exports without them.
   * `summaryOnly` leaves out the Transcript and its Screenshots.
   */
  exportMeeting: (meetingId: string, path: string, screenshots: ScreenshotsMode | null, summaryOnly = false) =>
    invoke<string>("export_meeting", { meetingId, path, screenshots, summaryOnly }),
  /** The contents of notes.md; empty when there are none. */
  notes: (meetingId: string) => invoke<string>("notes", { meetingId }),
  saveNotes: (meetingId: string, notes: string) => invoke<void>("save_notes", { meetingId, notes }),
  /** Answers a `flushNotes` event once pending Notes are saved. */
  notesFlushed: (request: number) => invoke<void>("notes_flushed", { request }),
  /** Keywords, quoted phrases and `tag:` filters, matched by keywords and by meaning; malformed input finds less, never fails. */
  search: (query: string) => invoke<SearchResults>("search", { query }),
  /** Indexes the Meeting for Search again, once Notes editing stops; not on every autosave. */
  indexMeeting: (meetingId: string) => invoke<void>("index_meeting", { meetingId }),
  /** How far the startup indexing for Search has got. */
  indexingProgress: () => invoke<IndexProgress>("indexing_progress"),
  /** How far embedding Meetings for meaning search has got. */
  embeddingProgress: () => invoke<EmbeddingProgress>("embedding_progress"),
  /** Why asking questions of the Meetings is off (no key or no Ask model); null when it is on. */
  askOff: () => invoke<string | null>("ask_off"),
  /** Answers a question as the next turn of the conversation; `onEvent` gets the Passages it may cite, then the
   * whole answer so far each time more of it comes. */
  ask: (question: string, onEvent: (event: AskEvent) => void) => {
    const channel = new Channel<AskEvent>();
    channel.onmessage = onEvent;
    return invoke<Answer>("ask", { question, onEvent: channel });
  },
  /** Forgets the conversation, so the next question starts a new one. */
  newConversation: () => invoke<void>("new_conversation"),
};

/** Event names emitted by src-tauri/src/main.rs, recorder.rs, jobs.rs, tray.rs, notes.rs, search.rs and embedding.rs. */
export const EVENTS = {
  pipeline: "pipeline",
  activeMeeting: "active-meeting",
  navigate: "navigate",
  job: "meeting-job",
  screenshots: "screenshots",
  /** Payload: the request id to pass to `api.notesFlushed`. */
  flushNotes: "flush-notes",
  /** Payload: the id of the Meeting whose spend grew, or null for spend on no Meeting. */
  spend: "spend",
  /** Payload: an `IndexProgress`. */
  indexing: "indexing",
  /** Payload: an `EmbeddingProgress`. */
  embedding: "embedding",
} as const;

/** Served by src-tauri/src/screenshot_protocol.rs. */
export const screenshotUrl = (meetingId: string, shot: Pick<ScreenshotView, "file">) =>
  `screenshot://localhost/${encodeURIComponent(meetingId)}/${encodeURIComponent(shot.file)}`;

export const trackLabel = (track: Track) => (track === "you" ? "You" : "Others");

/** A Meeting status's label and `.chip` class (app.css). */
export const STATUS_CHIP: Record<MeetingStatus, { label: string; chip: string }> = {
  complete: { label: "Complete", chip: "ok" },
  recording: { label: "Recording", chip: "rec" },
  transcribing: { label: "Transcribing", chip: "" },
  incomplete: { label: "Incomplete", chip: "warn" },
};

export function clock(seconds: number): string {
  const s = Math.floor(seconds);
  const h = Math.floor(s / 3600);
  const mm = String(Math.floor((s % 3600) / 60)).padStart(2, "0");
  const ss = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}
