# Meeting detection prompts; it never starts recording on its own

Detection watches for a PipeWire capture stream from a known meeting app (Teams, Zoom, or a browser whose KWin window title matches Teams/Zoom/Meet). Once the stream has been open for about 10 s, it shows a Detection prompt. It never starts recording silently, both for consent reasons and because the heuristics will misfire. When the stream ends, it prompts to stop and auto-stops after 60 s without a response. Manual start/stop from the tray is always available.
