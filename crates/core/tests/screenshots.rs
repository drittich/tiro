use tiro_core::screenshots::{file_name, mark_thinned, parse_file_name, thin, card_screenshot, into_bgra, Frame, Keep, KeepRule, Rect, Screenshot, Thumbnail};

/// A 100×20 thumbnail (2,000 pixels) of one grey level, with `changed` pixels set to another.
fn thumb(base: u8, changed: &[usize], to: u8) -> Thumbnail {
    let mut pixels = vec![base; 2000];
    for &i in changed {
        pixels[i] = to;
    }
    Thumbnail { width: 100, height: 20, pixels }
}

fn range(from: usize, count: usize) -> Vec<usize> {
    (from..from + count).collect()
}

#[test]
fn the_first_frame_is_always_kept_and_an_unchanged_window_adds_nothing() {
    let mut rule = KeepRule::default();
    assert_eq!(rule.offer(thumb(0, &[], 0), 0), Some(Keep::First));
    for at in 1..30 {
        assert_eq!(rule.offer(thumb(0, &[], 0), at * 2000), None);
    }
}

#[test]
fn a_change_is_kept_once_it_has_settled() {
    let mut rule = KeepRule::default();
    rule.offer(thumb(0, &[], 0), 0);
    // A new slide: 2.5% of pixels change, so it hasn't settled against the previous poll yet.
    let slide = thumb(0, &range(0, 50), 200);
    assert_eq!(rule.offer(slide.clone(), 2000), None);
    assert_eq!(rule.offer(slide.clone(), 4000), Some(Keep::Changed));
    assert_eq!(rule.offer(slide, 6000), None, "kept once");
}

#[test]
fn a_change_that_lands_between_polls_is_kept_at_the_next_poll() {
    let mut rule = KeepRule::default();
    rule.offer(thumb(0, &[], 0), 0);
    // One bullet appears: 0.25% against the last kept frame, and settled (also 0.25% against the previous poll).
    assert_eq!(rule.offer(thumb(0, &range(0, 5), 200), 2000), Some(Keep::Changed));
}

#[test]
fn changes_below_the_threshold_add_up_against_the_last_kept_frame() {
    let mut rule = KeepRule::default();
    rule.offer(thumb(0, &[], 0), 0);
    assert_eq!(rule.offer(thumb(0, &[0], 200), 2000), None, "0.05% is below 0.1%");
    assert_eq!(rule.offer(thumb(0, &[0, 99], 200), 4000), Some(Keep::Changed), "0.1% since the last kept frame");
}

#[test]
fn small_brightness_noise_does_not_count_as_change() {
    let mut rule = KeepRule::default();
    rule.offer(thumb(100, &[], 0), 0);
    assert_eq!(rule.offer(thumb(120, &[], 0), 2000), None, "every pixel moved, but by 20 levels, under 32");
}

#[test]
fn a_video_filling_the_window_keeps_one_screenshot_after_four_seconds_then_at_most_one_a_minute() {
    let mut rule = KeepRule::default();
    rule.offer(thumb(0, &[], 0), 0);
    // Video: 60% of the frame changes on every poll and never settles. That is too much to be cameras.
    let frame = |at: u64| thumb(0, &range((at / 2000) as usize % 2 * 1000, 600), 200);
    let kept: Vec<(u64, Keep)> = (1..=60).map(|n| n * 2000).filter_map(|at| rule.offer(frame(at), at).map(|k| (at, k))).collect();
    // Unsettled from the poll at 2 s, so 4 s later at 6 s; then 60 s after that.
    assert_eq!(kept, vec![(6000, Keep::Motion), (66000, Keep::Motion)]);
}

#[test]
fn a_video_filling_the_window_that_pauses_keeps_the_screenshot_it_paused_on() {
    let mut rule = KeepRule::default();
    rule.offer(thumb(0, &[], 0), 0);
    rule.offer(thumb(0, &range(0, 600), 200), 2000);
    let paused = thumb(0, &range(1000, 600), 200);
    assert_eq!(rule.offer(paused.clone(), 4000), None, "still moving");
    assert_eq!(rule.offer(paused, 6000), Some(Keep::Changed));
}

/// A 320×180 thumbnail like a meeting window's: a dark background, a camera tile on the left (x < 96) in which a
/// 16×24 head sits at `head`, and, from x = 160, a slide showing its first `bullets` lines.
fn window(head: (usize, usize), bullets: usize) -> Thumbnail {
    let (width, height) = (320, 180);
    let mut pixels = vec![30; width * height];
    let mut fill = |x0: usize, y0: usize, w: usize, h: usize, level: u8| {
        for y in y0..y0 + h {
            pixels[y * width + x0..y * width + x0 + w].fill(level);
        }
    };
    fill(0, 40, 96, 96, 120);
    fill(head.0, head.1, 16, 24, 220);
    fill(160, 20, 150, 140, 250);
    for line in 0..bullets {
        fill(170, 40 + line * 16, 100, 4, 40);
    }
    Thumbnail { width, height, pixels }
}

/// Where the head is at poll `n`: it never sits still, swaying across the tile.
fn sway(n: u64) -> (usize, usize) {
    (20 + (n as usize * 7) % 50, 70 + (n as usize * 3) % 20)
}

/// What a fresh rule keeps of polls `0..=last`, 2 s apart, with `head` giving where the head is at each.
fn kept_over(last: u64, head: impl Fn(u64) -> (usize, usize)) -> Vec<(u64, Keep)> {
    let mut rule = KeepRule::default();
    (0..=last).filter_map(|n| rule.offer(window(head(n), 1), n * 2000).map(|k| (n * 2000, k))).collect()
}

#[test]
fn a_camera_that_keeps_moving_stops_adding_screenshots_once_it_is_seen_moving() {
    let kept = kept_over(119, sway);
    // The first move can't yet be told from new content; from the second, the tile is live and ignored.
    assert_eq!(kept, vec![(0, Keep::First), (2000, Keep::Changed)]);
}

#[test]
fn while_a_camera_keeps_moving_a_frame_is_kept_every_five_minutes_to_show_who_was_on_screen() {
    let kept = kept_over(330, sway);
    assert_eq!(kept, vec![(0, Keep::First), (2000, Keep::Changed), (302_000, Keep::Periodic), (602_000, Keep::Periodic)]);
}

#[test]
fn a_bullet_added_beside_a_moving_camera_is_kept() {
    let mut rule = KeepRule::default();
    for n in 0..10 {
        rule.offer(window(sway(n), 1), n * 2000);
    }
    assert_eq!(rule.offer(window(sway(10), 2), 20_000), Some(Keep::Changed));
    assert_eq!(rule.offer(window(sway(11), 2), 22_000), None, "kept once");
}

#[test]
fn a_camera_that_stops_moving_counts_again_two_minutes_after_its_last_move() {
    // The head sways until poll 9 (18 s), then stays where it is.
    let kept = kept_over(100, |n| sway(n.min(9)));
    assert_eq!(kept, vec![(0, Keep::First), (2000, Keep::Changed), (140_000, Keep::Changed)]);
}

/// The same window once someone shares: the shared slide fills it, with its first `bullets` lines from the left
/// edge, where the camera tile was.
fn shared(bullets: usize) -> Thumbnail {
    let mut slide = Thumbnail { width: 320, height: 180, pixels: vec![250; 320 * 180] };
    for line in 0..bullets {
        for y in 40 + line * 16..44 + line * 16 {
            slide.pixels[y * 320 + 10..y * 320 + 110].fill(40);
        }
    }
    slide
}

#[test]
fn a_share_replacing_the_cameras_is_kept_and_its_slides_count_where_the_camera_was() {
    let mut rule = KeepRule::default();
    for n in 0..10 {
        rule.offer(window(sway(n), 1), n * 2000);
    }
    assert_eq!(rule.offer(shared(1), 20_000), None, "not settled yet");
    assert_eq!(rule.offer(shared(1), 22_000), Some(Keep::Changed));
    assert_eq!(rule.offer(shared(2), 24_000), Some(Keep::Changed), "a bullet where the camera tile was");
}

#[test]
fn a_thumbnail_of_a_different_size_counts_as_fully_changed() {
    let small = Thumbnail { width: 10, height: 10, pixels: vec![0; 100] };
    assert_eq!(small.changed(&thumb(0, &[], 0)), 1.0);
}

/// A BGRA frame with a 2-pixel transparent border around an opaque area that is white on the left half and
/// black on the right. Rows are padded to `stride`.
fn frame_bytes(width: usize, height: usize, stride: usize) -> Vec<u8> {
    let mut data = vec![0u8; stride * height];
    for y in 2..height - 2 {
        for x in 2..width - 2 {
            let level = if x < width / 2 { 255 } else { 0 };
            let at = y * stride + x * 4;
            data[at..at + 4].copy_from_slice(&[level, level, level, 255]);
        }
    }
    data
}

#[test]
fn the_transparent_shadow_around_a_window_is_cropped_away() {
    let data = frame_bytes(20, 10, 96);
    let frame = Frame { data: &data, width: 20, height: 10, stride: 96 };
    assert_eq!(frame.opaque_bounds(), Some(Rect { x: 2, y: 2, width: 16, height: 6 }));

    let blank = vec![0u8; 96 * 10];
    assert_eq!(Frame { data: &blank, width: 20, height: 10, stride: 96 }.opaque_bounds(), None);
}

#[test]
fn the_thumbnail_is_320_wide_grayscale_and_averages_the_pixels_it_covers() {
    let data = frame_bytes(1284, 404, 1284 * 4);
    let frame = Frame { data: &data, width: 1284, height: 404, stride: 1284 * 4 };
    let frame = frame.crop(frame.opaque_bounds().unwrap());
    assert_eq!((frame.width, frame.height), (1280, 400));

    let thumbnail = frame.thumbnail();
    assert_eq!((thumbnail.width, thumbnail.height), (320, 100));
    assert_eq!(thumbnail.pixels[0], 255);
    assert_eq!(thumbnail.pixels[319], 0);
}

#[test]
fn a_kept_frame_is_encoded_as_webp_at_most_1920_wide() {
    let data = frame_bytes(2404, 104, 2404 * 4);
    let frame = Frame { data: &data, width: 2404, height: 104, stride: 2404 * 4 };
    let frame = frame.crop(frame.opaque_bounds().unwrap());

    let webp = frame.encode_webp();
    assert_eq!(&webp[..4], b"RIFF");
    assert_eq!(&webp[8..12], b"WEBP");
    let image = webp::Decoder::new(&webp).decode().unwrap();
    assert_eq!((image.width(), image.height()), (1920, 80));

    let small = vec![255u8; 40 * 30 * 4];
    let image = Frame { data: &small, width: 40, height: 30, stride: 160 }.encode_webp();
    assert_eq!(webp::Decoder::new(&image).decode().unwrap().width(), 40, "never scaled up");
}

#[test]
fn file_names_carry_the_index_and_the_meeting_time_in_ms() {
    assert_eq!(file_name(7, 152_250), "00007-000152250.webp");
    assert_eq!(parse_file_name("00007-000152250.webp"), Some((7, 152_250)));
    assert_eq!(parse_file_name("00007-000152250.webp.tmp"), None);
    assert_eq!(parse_file_name("others-00007-000152250-000181500.ogg"), None);
}

fn store() -> (tempfile::TempDir, tiro_core::store::MeetingStore, tiro_core::store::Meeting) {
    let dir = tempfile::tempdir().unwrap();
    let store = tiro_core::store::MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("i.sqlite")).unwrap();
    let started = chrono::DateTime::parse_from_rfc3339("2026-10-20T09:03:00-04:00").unwrap();
    let meeting = store.create("Teams", started).unwrap();
    (dir, store, meeting)
}

#[test]
fn a_meetings_screenshots_are_listed_by_time_without_stray_files() {
    let (_dir, store, meeting) = store();
    let shots = meeting.dir.join("screenshots");
    std::fs::create_dir(&shots).unwrap();
    for name in ["00002-000090000.webp", "00000-000001500.webp", "00001-000004000.webp", "notes.txt", "00003-12.webp", ".00004-000095000.webp.tmp"] {
        std::fs::write(shots.join(name), name).unwrap();
    }

    let listed: Vec<(u32, u64, std::path::PathBuf)> =
        store.screenshots(&meeting).unwrap().into_iter().map(|s| (s.index, s.ms, s.path)).collect();

    assert_eq!(
        listed,
        [
            (0, 1_500, shots.join("00000-000001500.webp")),
            (1, 4_000, shots.join("00001-000004000.webp")),
            (2, 90_000, shots.join("00002-000090000.webp")),
        ]
    );
}

/// `thumbnail` as a stored Screenshot: grey BGRA, encoded the way capture encodes it.
fn webp_of(thumbnail: &Thumbnail) -> Vec<u8> {
    let data: Vec<u8> = thumbnail.pixels.iter().flat_map(|&p| [p, p, p, 255]).collect();
    Frame { data: &data, width: thumbnail.width, height: thumbnail.height, stride: thumbnail.width * 4 }.encode_webp()
}

#[test]
fn thinning_replays_a_meetings_screenshots_through_the_rule_once_and_deletes_the_ones_it_drops() {
    let (_dir, store, meeting) = store();
    let shots = meeting.dir.join("screenshots");
    std::fs::create_dir(&shots).unwrap();
    for n in 0..20u64 {
        std::fs::write(shots.join(file_name(n as u32, n * 2000)), webp_of(&window(sway(n), 1))).unwrap();
    }
    std::fs::write(shots.join("notes.txt"), "not a Screenshot").unwrap();

    assert_eq!(thin(&meeting.dir).unwrap(), 18);
    let left: Vec<u32> = store.screenshots(&meeting).unwrap().iter().map(|s| s.index).collect();
    assert_eq!(left, [0, 1]);
    assert!(shots.join("notes.txt").exists());

    assert_eq!(thin(&meeting.dir).unwrap(), 0, "a Meeting is thinned only once");
    assert_eq!(store.screenshots(&meeting).unwrap().len(), 2);
}

#[test]
fn thinning_keeps_every_slide_of_a_share_since_each_screenshot_had_settled_when_it_was_kept() {
    let (_dir, store, meeting) = store();
    let shots = meeting.dir.join("screenshots");
    std::fs::create_dir(&shots).unwrap();
    // Slides 30 s apart, each one a large change from the one before, then a build step on the last.
    let slides = [shared(1), window((20, 70), 1), shared(3), window((20, 70), 4), window((20, 70), 5)];
    for (n, slide) in slides.iter().enumerate() {
        std::fs::write(shots.join(file_name(n as u32, n as u64 * 30_000)), webp_of(slide)).unwrap();
    }

    assert_eq!(thin(&meeting.dir).unwrap(), 0);
    assert_eq!(store.screenshots(&meeting).unwrap().len(), 5);
}

#[test]
fn a_meeting_marked_thinned_or_without_screenshots_is_left_alone() {
    let (_dir, store, meeting) = store();
    assert_eq!(thin(&meeting.dir).unwrap(), 0);
    assert!(!meeting.dir.join("screenshots").exists());

    // Capture marks the folders it starts, since its Screenshots were kept by the current rule.
    mark_thinned(&meeting.dir).unwrap();
    let shots = meeting.dir.join("screenshots");
    for n in 0..5u64 {
        std::fs::write(shots.join(file_name(n as u32, n * 2000)), webp_of(&window(sway(n), 1))).unwrap();
    }
    assert_eq!(thin(&meeting.dir).unwrap(), 0);
    assert_eq!(store.screenshots(&meeting).unwrap().len(), 5);
}

#[test]
fn a_meeting_without_a_screenshots_folder_has_none() {
    let (_dir, store, meeting) = store();
    assert!(store.screenshots(&meeting).unwrap().is_empty());
}

fn segments(starts: &[f64]) -> Vec<tiro_core::transcript::Segment> {
    starts
        .iter()
        .map(|&start| tiro_core::transcript::Segment {
            track: tiro_core::transcript::Track::Others,
            start,
            end: start + 1.0,
            text: "Words.".into(),
            speaker: None,
        })
        .collect()
}

#[test]
fn a_screenshot_goes_after_the_segments_that_start_at_or_before_it() {
    use tiro_core::screenshots::position;
    let transcript = segments(&[2.0, 5.0, 5.0, 9.5]);
    assert_eq!(position(&transcript, 1_999), 0, "before the first Segment");
    assert_eq!(position(&transcript, 2_000), 1, "a Segment starting at the same time comes first");
    assert_eq!(position(&transcript, 4_999), 1);
    assert_eq!(position(&transcript, 5_000), 3, "after both Segments starting then");
    assert_eq!(position(&transcript, 9_400), 3);
    assert_eq!(position(&transcript, 60_000), 4, "after the last Segment");
}

#[test]
fn with_an_empty_transcript_every_screenshot_goes_at_the_start() {
    assert_eq!(tiro_core::screenshots::position(&[], 12_000), 0);
}

#[test]
fn only_a_listed_screenshot_of_a_meeting_can_be_looked_up_by_name() {
    let (_dir, store, meeting) = store();
    let shots = meeting.dir.join("screenshots");
    std::fs::create_dir(&shots).unwrap();
    std::fs::write(shots.join("00000-000001500.webp"), b"x").unwrap();
    std::fs::write(shots.join("stray.webp"), b"x").unwrap();
    std::fs::write(meeting.dir.join("00000-000001500.webp"), b"x").unwrap();

    let found = store.screenshot(&meeting.id, "00000-000001500.webp").unwrap();
    assert_eq!(found.map(|s| s.path), Some(shots.join("00000-000001500.webp")));
    assert_eq!(store.screenshot(&meeting.id, "stray.webp").unwrap(), None);
    assert_eq!(store.screenshot(&meeting.id, "../00000-000001500.webp").unwrap(), None);
    assert!(store.screenshot("..", "00000-000001500.webp").is_err());
    assert!(store.screenshot(&format!("{}/screenshots", meeting.id), "00000-000001500.webp").is_err());
    assert!(store.screenshot("no-such-meeting", "00000-000001500.webp").is_err());
}

#[test]
fn deleting_a_screenshot_removes_its_file_and_leaves_the_others() {
    let (_dir, store, meeting) = store();
    let shots = meeting.dir.join("screenshots");
    tiro_core::screenshots::mark_thinned(&meeting.dir).unwrap();
    for name in ["00000-000001500.webp", "00001-000004000.webp"] {
        std::fs::write(shots.join(name), b"x").unwrap();
    }

    assert!(store.delete_screenshot(&meeting.id, "00000-000001500.webp").unwrap());

    let left: Vec<String> = store.screenshots(&meeting).unwrap().iter().map(|s| s.file_name()).collect();
    assert_eq!(left, ["00001-000004000.webp"]);
    assert!(shots.join(".thinned").exists(), "the thinned marker stays, so thinning never runs again");
    assert!(!store.delete_screenshot(&meeting.id, "00000-000001500.webp").unwrap(), "already gone");
}

#[test]
fn only_a_listed_screenshot_of_a_meeting_can_be_deleted() {
    let (_dir, store, meeting) = store();
    let shots = meeting.dir.join("screenshots");
    std::fs::create_dir(&shots).unwrap();
    std::fs::write(shots.join("stray.webp"), b"x").unwrap();
    std::fs::write(meeting.dir.join("00000-000001500.webp"), b"x").unwrap();

    assert!(!store.delete_screenshot(&meeting.id, "stray.webp").unwrap());
    assert!(!store.delete_screenshot(&meeting.id, "../00000-000001500.webp").unwrap());
    assert!(store.delete_screenshot("..", "00000-000001500.webp").is_err());
    assert!(shots.join("stray.webp").exists());
    assert!(meeting.dir.join("00000-000001500.webp").exists());
}

/// Screenshots at these Meeting times, in order.
fn shots_at(times: &[u64]) -> Vec<Screenshot> {
    times.iter().enumerate().map(|(i, &ms)| Screenshot { index: i as u32, ms, path: file_name(i as u32, ms).into() }).collect()
}

fn picked_ms(times: &[u64]) -> Option<u64> {
    card_screenshot(&shots_at(times)).map(|s| s.ms)
}

#[test]
fn a_meeting_without_screenshots_has_no_thumbnail_and_one_with_a_single_screenshot_uses_it() {
    assert_eq!(picked_ms(&[]), None);
    assert_eq!(picked_ms(&[42_000]), Some(42_000));
    assert_eq!(picked_ms(&[900_000]), Some(900_000));
}

#[test]
fn a_meeting_shorter_than_five_minutes_shows_its_last_screenshot() {
    assert_eq!(picked_ms(&[0, 60_000, 180_000, 299_000]), Some(299_000));
}

#[test]
fn when_every_screenshot_is_after_five_minutes_the_first_is_nearest() {
    assert_eq!(picked_ms(&[310_000, 600_000, 1_200_000]), Some(310_000));
}

#[test]
fn the_screenshot_nearest_five_minutes_is_shown() {
    assert_eq!(picked_ms(&[0, 240_000, 330_000, 900_000]), Some(330_000));
    assert_eq!(picked_ms(&[0, 280_000, 330_000, 900_000]), Some(280_000));
}

#[test]
fn on_a_tie_the_earlier_screenshot_is_shown() {
    assert_eq!(picked_ms(&[0, 290_000, 310_000, 900_000]), Some(290_000));
}

#[test]
fn an_argb32_frame_is_already_bgra_and_is_left_as_it_is() {
    let mut data = vec![1, 2, 3, 255, 4, 5, 6, 255];
    assert!(into_bgra(6, &mut data, 2, 1, 8));
    assert_eq!(data, [1, 2, 3, 255, 4, 5, 6, 255]);
}

#[test]
fn an_rgba8888_frame_has_red_and_blue_swapped_but_not_its_row_padding() {
    // 1×2 pixels, 8 bytes a row: the last 4 bytes of each row are padding.
    let mut data = vec![10, 20, 30, 255, 9, 9, 9, 9, 40, 50, 60, 255, 9, 9, 9, 9];
    for format in 16..=18 {
        let mut frame = data.clone();
        assert!(into_bgra(format, &mut frame, 1, 2, 8));
        assert_eq!(frame, [30, 20, 10, 255, 9, 9, 9, 9, 60, 50, 40, 255, 9, 9, 9, 9]);
    }
    // The last row may stop right after its pixels.
    data.truncate(12);
    assert!(into_bgra(18, &mut data, 1, 2, 8));
    assert_eq!(data[8..], [60, 50, 40, 255]);
}

#[test]
fn an_rgba8888_frame_reads_as_the_colours_it_holds() {
    // A pure red pixel, RGBA in memory.
    let mut data = vec![255, 0, 0, 255];
    assert!(into_bgra(18, &mut data, 1, 1, 4));
    let frame = Frame { data: &data, width: 1, height: 1, stride: 4 };
    let red = (255 * 299 / 1000) as u8;
    assert_eq!(frame.thumbnail().pixels, [red]);
}

#[test]
fn a_format_that_is_not_four_bytes_a_pixel_is_refused() {
    let mut data = vec![1, 2, 3, 4];
    assert!(!into_bgra(13, &mut data, 1, 1, 4));
    assert_eq!(data, [1, 2, 3, 4]);
}
