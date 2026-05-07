# YouTube Transcribe Rebuild

A new macOS-first desktop transcription app built with Tauri, TypeScript, and Rust.

The app preserves the proven local workflow:

```text
YouTube or local media -> ffmpeg WAV conversion -> whisper.cpp transcription -> TXT/SRT outputs
```

YouTube is treated as an acquisition source, not the transcription engine. The app can use existing YouTube captions when requested, but local transcription is performed through `whisper.cpp`.

## MVP Goals

The first usable app focuses on a YouTube URL workflow with clear modes:

- Download video/audio and create a transcript.
- Create a transcript without keeping downloaded media.
- Download video/audio only.
- Download existing YouTube captions only, when available.
- Run `whisper.cpp` transcription only.
- Save both YouTube captions and Whisper output for comparison.

Local media is also supported with these modes:

- Convert local media to WAV only.
- Transcribe only when the selected file is already a compatible WAV.
- Convert and transcribe local media.

## Architecture

The frontend is TypeScript running inside Tauri. It owns the user workflow: URL/file inputs, output folder selection, mode selection, progress display, logs, and final output links.

The Rust backend owns local system work:

- Dependency detection for `yt-dlp`, `ffmpeg`, `whisper-cli`, and Whisper model files.
- YouTube caption discovery/download through `yt-dlp`.
- YouTube video/audio acquisition through `yt-dlp`.
- WAV extraction/conversion through `ffmpeg`.
- Local transcription through `whisper.cpp`.
- Temporary media cleanup when transcript-only modes are selected.
- Emitting progress, stdout, stderr, and errors back to the GUI.

## Non-Negotiables

- Do not depend on YouTube captions for transcription.
- Do not assume `.en` Whisper models exist.
- Dynamically detect installed `.bin` model files.
- Show commands, stdout, stderr, progress, and failures clearly.
- Generate source-labeled TXT and SRT outputs.
- Keep the design macOS-first while avoiding choices that block future Windows support.

## Development

```bash
npm install
npm run tauri dev
```

The app expects local command-line tools to be installed and discoverable, or selected/configured later in the UI:

- `yt-dlp`
- `ffmpeg`
- `whisper-cli` from `whisper.cpp`
- A compatible Whisper `.bin` model file
