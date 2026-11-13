# Tiro

A tray app for Fedora KDE that records online meetings, transcribes them live, and summarizes them afterwards without losing exploratory discussion.

## Language

### Recording

**Meeting**:
One recorded session, from start to stop, together with everything produced from it.
_Avoid_: Call, session, recording

**Track**:
One of the two audio sources of a Meeting: **You** (the user's microphone) or **Others** (everything the remote participants say).
_Avoid_: Channel, stream, source

**Chunk**:
A short slice of one Track's audio, bounded by pauses, that is sent for transcription as a unit.
_Avoid_: Segment, clip, block

**Detection prompt**:
The tray notification asking the user whether to start (or stop) recording because a meeting app appears to be in use.
_Avoid_: Auto-record, trigger

### Text

**Segment**:
A timestamped span of transcribed text attributed to one Track, and optionally to a Speaker within it.
_Avoid_: Line, utterance, chunk

**Speaker**:
A distinct remote voice within the Others Track, identified only by an index unless the user names it.
_Avoid_: Participant, attendee

**Display name**:
The name the user goes by in the Transcript and Summary in place of the You Track, including as the owner of their action items; "You" when they set none.
_Avoid_: Username, user name, nickname

**Transcript**:
The ordered set of Segments for a Meeting, interleaving both Tracks on one timeline.
_Avoid_: Captions, log

**Screenshot**:
An image of the meeting app's window, kept during a Meeting when its content changed, and placed on the Transcript's timeline by its Meeting-relative time. Parts that keep moving, such as camera tiles, don't count as a change; while they move, one is kept every five minutes.
_Avoid_: Capture, frame, snapshot

**Notes**:
Markdown the user writes about a Meeting, during or after it. Notes take priority over the Transcript when summarizing.
_Avoid_: Comments, annotations

**Summary**:
The model-written digest of a Meeting, built from the Transcript and Notes.
_Avoid_: Recap, minutes

**Discussion thread**:
A topic explored in a Meeting, captured in the Summary with its ideas, alternatives and set-aside options, whether or not it led to a decision.
_Avoid_: Topic, agenda item

**Tag**:
A part of the work a Meeting covered (a product area, workstream, business function or customer, never what nearly every Meeting is about), attached to the Meeting, generated from its Summary and drawn from the vocabulary of every Tag already in use, so the same topic in different Meetings gets the same Tag. Not to be confused with the audio-event tags (`[laughs]`) in a Segment's text.
_Avoid_: Label, category, keyword, topic

### Cost

**Spend**:
What OpenRouter charged, in USD, for Tiro's requests: transcribing Chunks, writing Summaries, generating Tags and embedding Passages and search queries. Counted per Meeting, per calendar month and in total, using the cost OpenRouter reports for each request. A request that was charged counts even if Tiro then treated it as a failure.
_Avoid_: Expenses, charges. "Cost" and "usage" are OpenRouter's words for one request's figure, not for Spend.

**Spend ledger**:
The append-only `spend.jsonl` in the data folder, one line per charged request with its time, Meeting, kind, model and cost. Every Spend figure is derived from it. It keeps the lines of deleted Meetings, so their Spend still counts.
_Avoid_: Spend log, usage log

### Search

**Passage**:
A searchable piece of a Meeting with its place in it: its title, a Summary section (by heading), a Notes paragraph, or a Transcript window of a few Segments (about 30–60 seconds) with its start time. Search matches Passages, not whole Meetings, so a match deep in a long Transcript isn't diluted and a result can open the Meeting at the right spot.
_Avoid_: Chunk (that's audio), snippet (that's the excerpt of a Passage shown in a result), document

**Search**:
Finding Meetings by Tags, keywords and meaning from one search box. Tags filter; keyword and meaning matches rank Passages, and a Meeting ranks by its best Passage.
_Avoid_: Filter (that's only the Tag part), query (that's what the user types)

**Ask**:
Asking a question of the Meetings and getting an answer written by the Ask model from the Passages Search finds for it, each claim citing the Passage it came from. Follow-up questions keep the conversation's earlier questions and answers in view. A conversation lasts until a new one is started or Tiro quits, and isn't kept.
_Avoid_: Chat (that's only its look), Search (that finds Meetings, it doesn't answer)

**Embedding**:
A Passage's or a search query's meaning as a vector of numbers from the Embedding model, so Passages can be matched to a query worded differently. Kept in the Meeting folder, keyed by the Passage's text and the model, so an unchanged Passage is never embedded twice.
_Avoid_: Vector (that's only its numbers), semantic index
