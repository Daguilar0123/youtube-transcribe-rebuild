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

#[derive(Debug, Clone, Serialize)]
struct JobEvent {
    level: String,
    stage: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct DependencyStatus {
    name: String,
    found: bool,
    path: Option<String>,
}

#[derive(Debug, Serialize)]
struct EnvironmentReport {
    dependencies: Vec<DependencyStatus>,
    models: Vec<String>,
}

#[derive(Debug, Serialize)]
struct JobResult {
    status: String,
    outputs: Vec<String>,
    cleaned: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YoutubeJobRequest {
    url: String,
    output_dir: String,
    media_action: String,
    transcript_source: String,
    keep_media: bool,
    model_path: Option<String>,
    whisper_prompt: Option<String>,
    max_len: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalJobRequest {
    input_file: String,
    output_dir: String,
    mode: String,
    model_path: Option<String>,
    whisper_prompt: Option<String>,
    max_len: Option<u32>,
}

#[derive(Debug)]
struct ToolPaths {
    yt_dlp: Option<PathBuf>,
    ffmpeg: Option<PathBuf>,
    whisper_cli: Option<PathBuf>,
}

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

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

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

fn model_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home_dir() {
        dirs.push(home.join("whisper.cpp/models"));
        dirs.push(home.join("Downloads/whisper.cpp/models"));
    }
    dirs
}

fn scan_models() -> Vec<String> {
    let mut models = Vec::new();
    let mut seen = HashSet::new();

    for dir in model_dirs() {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension() == Some(OsStr::new("bin")) && seen.insert(path.clone()) {
                    models.push(path.to_string_lossy().to_string());
                }
            }
        }
    }

    models.sort_by(|a, b| model_rank(a).cmp(&model_rank(b)).then(a.cmp(b)));
    models
}

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

fn unique_temp_dir(output_dir: &Path) -> Result<PathBuf, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis();
    let dir = output_dir.join(format!(".yt-transcribe-tmp-{}", millis));
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create temp folder: {}", e))?;
    Ok(dir)
}

fn run_command(
    app: &AppHandle,
    stage: &str,
    program: &Path,
    args: &[String],
    working_dir: Option<&Path>,
) -> Result<(), String> {
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

    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to start {}: {}", program.to_string_lossy(), e))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut handles = Vec::new();

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

fn base_from_input(path: &Path) -> String {
    sanitize(
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("transcript"),
    )
}

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

fn download_captions(
    app: &AppHandle,
    yt_dlp: &Path,
    url: &str,
    temp_dir: &Path,
    output_dir: &Path,
    base: &str,
) -> Result<Vec<PathBuf>, String> {
    emit(
        app,
        "stage",
        "captions",
        "Checking for existing YouTube captions",
    );
    let outtmpl = temp_dir
        .join("captions.%(ext)s")
        .to_string_lossy()
        .to_string();
    let args = vec![
        "--skip-download".to_string(),
        "--write-subs".to_string(),
        "--write-auto-subs".to_string(),
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

    let _ = run_command(app, "captions", yt_dlp, &args, Some(temp_dir));
    let caption_file = newest_file_with_exts(temp_dir, &["srt", "vtt"]);
    let Some(caption_file) = caption_file else {
        emit(app, "info", "captions", "No YouTube captions were saved");
        return Ok(Vec::new());
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
        Ok(vec![srt_out, txt_out])
    } else {
        let txt = captions_to_txt(&caption_file)?;
        fs::write(&txt_out, txt).map_err(|e| format!("Could not save TXT captions: {}", e))?;
        fs::copy(&caption_file, &srt_out)
            .map_err(|e| format!("Could not save caption file: {}", e))?;
        emit(app, "stage", "captions", "Saved YouTube captions");
        Ok(vec![srt_out, txt_out])
    }
}

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

fn download_media(
    app: &AppHandle,
    yt_dlp: &Path,
    url: &str,
    output_dir: &Path,
    base: &str,
    media_action: &str,
) -> Result<PathBuf, String> {
    emit(app, "stage", "download", "Downloading YouTube media");
    let outtmpl = output_dir
        .join(format!("{}.%(ext)s", base))
        .to_string_lossy()
        .to_string();
    let format = if media_action == "video" {
        "bestvideo[ext=mp4]+bestaudio[ext=m4a]/best[ext=mp4]/best"
    } else {
        "bestaudio/best"
    };

    let args = vec![
        "--no-playlist".to_string(),
        "-f".to_string(),
        format.to_string(),
        "--merge-output-format".to_string(),
        "mp4".to_string(),
        "-o".to_string(),
        outtmpl,
        url.to_string(),
    ];

    run_command(app, "download", yt_dlp, &args, Some(output_dir))?;
    newest_media_file(output_dir)
        .ok_or_else(|| "yt-dlp finished but no media file was found".to_string())
}

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

fn newest_media_file(dir: &Path) -> Option<PathBuf> {
    newest_file_with_exts(
        dir,
        &[
            "mp4", "m4a", "mp3", "webm", "mkv", "mov", "aac", "opus", "ogg",
        ],
    )
}

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

#[tauri::command]
fn run_youtube_job(app: AppHandle, request: YoutubeJobRequest) -> Result<JobResult, String> {
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
    let mut cleanup_candidates = vec![temp_dir.clone()];

    emit(&app, "stage", "setup", "Preparing YouTube job");
    let base = get_youtube_title(&yt_dlp, &request.url);
    emit(&app, "info", "setup", format!("Output prefix: {}", base));

    let captions_requested = matches!(
        request.transcript_source.as_str(),
        "captions_fallback" | "captions_only" | "both"
    );
    let mut caption_outputs = Vec::new();
    if captions_requested {
        caption_outputs =
            download_captions(&app, &yt_dlp, &request.url, &temp_dir, &output_dir, &base)?;
        outputs.extend(
            caption_outputs
                .iter()
                .map(|p| p.to_string_lossy().to_string()),
        );
        if request.transcript_source == "captions_only" && caption_outputs.is_empty() {
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
    }

    let media_only = request.transcript_source == "none";
    let whisper_requested = match request.transcript_source.as_str() {
        "whisper_only" | "both" => true,
        "captions_fallback" => caption_outputs.is_empty(),
        _ => false,
    };

    if media_only || whisper_requested {
        let media_output_dir = if request.keep_media || media_only {
            output_dir.clone()
        } else {
            temp_dir.clone()
        };
        let media = download_media(
            &app,
            &yt_dlp,
            &request.url,
            &media_output_dir,
            &base,
            &request.media_action,
        )?;

        if request.keep_media || media_only {
            outputs.push(media.to_string_lossy().to_string());
        } else {
            cleanup_candidates.push(media.clone());
        }

        if whisper_requested {
            let whisper_cli = whisper_cli.ok_or_else(|| "whisper-cli was not found".to_string())?;
            let ffmpeg = ffmpeg.ok_or_else(|| "ffmpeg was not found".to_string())?;
            let model_path = require_path(request.model_path, "Whisper model")?;
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
            outputs.extend(
                whisper_outputs
                    .iter()
                    .map(|p| p.to_string_lossy().to_string()),
            );
        }
    }

    emit(&app, "stage", "cleanup", "Cleaning temporary files");
    let cleaned = clean_paths(&app, &cleanup_candidates);
    emit(&app, "stage", "done", "Job complete");

    Ok(JobResult {
        status: "complete".to_string(),
        outputs,
        cleaned,
    })
}

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            check_environment,
            run_youtube_job,
            run_local_job
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
