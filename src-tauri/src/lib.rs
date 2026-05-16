// =============================================================================
// lib.rs — the brains of the app.
//
// WHAT THIS FILE DOES (in plain English)
// --------------------------------------
// This is the Rust side of the YouTube Transcribe Rebuild app. The user
// interface (HTML/CSS/TypeScript) lives in the `src/` folder; this file is
// the engine that the UI talks to whenever the user clicks "Start".
//
// In a Tauri app, the UI is essentially a small web browser window, and Rust
// runs underneath it doing the work that browsers can't do safely — running
// command-line tools, reading and writing arbitrary files, etc. The UI sends
// requests over a bridge ("invoke a command"), Rust handles them, and Rust
// can also push live progress updates ("emit an event") back to the UI.
//
// The "commands" the UI can invoke are at the bottom of the file:
//
//   check_environment      — list which CLI tools are installed.
//   run_youtube_job        — handle the YouTube URL tab.
//   run_local_job          — handle the Local Media tab.
//   run_hybrid_merge_job   — handle the Merge Transcripts tab.
//
// Everything else in this file is a helper used by those four commands:
// tool detection, command spawning, file naming, cleanup, and so on.
// The actual hybrid-merge math lives in its own file, `hybrid.rs`.
// =============================================================================

// Pull in the sibling hybrid.rs module that contains the transcript merger.
mod hybrid;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

// -----------------------------------------------------------------------------
// Data shapes — these are the structured messages the Rust side exchanges
// with the UI (and a couple of internal-only helpers).
//
// `Serialize` types are what Rust sends *to* the UI. `Deserialize` types are
// what the UI sends *to* Rust. The `#[serde(rename_all = "camelCase")]`
// attribute means Rust uses snake_case names internally but converts them to
// camelCase on the wire (because JavaScript expects camelCase).
// -----------------------------------------------------------------------------

/// One live progress update sent to the UI while a job is running. The UI
/// shows these in the log panel and uses `stage` events to fill in the
/// "Progress" list.
#[derive(Debug, Clone, Serialize)]
struct JobEvent {
    level: String,    // e.g. "stage", "info", "warn", "error", "stdout", "stderr"
    stage: String,    // which phase the event belongs to (download, ffmpeg, etc.)
    message: String,  // free-form human-readable message
}

/// Reports whether a single command-line tool was found on the system, and
/// where. Used to fill in the row of dependency badges at the top of the UI.
#[derive(Debug, Serialize)]
struct DependencyStatus {
    name: String,
    found: bool,
    path: Option<String>,
}

/// The full environment report sent back when the UI asks "what tools and
/// Whisper models do you see on this machine?".
#[derive(Debug, Serialize)]
struct EnvironmentReport {
    dependencies: Vec<DependencyStatus>,
    models: Vec<String>,
}

/// The final summary the UI receives when a job finishes successfully:
/// the list of output files (so it can show them and reveal the folder)
/// and the list of temporary items that were cleaned up along the way.
#[derive(Debug, Serialize)]
struct JobResult {
    status: String,
    outputs: Vec<String>,
    cleaned: Vec<String>,
}

/// The form fields the UI sends when the user runs a job from the
/// YouTube URL tab. Mirrors the shape of the UI form.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YoutubeJobRequest {
    url: String,
    output_dir: String,
    media_action: String,       // "audio" or "video"
    transcript_source: String,  // "captions_fallback", "hybrid", "whisper_only", etc.
    keep_media: bool,
    model_path: Option<String>,
    whisper_prompt: Option<String>,
    max_len: Option<u32>,
}

/// The form fields the UI sends from the Local Media tab.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalJobRequest {
    input_file: String,
    output_dir: String,
    mode: String,               // "convert_transcribe", "convert_only", "transcribe_only"
    model_path: Option<String>,
    whisper_prompt: Option<String>,
    max_len: Option<u32>,
}

/// The form fields the UI sends from the Merge Transcripts tab.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergeJobRequest {
    whisper_srt: String,
    youtube_srt: String,
    info_json: Option<String>,
    output_dir: String,
    output_base: Option<String>,
}

/// The set of CLI tool paths the app needs, in one place. Any tool that
/// wasn't found is `None`.
#[derive(Debug)]
struct ToolPaths {
    yt_dlp: Option<PathBuf>,
    ffmpeg: Option<PathBuf>,
    whisper_cli: Option<PathBuf>,
}

// -----------------------------------------------------------------------------
// Logging / progress emit helper.
// -----------------------------------------------------------------------------

/// Sends one progress update from Rust to the UI. The UI is listening for
/// "job-event" messages and routes them into the log panel and the
/// "Progress" list. Errors during emit are silently swallowed — if the UI
/// isn't listening, there's nothing reasonable to do about it here.
fn emit(app: &AppHandle, level: &str, stage: &str, message: impl Into<String>) {
    let _ = app.emit(
        "job-event",
        JobEvent {
            level: level.to_string(),
            stage: stage.to_string(),
            message: message.into(),
        },
    );
}

// -----------------------------------------------------------------------------
// Tool discovery — finding yt-dlp, ffmpeg, whisper-cli, and Whisper models
// without hardcoding paths.
// -----------------------------------------------------------------------------

/// Returns the user's home directory (e.g. /Users/yourname). Used as a base
/// for looking up Whisper models and the whisper.cpp build folder.
fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

/// Builds the list of folders to search when looking for command-line tools.
/// Starts with a few well-known Homebrew/system folders, then adds anything
/// listed in the user's `PATH` environment variable.
fn path_entries() -> Vec<PathBuf> {
    let mut entries = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ];

    if let Some(paths) = env::var_os("PATH") {
        entries.extend(env::split_paths(&paths));
    }

    let mut seen = HashSet::new();
    entries
        .into_iter()
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

/// Looks for an executable file by name. First checks any `extra_paths`
/// passed in (used for tools like whisper-cli that live in a known
/// subfolder of the user's home directory), then walks every folder from
/// `path_entries()`.
fn find_executable(name: &str, extra_paths: &[PathBuf]) -> Option<PathBuf> {
    for path in extra_paths {
        if path.is_file() {
            return Some(path.clone());
        }
    }

    for dir in path_entries() {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// Runs `find_executable` for each of the three CLI tools the app depends on
/// and bundles the results into a single `ToolPaths` value. Called once per
/// job so that fresh installs/uninstalls take effect immediately.
fn detect_tools() -> ToolPaths {
    let home = home_dir();
    let whisper_candidates = home
        .iter()
        .flat_map(|home| {
            [
                home.join("whisper.cpp/build/bin/whisper-cli"),
                home.join("Downloads/whisper.cpp/build/bin/whisper-cli"),
            ]
        })
        .collect::<Vec<_>>();

    ToolPaths {
        yt_dlp: find_executable("yt-dlp", &[]),
        ffmpeg: find_executable("ffmpeg", &[]),
        whisper_cli: find_executable("whisper-cli", &whisper_candidates),
    }
}

/// The two folders we'll scan for downloaded Whisper `.bin` models. Anyone
/// who installs whisper.cpp via its README ends up with one of these.
fn model_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home_dir() {
        dirs.push(home.join("whisper.cpp/models"));
        dirs.push(home.join("Downloads/whisper.cpp/models"));
    }
    dirs
}

/// Lists every Whisper model `.bin` file found on disk, sorted with the
/// "best" model first (large, then medium, then everything else). The UI
/// populates its model dropdown from this list.
fn scan_models() -> Vec<String> {
    let mut models = Vec::new();
    let mut seen = HashSet::new();

    for dir in model_dirs() {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if is_plausible_whisper_model(&path) && seen.insert(path.clone()) {
                    models.push(path.to_string_lossy().to_string());
                }
            }
        }
    }

    models.sort_by(|a, b| model_rank(a).cmp(&model_rank(b)).then(a.cmp(b)));
    models
}

/// Cheap "does this look like a real Whisper model?" sanity check.
/// Filters out test stubs, voice-activity models, and anything that's too
/// small to actually be a Whisper checkpoint.
fn is_plausible_whisper_model(path: &Path) -> bool {
    if path.extension() != Some(OsStr::new("bin")) {
        return false;
    }

    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_lowercase();

    if !lower.starts_with("ggml-") || lower.contains("for-tests") || lower.contains("silero") {
        return false;
    }

    path.metadata()
        .map(|metadata| metadata.len() >= 10 * 1024 * 1024)
        .unwrap_or(false)
}

/// Used to sort the model dropdown: large multilingual first, then medium,
/// then anything else multilingual, then English-only models last. Lower
/// numbers sort earlier.
fn model_rank(path: &str) -> u8 {
    let name = path.to_lowercase();
    if name.contains("large") && !name.contains(".en") {
        0
    } else if name.contains("medium") && !name.contains(".en") {
        1
    } else if !name.contains(".en") {
        2
    } else {
        3
    }
}

// -----------------------------------------------------------------------------
// Filename / path helpers.
// -----------------------------------------------------------------------------

/// Cleans up a string (usually a YouTube video title) so it can safely be
/// used as a filename: letters, digits, spaces, dots, underscores and
/// dashes are kept; everything else becomes an underscore.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "transcript".to_string()
    } else {
        trimmed
    }
}

/// Creates a one-off temporary working folder inside the user's chosen
/// output folder. The name includes the current timestamp in milliseconds
/// so two jobs started back-to-back never collide.
fn unique_temp_dir(output_dir: &Path) -> Result<PathBuf, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis();
    let dir = output_dir.join(format!(".yt-transcribe-tmp-{}", millis));
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create temp folder: {}", e))?;
    Ok(dir)
}

// -----------------------------------------------------------------------------
// Command-spawning plumbing — running yt-dlp, ffmpeg, and whisper-cli, with
// live output streaming back to the UI.
// -----------------------------------------------------------------------------

/// Runs a command-line tool, streaming both its normal output (stdout) and
/// its error output (stderr) into the UI's log panel as the tool runs.
///
/// Returns `Ok(())` if the command exits successfully. Returns an `Err`
/// string with the tool's last 20 lines of stderr if it fails — that tail
/// is usually where the real error message is.
fn run_command(
    app: &AppHandle,
    stage: &str,
    program: &Path,
    args: &[String],
    working_dir: Option<&Path>,
) -> Result<(), String> {
    // First, log the full command line so the user can see exactly what's
    // running (useful for debugging and for trust).
    let command_line = format!(
        "{} {}",
        program.to_string_lossy(),
        args.iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    );
    emit(app, "command", stage, command_line);

    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = working_dir {
        command.current_dir(dir);
    }

    // Actually launch the program as a child process.
    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to start {}: {}", program.to_string_lossy(), e))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // `errors` is a rolling buffer of the most recent stderr lines, kept
    // behind a mutex because two threads (this one and the stderr reader)
    // touch it. If the command fails, we use it for the error message.
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut handles = Vec::new();

    // Each stream gets its own thread so output appears live instead of
    // appearing all at once when the command finishes.
    if let Some(stdout) = stdout {
        let app = app.clone();
        let stage = stage.to_string();
        handles.push(thread::spawn(move || {
            for line in BufReader::new(stdout).lines().flatten() {
                emit(&app, "stdout", &stage, line);
            }
        }));
    }

    if let Some(stderr) = stderr {
        let app = app.clone();
        let stage = stage.to_string();
        let errors = Arc::clone(&errors);
        handles.push(thread::spawn(move || {
            for line in BufReader::new(stderr).lines().flatten() {
                if let Ok(mut guard) = errors.lock() {
                    guard.push(line.clone());
                    if guard.len() > 20 {
                        guard.remove(0);
                    }
                }
                emit(&app, "stderr", &stage, line);
            }
        }));
    }

    let status = child
        .wait()
        .map_err(|e| format!("Failed while waiting for command: {}", e))?;

    for handle in handles {
        let _ = handle.join();
    }

    let code = status.code().unwrap_or(-1);
    if !status.success() {
        let tail = errors.lock().map(|g| g.join("\n")).unwrap_or_default();
        return Err(format!("Command failed with exit code {}.\n{}", code, tail));
    }

    Ok(())
}

/// Wraps a command-line argument in quotes if it contains spaces or
/// special characters. Used purely for *displaying* the command in the log
/// panel — the actual command execution doesn't go through a shell.
fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_alphanumeric() || "./_-=:".contains(c))
    {
        value.to_string()
    } else {
        format!("{:?}", value)
    }
}

/// Decides whether we need to fire up yt-dlp to download media. We do if:
///  - the user wants media only,
///  - the user wants to keep media alongside the transcript, OR
///  - we need audio so Whisper can run.
fn should_download_media(media_only: bool, keep_media: bool, whisper_requested: bool) -> bool {
    media_only || keep_media || whisper_requested
}

/// Like `run_command` but waits for the program to finish and returns its
/// captured stdout as a string. Used for quick one-shot queries like
/// "ask yt-dlp for this video's title" — no need to stream output for those.
fn command_output(
    program: &Path,
    args: &[String],
    working_dir: Option<&Path>,
) -> Result<String, String> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(dir) = working_dir {
        command.current_dir(dir);
    }
    let output = command
        .output()
        .map_err(|e| format!("Failed to run {}: {}", program.to_string_lossy(), e))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// For the Local Media tab: derives the output-file prefix from the input
/// file's name. (e.g. `lecture-04.mp4` -> `lecture-04`.)
fn base_from_input(path: &Path) -> String {
    sanitize(
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("transcript"),
    )
}

/// Asks yt-dlp for the YouTube video's title and returns a filename-safe
/// version of it. If anything goes wrong (no internet, deleted video, etc.)
/// falls back to a generic "youtube-transcript" so the job can still run.
fn get_youtube_title(yt_dlp: &Path, url: &str) -> String {
    let args = vec![
        "--no-playlist".to_string(),
        "--print".to_string(),
        "title".to_string(),
        url.to_string(),
    ];
    command_output(yt_dlp, &args, None)
        .ok()
        .and_then(|s| s.lines().next().map(sanitize))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "youtube-transcript".to_string())
}

// -----------------------------------------------------------------------------
// YouTube captions — downloading them, converting them, saving them.
// -----------------------------------------------------------------------------

/// What the caption-download step returns: which transcript files it saved,
/// which metadata sidecar files it saved alongside (description, info.json),
/// and the path to the .srt file if one was produced (used by the hybrid
/// merge later).
struct CaptionDownload {
    transcripts: Vec<PathBuf>,
    metadata: Vec<PathBuf>,
    srt_path: Option<PathBuf>,
}

/// Asks yt-dlp for the video's existing English captions (auto-generated or
/// human-written) plus the video's description and full metadata JSON.
/// Saves all of it into the user's output folder with consistent names.
///
/// Returns a `CaptionDownload`. The transcript lists are empty if no
/// captions were available; that's a normal outcome, not an error.
fn download_captions(
    app: &AppHandle,
    yt_dlp: &Path,
    url: &str,
    temp_dir: &Path,
    output_dir: &Path,
    base: &str,
) -> Result<CaptionDownload, String> {
    emit(
        app,
        "stage",
        "captions",
        "Checking for existing YouTube captions",
    );
    // yt-dlp puts everything in the temp folder first; we'll rename and move
    // anything worth keeping into the user's output folder afterward.
    let outtmpl = temp_dir
        .join("captions.%(ext)s")
        .to_string_lossy()
        .to_string();
    // What the flags mean:
    //   --skip-download         don't download the video, just the captions
    //   --write-subs            grab human-uploaded captions if present
    //   --write-auto-subs       fall back to YouTube's auto-captions
    //   --write-description     save the video's description as .description
    //   --write-info-json       save full metadata as .info.json
    //   --sub-langs en.*        any English variant (en, en-US, en-GB, ...)
    //   --convert-subs srt      always normalize captions to .srt
    let args = vec![
        "--skip-download".to_string(),
        "--write-subs".to_string(),
        "--write-auto-subs".to_string(),
        "--write-description".to_string(),
        "--write-info-json".to_string(),
        "--sub-langs".to_string(),
        "en.*".to_string(),
        "--sub-format".to_string(),
        "srt/vtt/best".to_string(),
        "--convert-subs".to_string(),
        "srt".to_string(),
        "-o".to_string(),
        outtmpl,
        url.to_string(),
    ];

    if let Err(error) = run_command(app, "captions", yt_dlp, &args, Some(temp_dir)) {
        emit(
            app,
            "warn",
            "captions",
            format!("Caption download did not complete cleanly: {}", error),
        );
    }

    let metadata = collect_metadata_sidecars(app, temp_dir, "captions", output_dir, base);

    let caption_file = newest_file_with_exts(temp_dir, &["srt", "vtt"]);
    let Some(caption_file) = caption_file else {
        emit(app, "info", "captions", "No YouTube captions were saved");
        return Ok(CaptionDownload {
            transcripts: Vec::new(),
            metadata,
            srt_path: None,
        });
    };

    let srt_out = output_dir.join(format!("{}.youtube-captions.srt", base));
    let txt_out = output_dir.join(format!("{}.youtube-captions.txt", base));

    if caption_file.extension() == Some(OsStr::new("srt")) {
        fs::copy(&caption_file, &srt_out)
            .map_err(|e| format!("Could not save SRT captions: {}", e))?;
        let txt = captions_to_txt(&caption_file)?;
        fs::write(&txt_out, txt).map_err(|e| format!("Could not save TXT captions: {}", e))?;
        emit(
            app,
            "stage",
            "captions",
            "Saved YouTube captions as TXT and SRT",
        );
    } else {
        let txt = captions_to_txt(&caption_file)?;
        fs::write(&txt_out, txt).map_err(|e| format!("Could not save TXT captions: {}", e))?;
        fs::copy(&caption_file, &srt_out)
            .map_err(|e| format!("Could not save caption file: {}", e))?;
        emit(app, "stage", "captions", "Saved YouTube captions");
    }

    Ok(CaptionDownload {
        transcripts: vec![srt_out.clone(), txt_out],
        metadata,
        srt_path: Some(srt_out),
    })
}

/// Finds any `.description` and `.info.json` files yt-dlp wrote (using
/// whatever filename stem we asked it to use), then moves or copies them
/// into the user's output folder with consistent `<base>.description` /
/// `<base>.info.json` names. Returns the final paths so the UI can list them.
fn collect_metadata_sidecars(
    app: &AppHandle,
    source_dir: &Path,
    source_stem: &str,
    output_dir: &Path,
    base: &str,
) -> Vec<PathBuf> {
    let mut collected = Vec::new();
    for ext in ["description", "info.json"] {
        let dst = output_dir.join(format!("{}.{}", base, ext));
        if let Some(found) = find_sidecar(source_dir, source_stem, ext) {
            if let Err(error) = move_or_copy(&found, &dst) {
                emit(
                    app,
                    "warn",
                    "metadata",
                    format!("Could not save {}: {}", ext, error),
                );
                continue;
            }
        }
        if dst.is_file() && !collected.contains(&dst) {
            collected.push(dst);
        }
    }
    if !collected.is_empty() {
        let names: Vec<String> = collected
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        emit(
            app,
            "stage",
            "metadata",
            format!("Saved metadata sidecar(s): {}", names.join(", ")),
        );
    }
    collected
}

/// Looks in `dir` for a file named exactly `<stem>.<ext>`. If that's not
/// there, falls back to any file that starts with `<stem>.` and ends with
/// `.<ext>` (yt-dlp sometimes inserts language suffixes).
fn find_sidecar(dir: &Path, stem: &str, ext: &str) -> Option<PathBuf> {
    let direct = dir.join(format!("{}.{}", stem, ext));
    if direct.is_file() {
        return Some(direct);
    }
    fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let path = entry.path();
        if !path.is_file() {
            return None;
        }
        let name = path.file_name()?.to_str()?;
        let suffix = format!(".{}", ext);
        if name.starts_with(&format!("{}.", stem)) && name.ends_with(&suffix) {
            Some(path)
        } else {
            None
        }
    })
}

/// Moves a file from `src` to `dst`. Tries a fast rename first (works when
/// both paths are on the same filesystem); falls back to copy-then-delete
/// if rename fails. If `src` and `dst` are the same path, does nothing.
fn move_or_copy(src: &Path, dst: &Path) -> Result<(), String> {
    if src == dst {
        return Ok(());
    }
    if dst.exists() {
        let _ = fs::remove_file(dst);
    }
    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    fs::copy(src, dst)
        .map(|_| ())
        .map_err(|e| format!("Could not copy {} → {}: {}", src.display(), dst.display(), e))?;
    let _ = fs::remove_file(src);
    Ok(())
}

/// Reads a YouTube caption file (.srt or .vtt) and produces a clean plain-
/// text version: dropping the block numbers, timing lines, format headers,
/// and HTML-style tags, and collapsing consecutive duplicate lines (which
/// rolling captions love to produce).
fn captions_to_txt(path: &Path) -> Result<String, String> {
    let content =
        fs::read_to_string(path).map_err(|e| format!("Could not read captions: {}", e))?;
    let mut lines = Vec::new();
    let mut previous = String::new();

    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line.chars().all(|c| c.is_ascii_digit())
            || line.contains("-->")
            || line.starts_with("WEBVTT")
            || line.starts_with("NOTE")
            || line.starts_with("Kind:")
            || line.starts_with("Language:")
        {
            continue;
        }
        let cleaned = line
            .replace("<c>", "")
            .replace("</c>", "")
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">");
        if cleaned != previous {
            previous = cleaned.clone();
            lines.push(cleaned);
        }
    }

    Ok(lines.join("\n"))
}

// -----------------------------------------------------------------------------
// YouTube media download — calls yt-dlp to fetch the actual video or audio.
// -----------------------------------------------------------------------------

/// Downloads the actual media file (video or just audio, depending on
/// `media_action`) into `output_dir`, named after `base`.
///
/// YouTube's available formats change without warning, so this function
/// tries a sequence of "preferred -> still-good -> fallback" format strings.
/// It moves to the next attempt only if the current one fails outright.
/// `--embed-metadata` writes the title/description/etc. into the media
/// file's container so apps like VLC, ffprobe, and MediaInfo can read it
/// back later.
fn download_media(
    app: &AppHandle,
    yt_dlp: &Path,
    ffmpeg: Option<&Path>,
    url: &str,
    output_dir: &Path,
    base: &str,
    media_action: &str,
) -> Result<Vec<PathBuf>, String> {
    emit(app, "stage", "download", "Downloading YouTube media");
    // Tell yt-dlp how to name the output: "<base>.<extension>".
    let outtmpl = output_dir
        .join(format!("{}.%(ext)s", base))
        .to_string_lossy()
        .to_string();
    // Pick a list of yt-dlp format strings to try. For video we want H.264
    // video + AAC audio in an MP4 container (most-compatible). For audio
    // we want AAC inside an M4A container.
    let format_attempts = if media_action == "video" {
        vec![
            "bv*[ext=mp4][vcodec^=avc1]+ba[ext=m4a][acodec^=mp4a]/b[ext=mp4][vcodec^=avc1][acodec^=mp4a]",
            "bv*[ext=mp4][vcodec^=avc1]+ba[ext=m4a]/b[ext=mp4][vcodec^=avc1]",
            "bv*[ext=mp4]+ba[ext=m4a]/b[ext=mp4]/bv*+ba/b",
            "bv*+ba/b",
            "best",
        ]
    } else {
        vec![
            "ba[ext=m4a][acodec^=mp4a]/ba[ext=m4a]/ba[acodec^=mp4a]/ba/b",
            "ba[ext=m4a]/ba/b",
            "ba/b",
            "best",
        ]
    };

    let mut last_error = None;
    for (index, format) in format_attempts.iter().enumerate() {
        if index > 0 {
            emit(
                app,
                "warn",
                "download",
                format!("Retrying download with fallback format: {}", format),
            );
        }

        let mut args = vec![
            "--no-playlist".to_string(),
            "--no-keep-video".to_string(),
            "--write-description".to_string(),
            "--write-info-json".to_string(),
            "--embed-metadata".to_string(),
            "-f".to_string(),
            format.to_string(),
        ];

        if media_action == "video" {
            if let Some(ffmpeg) = ffmpeg {
                args.push("--ffmpeg-location".to_string());
                args.push(ffmpeg.to_string_lossy().to_string());
                args.push("--merge-output-format".to_string());
                args.push("mp4".to_string());
            }
        } else if let Some(ffmpeg) = ffmpeg {
            args.push("--ffmpeg-location".to_string());
            args.push(ffmpeg.to_string_lossy().to_string());
        }

        args.extend(["-o".to_string(), outtmpl.clone(), url.to_string()]);

        match run_command(app, "download", yt_dlp, &args, Some(output_dir)) {
            Ok(()) => {
                let artifacts = yt_dlp_format_artifacts(output_dir, base).unwrap_or_default();
                let _ = clean_paths(app, &artifacts);
                return downloaded_media_files(output_dir, base)
                    .map(|files| newest_first(files).collect())
                    .filter(|files: &Vec<PathBuf>| !files.is_empty())
                    .ok_or_else(|| "yt-dlp finished but no media file was found".to_string());
            }
            Err(error) => {
                let artifacts = yt_dlp_format_artifacts(output_dir, base).unwrap_or_default();
                let _ = clean_paths(app, &artifacts);
                last_error = Some(error);
            }
        }
    }

    if let Some(error) = last_error {
        return Err(error);
    }

    let artifacts = yt_dlp_format_artifacts(output_dir, base).unwrap_or_default();
    let _ = clean_paths(app, &artifacts);
    downloaded_media_files(output_dir, base)
        .map(|files| newest_first(files).collect())
        .filter(|files: &Vec<PathBuf>| !files.is_empty())
        .ok_or_else(|| "yt-dlp finished but no media file was found".to_string())
}

// -----------------------------------------------------------------------------
// Audio preparation — turning whatever media we have into the WAV format
// Whisper expects.
// -----------------------------------------------------------------------------

/// Uses ffmpeg to extract a 16 kHz mono 16-bit WAV file from any audio or
/// video input. whisper.cpp specifically wants that format, so we always
/// transcode before transcribing.
fn convert_to_wav(
    app: &AppHandle,
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
) -> Result<PathBuf, String> {
    emit(
        app,
        "stage",
        "ffmpeg",
        "Extracting 16 kHz mono WAV with ffmpeg",
    );
    // ffmpeg flags:
    //   -y               overwrite any existing output file
    //   -i <input>       the input file
    //   -vn              skip the video stream (audio only)
    //   -acodec pcm_s16le  16-bit signed little-endian PCM samples
    //   -ar 16000        16 kHz sample rate
    //   -ac 1            mono (one channel)
    let args = vec![
        "-y".to_string(),
        "-i".to_string(),
        input.to_string_lossy().to_string(),
        "-vn".to_string(),
        "-acodec".to_string(),
        "pcm_s16le".to_string(),
        "-ar".to_string(),
        "16000".to_string(),
        "-ac".to_string(),
        "1".to_string(),
        output.to_string_lossy().to_string(),
    ];
    run_command(app, "ffmpeg", ffmpeg, &args, output.parent())?;
    Ok(output.to_path_buf())
}

// -----------------------------------------------------------------------------
// Whisper — local speech-to-text using whisper.cpp.
// -----------------------------------------------------------------------------

/// Runs whisper-cli against the prepared WAV file and writes both .txt and
/// .srt outputs. Optionally feeds in a prompt (free-form vocabulary hints —
/// names, technical terms) and a max subtitle line length.
fn run_whisper(
    app: &AppHandle,
    whisper_cli: &Path,
    model_path: &Path,
    wav: &Path,
    output_prefix: &Path,
    prompt: Option<&str>,
    max_len: Option<u32>,
) -> Result<Vec<PathBuf>, String> {
    emit(app, "stage", "whisper", "Running whisper.cpp locally");
    let mut args = vec![
        "-m".to_string(),
        model_path.to_string_lossy().to_string(),
        "-f".to_string(),
        wav.to_string_lossy().to_string(),
        "-otxt".to_string(),
        "-osrt".to_string(),
        "-of".to_string(),
        output_prefix.to_string_lossy().to_string(),
    ];

    if let Some(max_len) = max_len.filter(|v| *v > 0) {
        args.push("--max-len".to_string());
        args.push(max_len.to_string());
    }
    if let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) {
        args.push("--prompt".to_string());
        args.push(prompt.trim().to_string());
    }

    run_command(app, "whisper", whisper_cli, &args, output_prefix.parent())?;

    let txt = output_prefix.with_extension("txt");
    let srt = output_prefix.with_extension("srt");
    let mut outputs = Vec::new();
    if txt.is_file() {
        outputs.push(txt);
    }
    if srt.is_file() {
        outputs.push(srt);
    }
    if outputs.is_empty() {
        return Err("whisper.cpp finished but no TXT or SRT output was found".to_string());
    }
    Ok(outputs)
}

// -----------------------------------------------------------------------------
// Folder-scanning helpers — figuring out which files yt-dlp / whisper / ffmpeg
// actually produced, after the fact.
// -----------------------------------------------------------------------------

/// Returns the most recently modified file in `dir` with any of the given
/// extensions. Used to grab "whatever caption file yt-dlp just wrote" when
/// we don't know in advance whether it'll be .srt or .vtt.
fn newest_file_with_exts(dir: &Path, extensions: &[&str]) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|ext| {
                        extensions
                            .iter()
                            .any(|wanted| ext.eq_ignore_ascii_case(wanted))
                    })
                    .unwrap_or(false)
        })
        .max_by_key(|path| path.metadata().and_then(|m| m.modified()).ok())
}

/// Lists every audio or video file in `dir` (regardless of name) using a
/// fixed list of recognized container extensions.
fn media_files(dir: &Path) -> Option<Vec<PathBuf>> {
    let extensions = [
        "mp4", "m4a", "mp3", "webm", "mkv", "mov", "aac", "opus", "ogg",
    ];
    let files = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|ext| {
                        extensions
                            .iter()
                            .any(|wanted| ext.eq_ignore_ascii_case(wanted))
                    })
                    .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    Some(files)
}

/// Of all media files in `dir`, returns only the ones whose name exactly
/// matches our chosen base (e.g. `<base>.mp4`). This is the *finished*
/// download — not yt-dlp's intermediate per-stream artifacts.
fn downloaded_media_files(dir: &Path, base: &str) -> Option<Vec<PathBuf>> {
    Some(
        media_files(dir)?
            .into_iter()
            .filter(|path| media_file_matches_base(path, base))
            .collect(),
    )
}

/// Returns the *intermediate* per-format files yt-dlp leaves behind during
/// adaptive downloads — things like `<base>.f137.mp4` (video stream) and
/// `<base>.f140.m4a` (audio stream). We delete these as cleanup after the
/// merged output file is in place.
fn yt_dlp_format_artifacts(dir: &Path, base: &str) -> Option<Vec<PathBuf>> {
    Some(
        media_files(dir)?
            .into_iter()
            .filter(|path| media_file_is_format_artifact(path, base))
            .collect(),
    )
}

/// True if the file's name (without extension) is exactly the base we chose.
/// Helps us tell `<base>.mp4` (keep) apart from `<base>.f137.mp4` (cleanup).
fn media_file_matches_base(path: &Path, base: &str) -> bool {
    path.file_stem()
        .and_then(OsStr::to_str)
        .map(|stem| stem == base)
        .unwrap_or(false)
}

/// True if the file's name follows yt-dlp's per-stream pattern
/// `<base>.f<digits>` — meaning it's a transient download artifact, not the
/// final merged output.
fn media_file_is_format_artifact(path: &Path, base: &str) -> bool {
    path.file_stem()
        .and_then(OsStr::to_str)
        .and_then(|stem| stem.strip_prefix(base))
        .and_then(|suffix| suffix.strip_prefix(".f"))
        .map(|format_id| !format_id.is_empty() && format_id.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

/// Sorts files newest-first by last-modified time. Used so we always reach
/// for the result of the latest yt-dlp run when there are multiple
/// candidates.
fn newest_first(files: Vec<PathBuf>) -> impl Iterator<Item = PathBuf> {
    let mut files = files;
    files.sort_by_key(|path| path.metadata().and_then(|m| m.modified()).ok());
    files.reverse();
    files.into_iter()
}

/// Of a list of media files, returns the first one that actually contains
/// an audio stream (ffmpeg will tell us if there is one). Avoids the
/// frustrating case where yt-dlp delivered a video-only stream that
/// Whisper can't transcribe.
fn find_audio_media(ffmpeg: &Path, files: &[PathBuf]) -> Option<PathBuf> {
    files
        .iter()
        .find(|path| media_has_audio(ffmpeg, path))
        .cloned()
}

/// Quick probe: ask ffmpeg to inspect a file, and look for the word
/// "Audio:" in its stderr. If it's there, the file contains an audio stream.
fn media_has_audio(ffmpeg: &Path, path: &Path) -> bool {
    let args = vec![
        "-hide_banner".to_string(),
        "-i".to_string(),
        path.to_string_lossy().to_string(),
    ];

    Command::new(ffmpeg)
        .args(args)
        .output()
        .map(|output| {
            let stderr = String::from_utf8_lossy(&output.stderr);
            stderr.contains("Audio:")
        })
        .unwrap_or(false)
}

/// Validates that an optional path from the UI was provided AND points at
/// a file that exists. Returns a friendly error message if either check
/// fails. Used for things like the Whisper model path.
fn require_path(value: Option<String>, label: &str) -> Result<PathBuf, String> {
    let Some(value) = value else {
        return Err(format!("{} is required", label));
    };
    let path = PathBuf::from(value);
    if !path.is_file() {
        return Err(format!(
            "{} was not found: {}",
            label,
            path.to_string_lossy()
        ));
    }
    Ok(path)
}

/// Deletes a list of files and/or folders. Reports each successful deletion
/// to the log panel and warns on any that couldn't be removed. Returns the
/// list of paths that were actually cleaned up.
fn clean_paths(app: &AppHandle, paths: &[PathBuf]) -> Vec<String> {
    let mut cleaned = Vec::new();
    for path in paths {
        if path.exists() {
            let result = if path.is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };
            match result {
                Ok(_) => {
                    emit(
                        app,
                        "stage",
                        "cleanup",
                        format!("Removed {}", path.to_string_lossy()),
                    );
                    cleaned.push(path.to_string_lossy().to_string());
                }
                Err(e) => emit(
                    app,
                    "warn",
                    "cleanup",
                    format!("Could not remove {}: {}", path.to_string_lossy(), e),
                ),
            }
        }
    }
    cleaned
}

// =============================================================================
// Tauri commands — these are the four entry points the UI can call.
// `#[tauri::command]` registers each one with the Tauri runtime so JavaScript
// can `invoke()` it by name.
// =============================================================================

/// Tauri command: inspect the local machine and report which CLI tools and
/// Whisper models are available. The UI calls this on launch and whenever
/// the user clicks "Check Tools".
#[tauri::command]
fn check_environment() -> EnvironmentReport {
    let tools = detect_tools();
    EnvironmentReport {
        dependencies: vec![
            DependencyStatus {
                name: "yt-dlp".to_string(),
                found: tools.yt_dlp.is_some(),
                path: tools.yt_dlp.map(|p| p.to_string_lossy().to_string()),
            },
            DependencyStatus {
                name: "ffmpeg".to_string(),
                found: tools.ffmpeg.is_some(),
                path: tools.ffmpeg.map(|p| p.to_string_lossy().to_string()),
            },
            DependencyStatus {
                name: "whisper-cli".to_string(),
                found: tools.whisper_cli.is_some(),
                path: tools.whisper_cli.map(|p| p.to_string_lossy().to_string()),
            },
        ],
        models: scan_models(),
    }
}

/// Tauri command: the big one. Handles every variation of the YouTube URL
/// tab — captions only, Whisper only, hybrid merge, media download, and
/// every combination. The flow at a high level:
///
///   1. Set up an output folder and a scratch temp folder.
///   2. Look up the video's title (so output files are named after it).
///   3. If captions were requested, ask yt-dlp for them + the metadata
///      sidecars.
///   4. If we need audio (because the user asked for Whisper or just media),
///      download it with yt-dlp.
///   5. If Whisper was requested, transcode to WAV and run whisper-cli.
///   6. If Hybrid mode was requested, merge the two transcripts.
///   7. Clean up temp files and return the final list of outputs.
#[tauri::command]
fn run_youtube_job(app: AppHandle, request: YoutubeJobRequest) -> Result<JobResult, String> {
    // 1. Make sure the output folder exists and grab the CLI tool paths.
    let output_dir = PathBuf::from(request.output_dir.trim());
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Could not create output folder: {}", e))?;

    let tools = detect_tools();
    let yt_dlp = tools
        .yt_dlp
        .ok_or_else(|| "yt-dlp was not found".to_string())?;
    let ffmpeg = tools.ffmpeg;
    let whisper_cli = tools.whisper_cli;
    let temp_dir = unique_temp_dir(&output_dir)?;
    let mut outputs = Vec::new();
    // Things we'll delete at the end (the temp folder itself, plus anything
    // we downloaded but won't be keeping).
    let mut cleanup_candidates = vec![temp_dir.clone()];

    emit(&app, "stage", "setup", "Preparing YouTube job");
    // 2. Resolve the video's title — used to name all output files.
    let base = get_youtube_title(&yt_dlp, &request.url);
    emit(&app, "info", "setup", format!("Output prefix: {}", base));

    // 3. Captions phase — does this job need YouTube's existing captions?
    // (Captions are required for any mode except "whisper only" or "media only".)
    let captions_requested = matches!(
        request.transcript_source.as_str(),
        "captions_fallback" | "captions_only" | "both" | "hybrid"
    );
    let mut youtube_srt_path: Option<PathBuf> = None;
    let mut info_json_path: Option<PathBuf> = None;
    let mut metadata_paths: Vec<PathBuf> = Vec::new();
    let mut had_captions = false;
    if captions_requested {
        let caption_download =
            download_captions(&app, &yt_dlp, &request.url, &temp_dir, &output_dir, &base)?;
        had_captions = !caption_download.transcripts.is_empty();
        outputs.extend(
            caption_download
                .transcripts
                .iter()
                .map(|p| p.to_string_lossy().to_string()),
        );
        youtube_srt_path = caption_download.srt_path;
        for sidecar in &caption_download.metadata {
            outputs.push(sidecar.to_string_lossy().to_string());
            if sidecar
                .file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.ends_with(".info.json"))
                .unwrap_or(false)
            {
                info_json_path = Some(sidecar.clone());
            }
            metadata_paths.push(sidecar.clone());
        }
        if request.transcript_source == "captions_only" && !had_captions {
            let cleaned = clean_paths(&app, &cleanup_candidates);
            emit(
                &app,
                "error",
                "captions",
                "No YouTube captions were available for this URL",
            );
            return Err(format!(
                "No YouTube captions were available. Cleaned {} temporary item(s).",
                cleaned.len()
            ));
        }
        if request.transcript_source == "hybrid" && !had_captions {
            emit(
                &app,
                "warn",
                "hybrid",
                "No YouTube captions found — falling back to Whisper-only; no merge will be produced",
            );
        }
    }

    // 4-5. Decide whether we need to download media, run Whisper, or both.
    // `media_only` means the user wants the video file but no transcript.
    // `whisper_requested` is true any time we'll be running whisper-cli.
    let media_only = request.transcript_source == "none";
    let whisper_requested = match request.transcript_source.as_str() {
        "whisper_only" | "both" | "hybrid" => true,
        // For "captions if available, otherwise Whisper", only run Whisper
        // when captions were missing.
        "captions_fallback" => !had_captions,
        _ => false,
    };

    let mut whisper_srt_path: Option<PathBuf> = None;

    if should_download_media(media_only, request.keep_media, whisper_requested) {
        // If the user wants to keep the media, download it straight into
        // the output folder. Otherwise we download into a temp folder that
        // gets cleaned up at the end.
        let media_output_dir = if request.keep_media || media_only {
            output_dir.clone()
        } else {
            temp_dir.clone()
        };
        let downloaded_media = download_media(
            &app,
            &yt_dlp,
            ffmpeg.as_deref(),
            &request.url,
            &media_output_dir,
            &base,
            &request.media_action,
        )?;

        let media_metadata = collect_metadata_sidecars(
            &app,
            &media_output_dir,
            &base,
            &output_dir,
            &base,
        );
        for sidecar in &media_metadata {
            if metadata_paths.iter().any(|existing| existing == sidecar) {
                continue;
            }
            outputs.push(sidecar.to_string_lossy().to_string());
            if sidecar
                .file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.ends_with(".info.json"))
                .unwrap_or(false)
                && info_json_path.is_none()
            {
                info_json_path = Some(sidecar.clone());
            }
            metadata_paths.push(sidecar.clone());
        }

        if request.keep_media || media_only {
            outputs.extend(
                downloaded_media
                    .iter()
                    .map(|path| path.to_string_lossy().to_string()),
            );
        } else {
            cleanup_candidates.extend(downloaded_media.iter().cloned());
        }

        if whisper_requested {
            let whisper_cli = whisper_cli.ok_or_else(|| "whisper-cli was not found".to_string())?;
            let ffmpeg = ffmpeg.ok_or_else(|| "ffmpeg was not found".to_string())?;
            let model_path = require_path(request.model_path, "Whisper model")?;
            let media = find_audio_media(&ffmpeg, &downloaded_media).ok_or_else(|| {
                "Downloaded media did not contain an audio stream for Whisper".to_string()
            })?;
            let wav = temp_dir.join(format!("{}.whisper-input.wav", base));
            convert_to_wav(&app, &ffmpeg, &media, &wav)?;
            cleanup_candidates.push(wav.clone());
            let prefix = output_dir.join(format!("{}.whisper", base));
            let whisper_outputs = run_whisper(
                &app,
                &whisper_cli,
                &model_path,
                &wav,
                &prefix,
                request.whisper_prompt.as_deref(),
                request.max_len,
            )?;
            for path in &whisper_outputs {
                if path.extension().and_then(|s| s.to_str()) == Some("srt") {
                    whisper_srt_path = Some(path.clone());
                }
            }
            outputs.extend(
                whisper_outputs
                    .iter()
                    .map(|p| p.to_string_lossy().to_string()),
            );
        }
    }

    // 6. Hybrid merge — only runs if the user picked Hybrid mode AND we
    // actually ended up with both transcripts. If captions were missing
    // (or Whisper somehow didn't produce SRT), we just skip the merge
    // and ship whatever transcripts we have.
    if request.transcript_source == "hybrid" {
        if let (Some(whisper_srt), Some(yt_srt)) =
            (whisper_srt_path.as_ref(), youtube_srt_path.as_ref())
        {
            emit(
                &app,
                "stage",
                "hybrid",
                "Merging Whisper prose with YouTube proper nouns",
            );
            let hybrid_txt = output_dir.join(format!("{}.hybrid.txt", base));
            let hybrid_srt = output_dir.join(format!("{}.hybrid.srt", base));
            let hybrid_flagged = output_dir.join(format!("{}.hybrid.flagged.txt", base));
            match hybrid::build_hybrid(
                whisper_srt,
                yt_srt,
                info_json_path.as_deref(),
                &hybrid_txt,
                &hybrid_srt,
                &hybrid_flagged,
            ) {
                Ok(result) => {
                    outputs.push(result.out_srt.to_string_lossy().to_string());
                    outputs.push(result.out_txt.to_string_lossy().to_string());
                    if let Some(flagged) = result.out_flagged {
                        outputs.push(flagged.to_string_lossy().to_string());
                    }
                    emit(
                        &app,
                        "stage",
                        "hybrid",
                        format!(
                            "Hybrid transcript ready: {} proper-noun replacement(s), {} flagged segment(s)",
                            result.replacements, result.flagged_segments
                        ),
                    );
                }
                Err(error) => {
                    emit(
                        &app,
                        "warn",
                        "hybrid",
                        format!("Could not build hybrid transcript: {}", error),
                    );
                }
            }
        } else if had_captions {
            emit(
                &app,
                "warn",
                "hybrid",
                "Missing one of the source transcripts — skipping merge",
            );
        }
    }

    // 7. Remove the temp folder and any intermediate downloads.
    emit(&app, "stage", "cleanup", "Cleaning temporary files");
    let cleaned = clean_paths(&app, &cleanup_candidates);
    emit(&app, "stage", "done", "Job complete");

    Ok(JobResult {
        status: "complete".to_string(),
        outputs,
        cleaned,
    })
}

/// Tauri command: handles the Local Media tab — transcribing a file that's
/// already on disk. Optionally just converts it to WAV, or just runs
/// Whisper on an already-WAV file, or does both.
#[tauri::command]
fn run_local_job(app: AppHandle, request: LocalJobRequest) -> Result<JobResult, String> {
    let input = PathBuf::from(request.input_file.trim());
    if !input.is_file() {
        return Err(format!(
            "Input file was not found: {}",
            input.to_string_lossy()
        ));
    }

    let output_dir = PathBuf::from(request.output_dir.trim());
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Could not create output folder: {}", e))?;
    let tools = detect_tools();
    let base = base_from_input(&input);
    let mut outputs = Vec::new();

    emit(&app, "stage", "setup", "Preparing local media job");

    let wav = if request.mode == "transcribe_only" {
        input.clone()
    } else {
        let ffmpeg = tools
            .ffmpeg
            .ok_or_else(|| "ffmpeg was not found".to_string())?;
        let wav = output_dir.join(format!("{}.converted.wav", base));
        let converted = convert_to_wav(&app, &ffmpeg, &input, &wav)?;
        outputs.push(converted.to_string_lossy().to_string());
        wav
    };

    if request.mode == "transcribe_only" || request.mode == "convert_transcribe" {
        let whisper_cli = tools
            .whisper_cli
            .ok_or_else(|| "whisper-cli was not found".to_string())?;
        let model_path = require_path(request.model_path, "Whisper model")?;
        let prefix = output_dir.join(format!("{}.whisper", base));
        let whisper_outputs = run_whisper(
            &app,
            &whisper_cli,
            &model_path,
            &wav,
            &prefix,
            request.whisper_prompt.as_deref(),
            request.max_len,
        )?;
        outputs.extend(
            whisper_outputs
                .iter()
                .map(|p| p.to_string_lossy().to_string()),
        );
    }

    emit(&app, "stage", "done", "Job complete");
    Ok(JobResult {
        status: "complete".to_string(),
        outputs,
        cleaned: Vec::new(),
    })
}

/// Tauri command: handles the Merge Transcripts tab. Takes a Whisper SRT
/// and a YouTube SRT (plus an optional .info.json), produces the hybrid
/// `.hybrid.srt` + `.hybrid.txt` outputs without doing any downloading or
/// Whisper-running. Useful when you already have both transcripts on hand
/// and just want the merge.
#[tauri::command]
fn run_hybrid_merge_job(app: AppHandle, request: MergeJobRequest) -> Result<JobResult, String> {
    let whisper_srt = PathBuf::from(request.whisper_srt.trim());
    let youtube_srt = PathBuf::from(request.youtube_srt.trim());
    let info_json = request
        .info_json
        .as_ref()
        .map(|p| PathBuf::from(p.trim()))
        .filter(|p| !p.as_os_str().is_empty());

    if !whisper_srt.is_file() {
        return Err(format!(
            "Whisper SRT was not found: {}",
            whisper_srt.to_string_lossy()
        ));
    }
    if !youtube_srt.is_file() {
        return Err(format!(
            "YouTube captions SRT was not found: {}",
            youtube_srt.to_string_lossy()
        ));
    }
    if let Some(info) = info_json.as_ref() {
        if !info.is_file() {
            return Err(format!(
                "info.json was not found: {}",
                info.to_string_lossy()
            ));
        }
    }

    let output_dir = PathBuf::from(request.output_dir.trim());
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Could not create output folder: {}", e))?;

    let base_input = request
        .output_base
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let stem = whisper_srt
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("hybrid-transcript");
            stem.strip_suffix(".whisper").unwrap_or(stem).to_string()
        });
    let base = sanitize(&base_input);
    emit(&app, "stage", "setup", "Preparing hybrid merge");
    emit(&app, "info", "setup", format!("Output prefix: {}", base));

    let hybrid_txt = output_dir.join(format!("{}.hybrid.txt", base));
    let hybrid_srt = output_dir.join(format!("{}.hybrid.srt", base));
    let hybrid_flagged = output_dir.join(format!("{}.hybrid.flagged.txt", base));

    emit(
        &app,
        "stage",
        "hybrid",
        "Merging Whisper prose with YouTube proper nouns",
    );
    let result = hybrid::build_hybrid(
        &whisper_srt,
        &youtube_srt,
        info_json.as_deref(),
        &hybrid_txt,
        &hybrid_srt,
        &hybrid_flagged,
    )
    .map_err(|e| format!("Hybrid merge failed: {}", e))?;

    let mut outputs = vec![
        result.out_srt.to_string_lossy().to_string(),
        result.out_txt.to_string_lossy().to_string(),
    ];
    if let Some(flagged) = &result.out_flagged {
        outputs.push(flagged.to_string_lossy().to_string());
    }
    emit(
        &app,
        "stage",
        "hybrid",
        format!(
            "Hybrid transcript ready: {} proper-noun replacement(s), {} flagged segment(s)",
            result.replacements, result.flagged_segments
        ),
    );

    emit(&app, "stage", "done", "Job complete");
    Ok(JobResult {
        status: "complete".to_string(),
        outputs,
        cleaned: Vec::new(),
    })
}

// =============================================================================
// Unit tests — run via `cargo test`. These only verify a few small helpers;
// the heavy testing happens against `hybrid.rs`'s pure functions.
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_finished_media_file_for_base() {
        let path = Path::new("/tmp/Video.Title.mp4");
        assert!(media_file_matches_base(path, "Video.Title"));
    }

    #[test]
    fn rejects_yt_dlp_format_artifact_as_finished_media() {
        let path = Path::new("/tmp/Video.Title.f234.mp4");
        assert!(!media_file_matches_base(path, "Video.Title"));
        assert!(media_file_is_format_artifact(path, "Video.Title"));
    }

    #[test]
    fn downloads_media_when_user_keeps_media_after_captions() {
        assert!(should_download_media(false, true, false));
    }
}

// =============================================================================
// Application entry point. `main.rs` calls this; it boots up the Tauri
// runtime, registers our commands, and shows the app window.
// =============================================================================

/// Boots the Tauri app. Installs two helper plugins (opener, for revealing
/// files in Finder; dialog, for the "Browse" file pickers) and registers
/// the four `#[tauri::command]` functions above so the UI can invoke them.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            check_environment,
            run_youtube_job,
            run_local_job,
            run_hybrid_merge_job
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
