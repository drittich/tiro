//! Meeting folders (the source of truth) and the SQLite index over them (ADR-0005).

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, FixedOffset};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::screenshots::{self, Screenshot};
use crate::search::{self, Fragment, Hit, Passage, Place};
use crate::summary::Summary;
use crate::transcript::Segment;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MeetingStatus {
    Recording,
    Transcribing,
    Complete,
    /// Some Chunks were never transcribed (blocked, rejected, or the app quit first); they stay in `chunks/`.
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Meeting {
    pub id: String,
    pub dir: PathBuf,
    pub title: String,
    pub app: String,
    pub started_at: DateTime<FixedOffset>,
    pub ended_at: Option<DateTime<FixedOffset>>,
    pub status: MeetingStatus,
    /// The Meeting's Tags; `None` until it has been tagged, empty when tagged and nothing fit.
    pub tags: Option<Vec<String>>,
}

impl Meeting {
    pub fn chunks_dir(&self) -> PathBuf {
        self.dir.join("chunks")
    }
}

/// `meeting.json` in each Meeting folder: the metadata the index is rebuilt from.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MeetingFile {
    title: String,
    app: String,
    started_at: DateTime<FixedOffset>,
    ended_at: Option<DateTime<FixedOffset>>,
    status: MeetingStatus,
    /// Whether the user has given the Meeting its title, which a Summary then leaves alone. Missing in files from
    /// before Summaries applied their titles: the title then counts as the user's unless it is still the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    renamed: Option<bool>,
    /// Missing until the Meeting has been tagged; an empty list once tagged with nothing that fit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
}

const META: &str = "meeting.json";
const TRANSCRIPT: &str = "transcript.jsonl";
const NOTES: &str = "notes.md";
const SUMMARY: &str = "summary.md";

type IndexedListener = Box<dyn Fn(&str) + Send + Sync>;

pub struct MeetingStore {
    root: PathBuf,
    index: Mutex<Connection>,
    indexed: OnceLock<IndexedListener>,
}

impl MeetingStore {
    /// Opens the Meetings folder `root` and the index database at `db`, creating either if missing.
    pub fn open(root: &Path, db: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        if let Some(parent) = db.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db)?;
        // The index is only a cache: one from before Tags is dropped, and the rebuild at startup fills it again.
        if conn.prepare("SELECT tags FROM meetings LIMIT 0").is_err() {
            conn.execute_batch("DROP TABLE IF EXISTS meetings;")?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meetings (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                app TEXT NOT NULL,
                started_at TEXT NOT NULL,
                started_ts INTEGER NOT NULL,
                ended_at TEXT,
                status TEXT NOT NULL,
                tags TEXT
            );",
        )?;
        // Passages, searched by their text and a Summary section's heading, and the fingerprint of the files each
        // Meeting's were split from (ADR-0005). Dropped together when either's shape changes (raise
        // PASSAGES_VERSION when the FTS5 columns do); the startup indexing fills them again.
        let current = conn
            .query_row("SELECT version FROM passages_version", [], |row| row.get::<_, u32>(0))
            .is_ok_and(|v| v == PASSAGES_VERSION);
        if !current || conn.prepare("SELECT stamp FROM passage_stamps LIMIT 0").is_err() {
            conn.execute_batch("DROP TABLE IF EXISTS passages; DROP TABLE IF EXISTS passage_stamps;")?;
        }
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS passages USING fts5(
                text,
                meeting_id UNINDEXED,
                source UNINDEXED,
                anchor UNINDEXED,
                heading,
                tokenize = 'unicode61 remove_diacritics 2'
            );
            CREATE TABLE IF NOT EXISTS passage_stamps (
                meeting_id TEXT PRIMARY KEY,
                stamp TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS passages_version (version INTEGER NOT NULL);",
        )?;
        if !current {
            conn.execute_batch("DELETE FROM passages_version;")?;
            conn.execute("INSERT INTO passages_version (version) VALUES (?1)", params![PASSAGES_VERSION])?;
        }
        Ok(Self { root: root.to_owned(), index: Mutex::new(conn), indexed: OnceLock::new() })
    }

    /// Calls `listener` with the Meeting's id each time its Passages are indexed from now on. Only the first
    /// listener set is kept.
    pub fn on_indexed(&self, listener: impl Fn(&str) + Send + Sync + 'static) {
        let _ = self.indexed.set(Box::new(listener));
    }

    /// Creates the folder for a new Meeting titled `<app> — <YYYY-MM-DD HH:MM>`.
    pub fn create(&self, app: &str, started_at: DateTime<FixedOffset>) -> Result<Meeting> {
        let base = format!("{}-{}", started_at.format("%Y-%m-%d-%H%M"), slug(app));
        let mut n = 1;
        let (id, dir) = loop {
            let id = if n == 1 { base.clone() } else { format!("{base}-{n}") };
            let dir = self.root.join(&id);
            match fs::create_dir(&dir) {
                Ok(()) => break (id, dir),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e.into()),
            }
        };
        fs::create_dir(dir.join("chunks"))?;

        let file = MeetingFile {
            title: default_title(app, started_at),
            app: app.to_owned(),
            started_at,
            ended_at: None,
            status: MeetingStatus::Recording,
            renamed: Some(false),
            tags: None,
        };
        write_meta(&dir, &file)?;
        let meeting = to_meeting(id, dir, file);
        self.index_upsert(&meeting)?;
        Ok(meeting)
    }

    pub fn append_segments(&self, meeting: &Meeting, segments: &[Segment]) -> Result<()> {
        let mut lines = String::new();
        for segment in segments {
            lines.push_str(&serde_json::to_string(segment)?);
            lines.push('\n');
        }
        let mut file = OpenOptions::new().create(true).append(true).open(meeting.dir.join(TRANSCRIPT))?;
        file.write_all(lines.as_bytes())?;
        Ok(())
    }

    /// Rewrites `transcript.jsonl` with `segments`, atomically.
    pub fn replace_transcript(&self, meeting: &Meeting, segments: &[Segment]) -> Result<()> {
        let mut lines = String::new();
        for segment in segments {
            lines.push_str(&serde_json::to_string(segment)?);
            lines.push('\n');
        }
        write_atomic(&meeting.dir.join(TRANSCRIPT), lines.as_bytes())
    }

    pub fn transcript(&self, meeting: &Meeting) -> Result<Vec<Segment>> {
        let raw = match fs::read_to_string(meeting.dir.join(TRANSCRIPT)) {
            Ok(raw) => raw,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        raw.lines().filter(|l| !l.trim().is_empty()).map(|l| Ok(serde_json::from_str(l)?)).collect()
    }

    pub fn get(&self, id: &str) -> Result<Meeting> {
        let dir = self.root.join(id);
        Ok(to_meeting(id.to_owned(), dir.clone(), read_meta(&dir)?))
    }

    pub fn set_status(&self, id: &str, status: MeetingStatus, ended_at: Option<DateTime<FixedOffset>>) -> Result<()> {
        let dir = self.root.join(id);
        let mut file = read_meta(&dir)?;
        file.status = status;
        if ended_at.is_some() {
            file.ended_at = ended_at;
        }
        write_meta(&dir, &file)?;
        self.index_upsert(&to_meeting(id.to_owned(), dir, file))?;
        if matches!(status, MeetingStatus::Complete | MeetingStatus::Incomplete) {
            // The Transcript is final.
            self.reindex(id);
        }
        Ok(())
    }

    /// Renames the Meeting, so a later Summary keeps this title. Its folder keeps its name.
    pub fn set_title(&self, id: &str, title: &str) -> Result<()> {
        let title = title.trim();
        if title.is_empty() {
            return Err(StoreError::Invalid("A Meeting needs a title.".into()));
        }
        let dir = self.root.join(id);
        let mut file = read_meta(&dir)?;
        file.title = title.to_owned();
        file.renamed = Some(true);
        write_meta(&dir, &file)?;
        self.index_upsert(&to_meeting(id.to_owned(), dir, file))?;
        self.reindex(id);
        Ok(())
    }

    /// Replaces the Meeting's Tags.
    pub fn set_tags(&self, id: &str, tags: &[String]) -> Result<()> {
        let dir = self.root.join(id);
        let mut file = read_meta(&dir)?;
        file.tags = Some(tags.to_vec());
        write_meta(&dir, &file)?;
        self.index_upsert(&to_meeting(id.to_owned(), dir, file))
    }

    /// Removes the Meeting's Tags, so it counts as never tagged.
    pub fn clear_tags(&self, id: &str) -> Result<()> {
        let dir = self.root.join(id);
        let mut file = read_meta(&dir)?;
        file.tags = None;
        write_meta(&dir, &file)?;
        self.index_upsert(&to_meeting(id.to_owned(), dir, file))
    }

    /// Deletes the Meeting: its folder, with everything in it, and its index row.
    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.meeting_dir(id)?;
        // Only ever remove a Meeting folder.
        read_meta(&dir)?;
        fs::remove_dir_all(&dir)?;
        let conn = self.index.lock().unwrap();
        conn.execute("DELETE FROM meetings WHERE id = ?1", params![id])?;
        conn.execute("DELETE FROM passages WHERE meeting_id = ?1", params![id])?;
        conn.execute("DELETE FROM passage_stamps WHERE meeting_id = ?1", params![id])?;
        Ok(())
    }

    /// The user's Notes (`notes.md`); empty when there are none.
    pub fn notes(&self, meeting: &Meeting) -> Result<String> {
        Ok(read_optional(&meeting.dir.join(NOTES))?.unwrap_or_default())
    }

    /// Replaces the Notes, atomically, so a crash mid-save keeps the previous version. Their Passages are left as
    /// they were: call [`Self::index_passages`] once editing stops, not on every autosave.
    pub fn write_notes(&self, meeting: &Meeting, notes: &str) -> Result<()> {
        write_atomic(&meeting.dir.join(NOTES), notes.as_bytes())
    }

    /// The last Summary (`summary.md` and the title it suggested), if there is one.
    /// The Meeting's Summary. Its title was applied when it was written and isn't kept, so it has none.
    pub fn summary(&self, meeting: &Meeting) -> Result<Option<Summary>> {
        let Some(markdown) = read_optional(&meeting.dir.join(SUMMARY))? else { return Ok(None) };
        Ok(Some(Summary { suggested_title: None, markdown }))
    }

    /// Replaces the Summary. The Tags came from the old one, so they go until the Meeting is tagged again.
    pub fn write_summary(&self, meeting: &Meeting, summary: &Summary) -> Result<()> {
        write_atomic(&meeting.dir.join(SUMMARY), summary.markdown.as_bytes())?;
        let mut file = read_meta(&meeting.dir)?;
        // The Summary's title replaces the default, or an earlier Summary's, but never one the user gave.
        if let Some(title) = summary.suggested_title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            if !renamed_by_user(&file) {
                file.title = title.to_owned();
                file.renamed = Some(false);
            }
        }
        file.tags = None;
        write_meta(&meeting.dir, &file)?;
        self.index_upsert(&to_meeting(meeting.id.clone(), meeting.dir.clone(), file))?;
        self.reindex(&meeting.id);
        Ok(())
    }

    /// The Meeting's Screenshots, by time.
    pub fn screenshots(&self, meeting: &Meeting) -> Result<Vec<Screenshot>> {
        Ok(screenshots::list(&meeting.dir)?)
    }

    /// The Screenshot of Meeting `id` with this file name, if it has one. Only ever a file in that Meeting's
    /// `screenshots/` that the listing returns.
    pub fn screenshot(&self, id: &str, file_name: &str) -> Result<Option<Screenshot>> {
        let dir = self.meeting_dir(id)?;
        read_meta(&dir)?;
        Ok(screenshots::list(&dir)?.into_iter().find(|s| s.file_name() == file_name))
    }

    /// Deletes the Screenshot of Meeting `id` with this file name, for good. Only ever a file the listing returns, as
    /// in [`Self::screenshot`]. Returns whether there was one to delete. The `.thinned` marker stays.
    pub fn delete_screenshot(&self, id: &str, file_name: &str) -> Result<bool> {
        let Some(screenshot) = self.screenshot(id, file_name)? else { return Ok(false) };
        fs::remove_file(&screenshot.path)?;
        Ok(true)
    }

    /// The folder of Meeting `id`, refusing ids that are not a single folder name.
    fn meeting_dir(&self, id: &str) -> Result<PathBuf> {
        if !matches!(Path::new(id).components().collect::<Vec<_>>()[..], [Component::Normal(_)]) {
            return Err(StoreError::Invalid(format!("{id:?} is not a Meeting.")));
        }
        Ok(self.root.join(id))
    }

    /// Marks Meetings a previous run left recording or transcribing as incomplete. Call once at startup.
    pub fn mark_interrupted(&self) -> Result<()> {
        for meeting in self.list()? {
            if matches!(meeting.status, MeetingStatus::Recording | MeetingStatus::Transcribing) {
                self.set_status(&meeting.id, MeetingStatus::Incomplete, None)?;
            }
        }
        Ok(())
    }

    /// Every indexed Meeting, newest first.
    pub fn list(&self) -> Result<Vec<Meeting>> {
        let conn = self.index.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, app, started_at, ended_at, status, tags FROM meetings ORDER BY started_ts DESC, id DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut meetings = Vec::new();
        for row in rows {
            let (id, title, app, started_at, ended_at, status, tags) = row?;
            let parse = |s: &str| DateTime::parse_from_rfc3339(s).ok();
            let (Some(started_at), Ok(status)) = (parse(&started_at), serde_json::from_value(status.into())) else {
                continue;
            };
            meetings.push(Meeting {
                dir: self.root.join(&id),
                id,
                title,
                app,
                started_at,
                ended_at: ended_at.as_deref().and_then(parse),
                status,
                tags: tags.and_then(|t| serde_json::from_str(&t).ok()),
            });
        }
        Ok(meetings)
    }

    /// Replaces the index with what the Meeting folders say. Folders without a readable `meeting.json` are skipped.
    pub fn rebuild_index(&self) -> Result<()> {
        let mut meetings = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let dir = entry?.path();
            let Some(id) = dir.file_name().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
            match read_meta(&dir) {
                Ok(file) => meetings.push(to_meeting(id, dir, file)),
                Err(e) => tracing::debug!("skipping {}: {e}", dir.display()),
            }
        }
        self.index.lock().unwrap().execute("DELETE FROM meetings", [])?;
        for meeting in &meetings {
            self.index_upsert(meeting)?;
        }
        // The Passages of Meetings that are gone. The rest are checked against their files by `search::index_stale`.
        self.index.lock().unwrap().execute_batch(
            "DELETE FROM passages WHERE meeting_id NOT IN (SELECT id FROM meetings);
             DELETE FROM passage_stamps WHERE meeting_id NOT IN (SELECT id FROM meetings);",
        )?;
        Ok(())
    }

    /// Splits the Meeting into Passages again, from its files, replacing the ones it had. A Meeting still recording
    /// or transcribing is left until its Transcript is final.
    pub fn index_passages(&self, id: &str) -> Result<()> {
        let meeting = self.get(id)?;
        if matches!(meeting.status, MeetingStatus::Recording | MeetingStatus::Transcribing) {
            return Ok(());
        }
        // Taken before the files are read, so a change made while they are leaves the Passages stale.
        let stamp = passage_stamp(&meeting.dir);
        let summary = read_optional(&meeting.dir.join(SUMMARY))?.unwrap_or_default();
        let passages = search::split(id, &meeting.title, &summary, &self.notes(&meeting)?, &self.transcript(&meeting)?);

        let mut conn = self.index.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM passages WHERE meeting_id = ?1", params![id])?;
        {
            let mut insert =
                tx.prepare("INSERT INTO passages (text, meeting_id, source, anchor, heading) VALUES (?1, ?2, ?3, ?4, ?5)")?;
            for passage in &passages {
                let (source, anchor, heading) = place_columns(&passage.place);
                insert.execute(params![passage.text, id, source, anchor, heading])?;
            }
        }
        tx.execute("INSERT OR REPLACE INTO passage_stamps (meeting_id, stamp) VALUES (?1, ?2)", params![id, stamp])?;
        tx.commit()?;
        drop(conn);
        if let Some(listener) = self.indexed.get() {
            listener(id);
        }
        Ok(())
    }

    /// The Meeting's indexed Passages, in order; none until it has been indexed.
    pub fn passages(&self, id: &str) -> Result<Vec<Passage>> {
        let conn = self.index.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT text, source, anchor, heading FROM passages WHERE meeting_id = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map(params![id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, Option<String>>(3)?))
        })?;
        let mut passages = Vec::new();
        for row in rows {
            let (text, source, anchor, heading) = row?;
            if let Some(place) = place_from_columns(&source, anchor, heading) {
                passages.push(Passage { meeting_id: id.to_owned(), place, text });
            }
        }
        Ok(passages)
    }

    /// Whether the Meeting's Passages were split from its files as they are now.
    pub fn passages_current(&self, meeting: &Meeting) -> Result<bool> {
        let stored: Option<String> = self
            .index
            .lock()
            .unwrap()
            .query_row("SELECT stamp FROM passage_stamps WHERE meeting_id = ?1", params![meeting.id], |row| row.get(0))
            .map(Some)
            .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })?;
        Ok(stored.is_some_and(|s| s == passage_stamp(&meeting.dir)))
    }

    /// The Passages matching the FTS5 expression `fts`, best first (BM25), at most `limit`, only of the Meetings in
    /// `among` when it is given.
    pub fn matching_passages(&self, fts: &str, among: Option<&[String]>, limit: usize) -> Result<Vec<Hit>> {
        let among_sql = match among {
            Some(ids) => format!("AND meeting_id IN ({})", vec!["?"; ids.len()].join(", ")),
            None => String::new(),
        };
        let sql = format!(
            "SELECT meeting_id, source, anchor, heading, snippet(passages, 0, ?, ?, '…', {SNIPPET_TOKENS}), bm25(passages)
             FROM passages WHERE passages MATCH ? {among_sql} ORDER BY bm25(passages) LIMIT {limit}"
        );
        let conn = self.index.lock().unwrap();
        let mut stmt = conn.prepare(&sql)?;
        let mut values: Vec<&dyn rusqlite::ToSql> = vec![&MATCH_START, &MATCH_END, &fts];
        values.extend(among.unwrap_or_default().iter().map(|id| id as &dyn rusqlite::ToSql));
        let rows = stmt.query_map(values.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, f64>(5)?,
            ))
        })?;
        let mut hits = Vec::new();
        for row in rows {
            let (meeting_id, source, anchor, heading, snippet, bm25) = row?;
            let Some(place) = place_from_columns(&source, anchor, heading) else { continue };
            // BM25 is lower for a better match.
            hits.push(Hit { meeting_id, place, snippet: fragments(&snippet), score: -bm25 });
        }
        Ok(hits)
    }

    /// [`Self::index_passages`] after a change to the Meeting's text, which stands even when that fails.
    fn reindex(&self, id: &str) {
        if let Err(e) = self.index_passages(id) {
            tracing::warn!("could not index the Passages of {id}: {e}");
        }
    }

    fn index_upsert(&self, m: &Meeting) -> Result<()> {
        let status = serde_json::to_value(m.status)?.as_str().unwrap_or_default().to_owned();
        self.index.lock().unwrap().execute(
            "INSERT OR REPLACE INTO meetings (id, title, app, started_at, started_ts, ended_at, status, tags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                m.id,
                m.title,
                m.app,
                m.started_at.to_rfc3339(),
                m.started_at.timestamp(),
                m.ended_at.map(|t| t.to_rfc3339()),
                status,
                m.tags.as_ref().map(serde_json::to_string).transpose()?
            ],
        )?;
        Ok(())
    }
}

/// Brackets the matched words in a snippet; private-use characters, so never in a Meeting's text.
const MATCH_START: &str = "\u{E000}";
const MATCH_END: &str = "\u{E001}";
/// About a line of text either side of the match.
const SNIPPET_TOKENS: u32 = 16;
/// Raised when the `passages` table's columns change, so it is dropped and refilled.
const PASSAGES_VERSION: u32 = 1;
/// Raised when the Passage splitter changes, so every Meeting is split again.
const SPLITTER_VERSION: u32 = 1;

/// A fingerprint of the files a Meeting's Passages are split from: their sizes and modification times.
fn passage_stamp(dir: &Path) -> String {
    let mut stamp = format!("v{SPLITTER_VERSION}");
    for name in [META, SUMMARY, NOTES, TRANSCRIPT] {
        match fs::metadata(dir.join(name)) {
            Ok(meta) => {
                let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                stamp.push_str(&format!(";{}:{}", meta.len(), modified.map_or(0, |d| d.as_nanos())));
            }
            Err(_) => stamp.push_str(";-"),
        }
    }
    stamp
}

/// How a Place is stored in the `passages` table: source, anchor and heading.
fn place_columns(place: &Place) -> (&'static str, i64, Option<&str>) {
    match place {
        Place::Title => ("title", 0, None),
        Place::Summary { section, heading } => ("summary", *section as i64, heading.as_deref()),
        Place::Notes { line } => ("notes", *line as i64, None),
        Place::Transcript { ms } => ("transcript", *ms as i64, None),
    }
}

fn place_from_columns(source: &str, anchor: i64, heading: Option<String>) -> Option<Place> {
    let anchor = anchor.max(0);
    Some(match source {
        "title" => Place::Title,
        "summary" => Place::Summary { section: anchor as usize, heading },
        "notes" => Place::Notes { line: anchor as usize },
        "transcript" => Place::Transcript { ms: anchor as u64 },
        _ => return None,
    })
}

/// A snippet from FTS5, its matches bracketed by [`MATCH_START`] and [`MATCH_END`], as Fragments.
fn fragments(snippet: &str) -> Vec<Fragment> {
    let mut out = Vec::new();
    for (k, part) in snippet.split(MATCH_START).enumerate() {
        let (matched, rest) = match part.split_once(MATCH_END) {
            Some((matched, rest)) if k > 0 => (matched, rest),
            _ => ("", part),
        };
        if !matched.is_empty() {
            out.push(Fragment { text: matched.to_owned(), matched: true });
        }
        if !rest.is_empty() {
            out.push(Fragment { text: rest.to_owned(), matched: false });
        }
    }
    out
}

fn to_meeting(id: String, dir: PathBuf, f: MeetingFile) -> Meeting {
    Meeting {
        id,
        dir,
        title: f.title,
        app: f.app,
        started_at: f.started_at,
        ended_at: f.ended_at,
        status: f.status,
        tags: f.tags,
    }
}

fn default_title(app: &str, started_at: DateTime<FixedOffset>) -> String {
    format!("{app} — {}", started_at.format("%Y-%m-%d %H:%M"))
}

fn renamed_by_user(file: &MeetingFile) -> bool {
    file.renamed.unwrap_or_else(|| file.title != default_title(&file.app, file.started_at))
}

fn read_meta(dir: &Path) -> Result<MeetingFile> {
    Ok(serde_json::from_str(&fs::read_to_string(dir.join(META))?)?)
}

fn write_meta(dir: &Path, file: &MeetingFile) -> Result<()> {
    write_atomic(&dir.join(META), serde_json::to_string_pretty(file)?.as_bytes())
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    Ok(crate::xdg::write_atomic(path, contents)?)
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() { "meeting".into() } else { out.into() }
}
