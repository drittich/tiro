//! Meeting screenshots: which frames of the meeting window to keep, and how they are stored. Pure: the Tauri
//! adapter captures the frames through KWin and writes the files. The rule and its thresholds come from
//! ticket 09's measurements, and its handling of cameras from ticket 26's.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::transcript::Segment;

/// How often the meeting window is captured.
pub const POLL: Duration = Duration::from_secs(2);
/// How often capture asks KWin for the window list again, to follow a meeting that moves to a new window.
pub const WINDOW_REFRESH: Duration = Duration::from_secs(20);
/// Where screenshots go in the Meeting folder.
pub const DIR: &str = "screenshots";

const THUMBNAIL_WIDTH: usize = 320;
/// A thumbnail pixel has changed when its grey level moves by more than this.
const PIXEL_CHANGE: u8 = 32;
/// The share of changed pixels, against the last kept frame and outside live cells, that makes a frame worth
/// keeping. Low enough for one added bullet (about 0.13 %), now that camera tiles and other moving parts are
/// left out of the count (ticket 26).
const CHANGED: f64 = 0.001;
/// The most a frame can differ from the previous poll and still count as settled.
const SETTLED: f64 = 0.01;
/// Content that has not settled for this long gets one frame anyway (video, scrolling).
const MOTION_AFTER_MS: u64 = 4_000;
/// The least time between two such frames.
const MOTION_EVERY_MS: u64 = 60_000;
/// Liveness is tracked in squares of this many thumbnail pixels.
const CELL: usize = 16;
/// A cell that moves again within this long of its last move (three polls) is live: a camera tile or a video,
/// not a slide, whose changes are at least 8 s apart. Counted in time rather than polls, so thinning, which sees
/// only the Screenshots kept, judges the same way.
const LIVE_WITHIN_MS: u64 = 6_000;
/// A live cell stays live this long after its last move, so a camera whose subject sits still a while
/// doesn't count as new content when they move again.
const LIVE_FOR_MS: u64 = 120_000;
/// Cells this close to a live one are left out too: a head moving into the next cell isn't new content.
const LIVE_MARGIN: usize = 2;
/// While anything is live, one frame is kept this often anyway, to show who was on screen.
const PERIODIC_MS: u64 = 300_000;
/// A change this large between polls is a new layout (a share starting, someone joining), and forgets which
/// cells were live.
const LAYOUT_CHANGE: f64 = 0.25;
/// Wider frames are scaled down to this before encoding.
const MAX_WIDTH: usize = 1920;
const WEBP_QUALITY: f32 = 80.0;
/// A pixel this opaque or more belongs to the window rather than its client-side shadow.
const OPAQUE: u8 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// Puts a frame KWin's ScreenShot2 sent in QImage format `format` into BGRA, the byte order `Frame` reads,
/// in place. RGB32, ARGB32 and ARGB32_Premultiplied (4–6) are BGRA in memory already; RGBX8888, RGBA8888 and
/// RGBA8888_Premultiplied (16–18), which KWin 6.8 sends, have red and blue swapped. Returns `false`, leaving
/// `data` alone, for any other format. `data` must hold `height` rows of `stride` bytes, the last of which
/// may stop after `width` pixels.
pub fn into_bgra(format: u32, data: &mut [u8], width: usize, height: usize, stride: usize) -> bool {
    match format {
        4..=6 => true,
        16..=18 => {
            for y in 0..height {
                for pixel in data[y * stride..][..width * 4].chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
            }
            true
        }
        _ => false,
    }
}

/// A raw frame as KWin's ScreenShot2 sends it, put into BGRA by `into_bgra`, with `stride` bytes per row.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    pub data: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

impl<'a> Frame<'a> {
    fn pixel(&self, x: usize, y: usize) -> &[u8] {
        let at = y * self.stride + x * 4;
        &self.data[at..at + 4]
    }

    /// The smallest rectangle holding every opaque pixel, or `None` if there are none. Scans inwards from each
    /// edge, so a window with a thin shadow costs only a few rows and columns.
    pub fn opaque_bounds(&self) -> Option<Rect> {
        let opaque = |x: usize, y: usize| self.pixel(x, y)[3] >= OPAQUE;
        let row = |y: usize| (0..self.width).any(|x| opaque(x, y));
        let top = (0..self.height).find(|&y| row(y))?;
        let bottom = (top..self.height).rev().find(|&y| row(y))?;
        let column = |x: usize| (top..=bottom).any(|y| opaque(x, y));
        let left = (0..self.width).find(|&x| column(x))?;
        let right = (left..self.width).rev().find(|&x| column(x))?;
        Some(Rect { x: left, y: top, width: right - left + 1, height: bottom - top + 1 })
    }

    /// The part of the frame inside `rect`, without copying.
    pub fn crop(&self, rect: Rect) -> Frame<'a> {
        let start = rect.y * self.stride + rect.x * 4;
        Frame { data: &self.data[start..], width: rect.width, height: rect.height, stride: self.stride }
    }

    /// A small grayscale copy that frames are compared by.
    pub fn thumbnail(&self) -> Thumbnail {
        let width = THUMBNAIL_WIDTH.min(self.width);
        let height = scaled_height(self.height, self.width, width);
        let rgb = self.box_scale(width, height);
        let pixels = rgb.chunks_exact(3).map(|p| ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8).collect();
        Thumbnail { width, height, pixels }
    }

    /// Lossy WebP, scaled down to at most 1920 px wide. Transparency is dropped: crop to the opaque area first.
    pub fn encode_webp(&self) -> Vec<u8> {
        let width = MAX_WIDTH.min(self.width);
        let height = scaled_height(self.height, self.width, width);
        let rgb = self.box_scale(width, height);
        webp::Encoder::from_rgb(&rgb, width as u32, height as u32).encode(WEBP_QUALITY).to_vec()
    }

    /// RGB at `width`×`height`, each output pixel the average of the frame pixels it covers.
    fn box_scale(&self, width: usize, height: usize) -> Vec<u8> {
        let mut sums = vec![[0u32; 4]; width * height];
        let columns: Vec<usize> = (0..self.width).map(|x| x * width / self.width).collect();
        for y in 0..self.height {
            let row = y * height / self.height * width;
            let pixels = &self.data[y * self.stride..][..self.width * 4];
            for (p, column) in pixels.chunks_exact(4).zip(&columns) {
                let sum = &mut sums[row + column];
                sum[0] += p[2] as u32;
                sum[1] += p[1] as u32;
                sum[2] += p[0] as u32;
                sum[3] += 1;
            }
        }
        sums.iter().flat_map(|s| (0..3).map(move |c| (s[c] / s[3].max(1)) as u8)).collect()
    }
}

fn scaled_height(height: usize, from_width: usize, to_width: usize) -> usize {
    ((height * to_width + from_width / 2) / from_width).max(1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: usize,
    pub height: usize,
    /// Grey levels, row by row.
    pub pixels: Vec<u8>,
}

impl Thumbnail {
    /// The share of pixels that changed between the two; 1.0 when their sizes differ (the window was resized).
    pub fn changed(&self, other: &Thumbnail) -> f64 {
        self.changed_outside(other, &[])
    }

    /// Like [`Thumbnail::changed`], but pixels where `ignored` is true don't count as changed. They still count
    /// in the total, so a share means the same with or without them. An empty `ignored` ignores none.
    fn changed_outside(&self, other: &Thumbnail, ignored: &[bool]) -> f64 {
        if (self.width, self.height) != (other.width, other.height) || self.pixels.is_empty() {
            return 1.0;
        }
        let counted = |i: usize| ignored.get(i).is_none_or(|ignored| !ignored);
        let pairs = self.pixels.iter().zip(&other.pixels).enumerate();
        let changed = pairs.filter(|(i, (a, b))| a.abs_diff(**b) > PIXEL_CHANGE && counted(*i)).count();
        changed as f64 / self.pixels.len() as f64
    }
}

/// Which parts of the window keep moving, such as camera tiles and video, tracked in cells of the thumbnail.
#[derive(Debug, Default)]
struct Liveness {
    /// The thumbnail size the cells cover.
    size: (usize, usize),
    columns: usize,
    /// Per cell, the Meeting time (ms) it last moved.
    last_moved: Vec<Option<u64>>,
    /// Per cell, the Meeting time (ms) until which it is live.
    live_until: Vec<Option<u64>>,
}

impl Liveness {
    fn new(width: usize, height: usize) -> Self {
        let (columns, rows) = (width.div_ceil(CELL), height.div_ceil(CELL));
        Liveness { size: (width, height), columns, last_moved: vec![None; columns * rows], live_until: vec![None; columns * rows] }
    }

    /// Takes in the poll at `at_ms` and returns, per thumbnail pixel, whether it is in or near a live cell.
    fn update(&mut self, current: &Thumbnail, previous: &Thumbnail, at_ms: u64) -> Vec<bool> {
        let new_layout = current.changed(previous) >= LAYOUT_CHANGE;
        if new_layout || self.size != (current.width, current.height) {
            *self = Liveness::new(current.width, current.height);
        }
        if new_layout {
            return Vec::new();
        }
        let mut moved = vec![false; self.last_moved.len()];
        for (i, (a, b)) in current.pixels.iter().zip(&previous.pixels).enumerate() {
            if a.abs_diff(*b) > PIXEL_CHANGE {
                moved[self.cell_of(i)] = true;
            }
        }
        for cell in (0..moved.len()).filter(|&c| moved[c]) {
            if self.live(cell, at_ms) || self.last_moved[cell].is_some_and(|last| at_ms - last <= LIVE_WITHIN_MS) {
                self.live_until[cell] = Some(at_ms + LIVE_FOR_MS);
            }
            self.last_moved[cell] = Some(at_ms);
        }
        self.near_live(at_ms)
    }

    fn near_live(&self, at_ms: u64) -> Vec<bool> {
        let (width, height) = self.size;
        let rows = self.last_moved.len() / self.columns;
        let mut near = vec![false; self.last_moved.len()];
        for cell in (0..self.last_moved.len()).filter(|&c| self.live(c, at_ms)) {
            let (row, column) = (cell / self.columns, cell % self.columns);
            for r in row.saturating_sub(LIVE_MARGIN)..(row + LIVE_MARGIN + 1).min(rows) {
                for c in column.saturating_sub(LIVE_MARGIN)..(column + LIVE_MARGIN + 1).min(self.columns) {
                    near[r * self.columns + c] = true;
                }
            }
        }
        (0..width * height).map(|i| near[self.cell_of(i)]).collect()
    }

    fn live(&self, cell: usize, at_ms: u64) -> bool {
        self.live_until[cell].is_some_and(|until| at_ms <= until)
    }

    /// The cell holding thumbnail pixel `i`.
    fn cell_of(&self, i: usize) -> usize {
        (i / self.size.0) / CELL * self.columns + (i % self.size.0) / CELL
    }
}

/// Why a frame was kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    First,
    /// It differs from the last kept frame, and stopped changing.
    Changed,
    /// The content kept changing, so one frame was kept anyway.
    Motion,
    /// Parts of the window keep moving (cameras), so one frame is kept every five minutes to show them.
    Periodic,
}

/// Decides which polled frames to keep. A frame is kept when enough of it differs from the last kept frame
/// and it has settled since the previous poll, so a slide is kept once, after it finishes drawing. Parts that
/// keep moving, such as camera tiles, are left out of both comparisons (ticket 26).
#[derive(Debug, Default)]
pub struct KeepRule {
    last_kept: Option<Thumbnail>,
    /// The Meeting time (ms) of the last kept frame.
    last_kept_at: u64,
    previous: Option<Thumbnail>,
    /// The Meeting time (ms) of the first poll in the current run of unsettled ones.
    unsettled_since: Option<u64>,
    last_motion: Option<u64>,
    liveness: Liveness,
}

impl KeepRule {
    /// Offers the frame polled at `at_ms` (Meeting time); returns why to keep it, or `None` to drop it.
    pub fn offer(&mut self, thumbnail: Thumbnail, at_ms: u64) -> Option<Keep> {
        self.judge(thumbnail, at_ms, false)
    }

    /// Like [`KeepRule::offer`], for a stored Screenshot: it had settled when it was kept, which comparing it
    /// with the Screenshot before it, rather than the poll before it, can't tell.
    pub fn offer_settled(&mut self, thumbnail: Thumbnail, at_ms: u64) -> Option<Keep> {
        self.judge(thumbnail, at_ms, true)
    }

    fn judge(&mut self, thumbnail: Thumbnail, at_ms: u64, settled: bool) -> Option<Keep> {
        let keep = match (&self.last_kept, &self.previous) {
            (Some(kept), Some(previous)) => {
                let ignored = self.liveness.update(&thumbnail, previous, at_ms);
                if settled || thumbnail.changed_outside(previous, &ignored) <= SETTLED {
                    self.unsettled_since = None;
                    if thumbnail.changed_outside(kept, &ignored) >= CHANGED {
                        Some(Keep::Changed)
                    } else {
                        let periodic = ignored.contains(&true) && at_ms - self.last_kept_at >= PERIODIC_MS;
                        periodic.then_some(Keep::Periodic)
                    }
                } else {
                    let since = *self.unsettled_since.get_or_insert(at_ms);
                    let due = at_ms - since >= MOTION_AFTER_MS && self.last_motion.is_none_or(|t| at_ms - t >= MOTION_EVERY_MS);
                    due.then(|| {
                        self.last_motion = Some(at_ms);
                        Keep::Motion
                    })
                }
            }
            _ => Some(Keep::First),
        };
        if keep.is_some() {
            self.last_kept = Some(thumbnail.clone());
            self.last_kept_at = at_ms;
        }
        self.previous = Some(thumbnail);
        keep
    }
}

/// `NNNNN-TTTTTTTTT.webp`: the screenshot's index and its Meeting time in ms, padded like Chunk names.
pub fn file_name(index: u32, ms: u64) -> String {
    format!("{index:05}-{ms:09}.webp")
}

/// The index and Meeting time (ms) in a screenshot's file name.
pub fn parse_file_name(name: &str) -> Option<(u32, u64)> {
    let (index, ms) = name.strip_suffix(".webp")?.split_once('-')?;
    let digits = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(index, 5) || !digits(ms, 9) {
        return None;
    }
    Some((index.parse().ok()?, ms.parse().ok()?))
}

/// A Screenshot stored in a Meeting folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot {
    pub index: u32,
    /// Meeting time in ms; `ms / 1000.0` is on the same timeline as a Segment's `start`.
    pub ms: u64,
    pub path: PathBuf,
}

impl Screenshot {
    pub fn file_name(&self) -> String {
        file_name(self.index, self.ms)
    }
}

/// The Screenshots in the Meeting folder `meeting_dir`, by time. Files whose names don't parse are ignored, and a
/// missing `screenshots/` means none.
pub fn list(meeting_dir: &Path) -> std::io::Result<Vec<Screenshot>> {
    let dir = meeting_dir.join(DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut screenshots = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some((index, ms)) = entry.file_name().to_str().and_then(parse_file_name) else { continue };
        screenshots.push(Screenshot { index, ms, path: entry.path() });
    }
    screenshots.sort_by_key(|s| (s.ms, s.index));
    Ok(screenshots)
}

/// The Meeting time a Meeting card's Screenshot is taken nearest to: by five minutes in, the meeting has usually
/// settled and any share has started (ticket 27).
const CARD_SCREENSHOT_AT_MS: u64 = 300_000;

/// The Screenshot the Meetings list shows for a Meeting: of `screenshots`, by time, the one nearest to five minutes
/// in, the earlier on a tie. A Meeting shorter than that gets its last. `None` when there are none.
pub fn card_screenshot(screenshots: &[Screenshot]) -> Option<&Screenshot> {
    screenshots.iter().min_by_key(|s| s.ms.abs_diff(CARD_SCREENSHOT_AT_MS))
}

/// In `screenshots/`, marks Screenshots already kept by the current rule, so [`thin`] leaves them alone. The name
/// doesn't parse as a Screenshot, so listings skip it.
const THINNED: &str = ".thinned";

/// Marks the Meeting folder `meeting_dir` as thinned, creating its `screenshots/`. Capture calls this before its
/// first Screenshot.
pub fn mark_thinned(meeting_dir: &Path) -> std::io::Result<()> {
    let dir = meeting_dir.join(DIR);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(THINNED), b"")
}

/// Thins the Screenshots of a Meeting recorded before ticket 26: replays them through [`KeepRule`], marks the
/// folder so this happens once, then deletes those the rule dropped. Returns how many were deleted. A Screenshot that
/// can't be decoded is kept and not offered to the rule.
pub fn thin(meeting_dir: &Path) -> std::io::Result<usize> {
    if !meeting_dir.join(DIR).is_dir() || meeting_dir.join(DIR).join(THINNED).exists() {
        return Ok(0);
    }
    let mut rule = KeepRule::default();
    let mut dropped = Vec::new();
    for screenshot in list(meeting_dir)? {
        let Some(thumbnail) = decode_thumbnail(&std::fs::read(&screenshot.path)?) else { continue };
        if rule.offer_settled(thumbnail, screenshot.ms).is_none() {
            dropped.push(screenshot.path);
        }
    }
    // Marked before deleting: a run cut short leaves extra Screenshots, rather than thinning the rest again.
    mark_thinned(meeting_dir)?;
    for path in &dropped {
        std::fs::remove_file(path)?;
    }
    Ok(dropped.len())
}

/// The thumbnail of a stored WebP Screenshot, or `None` if it doesn't decode.
fn decode_thumbnail(webp: &[u8]) -> Option<Thumbnail> {
    let image = webp::Decoder::new(webp).decode()?;
    let (width, height) = (image.width() as usize, image.height() as usize);
    let channels = if image.is_alpha() { 4 } else { 3 };
    let bgra: Vec<u8> = image.chunks_exact(channels).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
    Some(Frame { data: &bgra, width, height, stride: width * 4 }.thumbnail())
}

/// Where a Screenshot taken at `ms` goes in the shown (`cleaned`) Transcript: the index of the first Segment
/// that starts after it, so it follows every Segment starting at or before it. The view and the export share it.
pub fn position(segments: &[Segment], ms: u64) -> usize {
    let at = ms as f64 / 1000.0;
    segments.iter().position(|s| s.start > at).unwrap_or(segments.len())
}
