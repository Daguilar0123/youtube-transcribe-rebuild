// =============================================================================
// main.rs — the program's entry point.
//
// This file is intentionally tiny. When you launch the YouTube Transcribe
// Rebuild app, the operating system runs `main()` below, which immediately
// hands off to `run()` over in `lib.rs`. All the real work — talking to
// yt-dlp, ffmpeg, whisper.cpp, merging transcripts — lives there.
//
// Think of this file as the front door: it opens the door and lets the rest
// of the program take over.
// =============================================================================

// On Windows release builds, this line tells the OS "I'm a GUI app, don't
// pop up a black command-prompt window behind me." On macOS it has no effect.
// DO NOT REMOVE — Tauri needs it to behave correctly on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Hand control over to the main library, which actually starts the app
    // window and registers the commands the UI can call.
    youtube_transcribe_rebuild_lib::run()
}
