# Meeting titles read from Screenshots

Tiro doesn't send a Meeting's Screenshots to a vision model to work out its title.

## Why this is out of scope

A new Meeting starts with a default title, `<app> — YYYY-MM-DD HH:MM`. The Summary already gives it a better one: the model's reply starts with a title line (`suggested_title` in `crates/core/src/summary.rs`), which replaces the default title unless the user has renamed the Meeting.

Reading the title from the first Screenshot would only get a better title in before the Summary does. For that it would need:

- an extra model call for every Meeting, which costs money and needs the network
- sending screen content to the provider before the user has asked for anything to be sent, which raises a consent question that Screenshot consent (15) doesn't settle
- a fallback for first frames that show no title (a lobby, a shared screen, a camera gallery)

That's too much cost and privacy risk for something the Summary already does.

Taking the title from the KWin window caption is a different idea. Detection already reads the caption (`meeting_in_caption`), and it needs no Screenshots, no model call and no consent. This rejection doesn't cover it.

## Prior requests

- `.scratch/tiro-v1/issues/18-title-from-first-screenshot.md`: "Suggest a Meeting title from the first screenshot"
