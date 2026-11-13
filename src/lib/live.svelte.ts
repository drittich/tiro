import { listen } from "@tauri-apps/api/event";
import { api, EVENTS, type JobEvent, type Meeting, type PipelineEvent, type ScreenshotsEvent, type Segment } from "./api";

/** The Meeting shown in the Live view and its Transcript so far. */
class LiveState {
  meeting = $state<Meeting | null>(null);
  recording = $state(false);
  segments = $state<Segment[]>([]);
  error = $state<string | null>(null);
  notice = $state<string | null>(null);
  busy = $state(false);
  /** The stopped Meeting's Summary is ready to open. */
  summaryReady = $state(false);
  /** The last word from screenshot capture. It can arrive before the Meeting it is about is shown here. */
  private screenshots = $state<ScreenshotsEvent | null>(null);

  /** Why the Meeting shown gets no screenshots, when they are turned on. */
  get screenshotsProblem(): string | null {
    return this.screenshots && this.screenshots.meetingId === this.meeting?.id ? this.screenshots.problem : null;
  }

  async init() {
    await listen<PipelineEvent>(EVENTS.pipeline, (e) => this.onPipeline(e.payload));
    await listen<Meeting | null>(EVENTS.activeMeeting, (e) => this.onActiveMeeting(e.payload));
    await listen<JobEvent>(EVENTS.job, (e) => this.onJob(e.payload));
    await listen<ScreenshotsEvent>(EVENTS.screenshots, (e) => (this.screenshots = e.payload));
    const current = await api.activeMeeting();
    if (current) await this.show(current, true);
  }

  async start() {
    this.busy = true;
    this.error = null;
    try {
      await api.startRecording();
    } catch (e) {
      this.error = String(e);
    } finally {
      this.busy = false;
    }
  }

  async stop() {
    this.busy = true;
    try {
      await api.stopRecording();
    } finally {
      this.busy = false;
    }
  }

  private async show(meeting: Meeting, recording: boolean) {
    this.meeting = meeting;
    this.recording = recording;
    this.notice = null;
    this.summaryReady = false;
    this.segments = await api.transcript(meeting.id);
  }

  private async onActiveMeeting(meeting: Meeting | null) {
    if (meeting) {
      this.error = null;
      await this.show(meeting, true);
    } else {
      this.recording = false;
      if (this.meeting) this.notice = "Stopped. Transcribing the last Chunks…";
    }
  }

  private onPipeline(event: PipelineEvent) {
    if (event.meetingId && event.meetingId !== this.meeting?.id) return;
    switch (event.kind) {
      case "segments":
        this.segments.push(...event.segments);
        if (this.recording) this.notice = null;
        break;
      case "retrying":
        this.notice = `Transcription is retrying in ${event.delaySecs}s (${event.message}). Recording continues.`;
        break;
      case "error":
        this.error = event.message;
        break;
      case "finished":
        this.notice = this.recording ? null : "Transcript complete.";
        break;
    }
  }

  private onJob(event: JobEvent) {
    if (event.meetingId !== this.meeting?.id || this.recording) return;
    switch (event.kind) {
      case "retrying":
        this.notice = "Retrying failed Chunks…";
        break;
      case "retried":
        api.transcript(event.meetingId).then((s) => (this.segments = s));
        this.notice = event.report.remaining
          ? `${event.report.remaining} Chunk(s) still failed; you can retry them from Meetings.`
          : null;
        break;
      case "summarizing":
        this.notice = "Transcript complete. Summarizing…";
        break;
      case "summarized":
        this.notice = "Summary ready.";
        this.summaryReady = true;
        break;
      case "failed":
        this.notice = null;
        this.error = `Summary failed: ${event.message} You can re-summarize from Meetings.`;
        break;
    }
  }
}

export const live = new LiveState();
