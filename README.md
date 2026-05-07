# YouTube Transcribe Rebuild

YouTube Transcribe Rebuild is a macOS-first desktop app for creating transcript files from YouTube videos and local media files.

The app is built with Tauri, TypeScript, and Rust. It preserves the workflow that already works reliably from Terminal:

```text
YouTube or local media -> ffmpeg WAV conversion -> whisper.cpp transcription -> TXT/SRT outputs
```

YouTube is only an input source. The app can save existing YouTube captions when they are available, but it does not depend on YouTube captions for transcription. Local transcription is done with `whisper.cpp`.

## What The App Does

The app helps you avoid typing long Terminal commands every time you need a transcript.

It can:

- Accept a YouTube URL.
- Download YouTube video or audio with `yt-dlp`.
- Save existing YouTube captions when available.
- Use `ffmpeg` to extract or convert audio to WAV.
- Run `whisper.cpp` locally to create transcripts.
- Generate TXT and SRT output files.
- Show progress, commands, stdout, stderr, and errors in the app window.
- Reveal the output folder when the job is done.

## Current MVP Features

The current MVP includes two main workflows:

- YouTube URL workflow, which is the primary workflow.
- Local Media workflow, for files already on the computer.

It currently supports:

- Dependency detection for `yt-dlp`, `ffmpeg`, and `whisper-cli`.
- Dynamic Whisper model scanning for `.bin` model files.
- Model selection from detected local models.
- Optional Whisper prompt text.
- Subtitle max length setting.
- Live progress updates.
- Live command log.
- Output file list.
- Source-labeled transcript filenames.

Example output names:

```text
video-title.youtube-captions.txt
video-title.youtube-captions.srt
video-title.whisper.txt
video-title.whisper.srt
```

## YouTube Workflow Modes

The YouTube tab supports several modes.

### Captions If Available, Otherwise Whisper

This is the default mode.

The app first tries to save existing YouTube captions. If no captions are available, it downloads temporary media, converts audio with `ffmpeg`, and runs `whisper.cpp` locally.

### Download Media + Transcript

The app keeps the downloaded media and also creates a transcript.

### Transcript Only

The app creates transcript files without keeping downloaded media.

If Whisper is needed, the app may temporarily download media or audio, but it should clean up temporary files afterward.

### Download Media Only

The app downloads video or audio and does not create transcript files.

### YouTube Captions Only

The app saves existing YouTube captions only.

If no captions are available, the job fails clearly instead of silently producing no transcript.

### Whisper Only

The app ignores YouTube captions and forces local transcription through `whisper.cpp`.

### Captions + Whisper

The app saves both YouTube captions and Whisper output so they can be compared.

## Local Media Workflow Modes

The Local Media tab supports files that are already on the computer, such as MP4, M4A, MP3, MOV, MKV, WEBM, or WAV files.

### Convert To WAV Only

Use this when you only want `ffmpeg` to create a compatible WAV file.

### Transcribe WAV Only

Use this when the selected file is already a compatible WAV file and you only want to run `whisper.cpp`.

### Convert + Transcribe

Use this for most local video or audio files.

The app converts the file to WAV with `ffmpeg`, then runs `whisper.cpp` and creates TXT/SRT outputs.

## Required Dependencies

The app currently expects these command-line tools to already be installed on the Mac:

- `yt-dlp`
- `ffmpeg`
- `whisper-cli` from `whisper.cpp`
- At least one compatible Whisper `.bin` model file

YouTube changes often, so `yt-dlp` should be kept current when testing or debugging YouTube downloads:

```bash
yt-dlp --version
brew update
brew upgrade yt-dlp
```

The app was last tested locally with:

```text
yt-dlp 2026.03.17
```

The app scans common model folders, including:

```text
~/whisper.cpp/models
~/Downloads/whisper.cpp/models
```

Important: the app must not assume `.en` models exist. It should work with models such as:

```text
ggml-medium.bin
ggml-large.bin
```

## Run In Development

From the project folder:

```bash
cd /Users/danielaguilar/Developer/youtube-transcribe-rebuild
npm install
npm run tauri dev
```

This starts the Tauri development app.

## Build The App

From the project folder:

```bash
npm run tauri build
```

A successful build creates a macOS app and DMG under:

```text
src-tauri/target/release/bundle/macos/
src-tauri/target/release/bundle/dmg/
```

## Open The Built App Without Installing

After building, the app can be opened directly without dragging it into `/Applications`.

Use Finder to open:

```text
/Users/danielaguilar/Developer/youtube-transcribe-rebuild/src-tauri/target/release/bundle/macos/YouTube Transcribe Rebuild.app
```

Or from Terminal:

```bash
open "/Users/danielaguilar/Developer/youtube-transcribe-rebuild/src-tauri/target/release/bundle/macos/YouTube Transcribe Rebuild.app"
```

The DMG is also available at:

```text
/Users/danielaguilar/Developer/youtube-transcribe-rebuild/src-tauri/target/release/bundle/dmg/YouTube Transcribe Rebuild_0.1.0_x64.dmg
```

## Known Issue: x64 Build

The current build is `x64` because the installed Rust toolchain is running under x86_64 emulation.

For best performance on Apple Silicon, this should be fixed later by installing native Apple Silicon Rust and rebuilding the app as arm64 or universal.

## Next Testing Checklist

Test these before treating the MVP as reliable:

- YouTube URL with existing captions.
- YouTube URL without captions, falling back to Whisper.
- YouTube captions only mode when captions exist.
- YouTube captions only mode when captions do not exist.
- Whisper only mode, even when captions exist.
- Captions + Whisper mode for comparison.
- Download video only.
- Download audio only.
- Transcript only with temporary media cleanup.
- Local MP4 or MOV convert + transcribe.
- Local M4A or MP3 convert + transcribe.
- Local media convert to WAV only.
- Compatible WAV transcribe only.
- Missing `yt-dlp` error message.
- Missing `ffmpeg` error message.
- Missing `whisper-cli` error message.
- Missing or invalid model path error message.
- Output folder reveal button.
