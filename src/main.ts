import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { revealItemInDir, openPath } from "@tauri-apps/plugin-opener";

type DependencyStatus = {
  name: string;
  found: boolean;
  path?: string | null;
};

type EnvironmentReport = {
  dependencies: DependencyStatus[];
  models: string[];
};

type JobEvent = {
  level: string;
  stage: string;
  message: string;
};

type JobResult = {
  status: string;
  outputs: string[];
  cleaned: string[];
};

const $ = <T extends HTMLElement>(selector: string): T => {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`Missing element: ${selector}`);
  return element;
};

const youtubeTab = $("#youtube-tab");
const localTab = $("#local-tab");
const youtubePanel = $("#youtube-panel");
const localPanel = $("#local-panel");
const environmentStatus = $("#environment-status");
const modelSelect = $("#model-select") as HTMLSelectElement;
const logOutput = $("#log-output");
const progressList = $("#progress-list");
const outputList = $("#output-list");
const jobStatus = $("#job-status");
const revealOutputButton = $("#reveal-output") as HTMLButtonElement;
const startButton = $("#start-job") as HTMLButtonElement;

let activeMode: "youtube" | "local" = "youtube";
let activeOutputFolder = "";
let running = false;

function input(selector: string): HTMLInputElement {
  return $(selector) as HTMLInputElement;
}

function select(selector: string): HTMLSelectElement {
  return $(selector) as HTMLSelectElement;
}

function textarea(selector: string): HTMLTextAreaElement {
  return $(selector) as HTMLTextAreaElement;
}

function setActiveMode(mode: "youtube" | "local") {
  activeMode = mode;
  youtubeTab.classList.toggle("active", mode === "youtube");
  localTab.classList.toggle("active", mode === "local");
  youtubePanel.classList.toggle("active", mode === "youtube");
  localPanel.classList.toggle("active", mode === "local");
}

function appendLog(event: JobEvent) {
  const prefix = `[${event.stage}] ${event.level.toUpperCase()}`;
  logOutput.textContent += `${prefix}: ${event.message}\n`;
  logOutput.scrollTop = logOutput.scrollHeight;

  if (event.level === "stage") {
    const li = document.createElement("li");
    li.textContent = event.message;
    li.dataset.stage = event.stage;
    progressList.appendChild(li);
  }
}

function clearRunState() {
  logOutput.textContent = "";
  progressList.innerHTML = "";
  outputList.innerHTML = "";
  revealOutputButton.disabled = true;
}

function setRunning(value: boolean) {
  running = value;
  startButton.disabled = value;
  startButton.textContent = value ? "Running" : "Start";
  jobStatus.textContent = value ? "Running" : "Idle";
}

function renderOutputs(paths: string[]) {
  outputList.innerHTML = "";
  for (const path of paths) {
    const li = document.createElement("li");
    const button = document.createElement("button");
    button.type = "button";
    button.className = "link-button";
    button.textContent = path.split(/[\\/]/).pop() || path;
    button.title = path;
    button.addEventListener("click", () => revealItemInDir(path));
    li.appendChild(button);
    outputList.appendChild(li);
  }
}

function renderEnvironment(report: EnvironmentReport) {
  environmentStatus.innerHTML = "";
  for (const dep of report.dependencies) {
    const item = document.createElement("div");
    item.className = `tool-status ${dep.found ? "ok" : "missing"}`;
    item.innerHTML = `<strong>${dep.name}</strong><span>${dep.found ? dep.path : "Not found"}</span>`;
    environmentStatus.appendChild(item);
  }

  modelSelect.innerHTML = "";
  if (report.models.length === 0) {
    const option = document.createElement("option");
    option.value = "";
    option.textContent = "No .bin models detected";
    modelSelect.appendChild(option);
  } else {
    for (const model of report.models) {
      const option = document.createElement("option");
      option.value = model;
      option.textContent = model;
      modelSelect.appendChild(option);
    }
  }
}

async function refreshEnvironment() {
  const report = await invoke<EnvironmentReport>("check_environment");
  renderEnvironment(report);
}

async function pickDirectory(target: HTMLInputElement) {
  const selected = await open({ directory: true, multiple: false, canCreateDirectories: true });
  if (typeof selected === "string") target.value = selected;
}

async function pickLocalFile() {
  const selected = await open({
    multiple: false,
    filters: [
      { name: "Media", extensions: ["mp4", "m4a", "mp3", "wav", "mov", "mkv", "webm", "aac", "flac", "ogg"] },
      { name: "All Files", extensions: ["*"] },
    ],
  });
  if (typeof selected === "string") input("#local-input").value = selected;
}

function applyYoutubePreset() {
  const preset = select("#youtube-preset").value;
  const source = select("#youtube-transcript-source");
  const keepMedia = input("#youtube-keep-media");
  const mediaAction = select("#youtube-media-action");

  if (preset === "fallback") {
    source.value = "captions_fallback";
    keepMedia.checked = false;
    mediaAction.value = "audio";
  } else if (preset === "media_transcript") {
    source.value = "captions_fallback";
    keepMedia.checked = true;
  } else if (preset === "transcript_only") {
    source.value = "captions_fallback";
    keepMedia.checked = false;
    mediaAction.value = "audio";
  } else if (preset === "media_only") {
    source.value = "none";
    keepMedia.checked = true;
  } else if (preset === "captions_only") {
    source.value = "captions_only";
    keepMedia.checked = false;
  } else if (preset === "whisper_only") {
    source.value = "whisper_only";
    keepMedia.checked = false;
    mediaAction.value = "audio";
  } else if (preset === "both") {
    source.value = "both";
    keepMedia.checked = false;
    mediaAction.value = "audio";
  }
}

function requireValue(selector: string, label: string): string {
  const value = input(selector).value.trim();
  if (!value) throw new Error(`${label} is required`);
  return value;
}

function modelValue(): string | null {
  const value = modelSelect.value.trim();
  return value || null;
}

function maxLenValue(): number | null {
  const value = input("#max-len").value.trim();
  return value ? Number(value) : null;
}

async function runYoutubeJob() {
  activeOutputFolder = requireValue("#youtube-output", "Output folder");
  const request = {
    url: requireValue("#youtube-url", "YouTube URL"),
    outputDir: activeOutputFolder,
    mediaAction: select("#youtube-media-action").value,
    transcriptSource: select("#youtube-transcript-source").value,
    keepMedia: input("#youtube-keep-media").checked,
    modelPath: modelValue(),
    whisperPrompt: textarea("#whisper-prompt").value,
    maxLen: maxLenValue(),
  };
  return invoke<JobResult>("run_youtube_job", { request });
}

async function runLocalJob() {
  activeOutputFolder = requireValue("#local-output", "Output folder");
  const request = {
    inputFile: requireValue("#local-input", "Local media file"),
    outputDir: activeOutputFolder,
    mode: select("#local-mode").value,
    modelPath: modelValue(),
    whisperPrompt: textarea("#whisper-prompt").value,
    maxLen: maxLenValue(),
  };
  return invoke<JobResult>("run_local_job", { request });
}

async function startJob() {
  if (running) return;
  clearRunState();
  setRunning(true);
  jobStatus.textContent = "Running";

  try {
    const result = activeMode === "youtube" ? await runYoutubeJob() : await runLocalJob();
    renderOutputs(result.outputs);
    revealOutputButton.disabled = !activeOutputFolder;
    jobStatus.textContent = `Complete: ${result.outputs.length} output${result.outputs.length === 1 ? "" : "s"}`;
    appendLog({ level: "info", stage: "done", message: `Cleaned ${result.cleaned.length} temporary item(s)` });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    appendLog({ level: "error", stage: "error", message });
    jobStatus.textContent = "Failed";
  } finally {
    setRunning(false);
  }
}

window.addEventListener("DOMContentLoaded", async () => {
  await listen<JobEvent>("job-event", (event) => appendLog(event.payload));

  youtubeTab.addEventListener("click", () => setActiveMode("youtube"));
  localTab.addEventListener("click", () => setActiveMode("local"));
  $("#refresh-env").addEventListener("click", refreshEnvironment);
  $("#pick-youtube-output").addEventListener("click", () => pickDirectory(input("#youtube-output")));
  $("#pick-local-output").addEventListener("click", () => pickDirectory(input("#local-output")));
  $("#pick-local-input").addEventListener("click", pickLocalFile);
  $("#youtube-preset").addEventListener("change", applyYoutubePreset);
  $("#clear-log").addEventListener("click", clearRunState);
  startButton.addEventListener("click", startJob);
  revealOutputButton.addEventListener("click", async () => {
    if (activeOutputFolder) {
      try {
        await revealItemInDir(activeOutputFolder);
      } catch {
        await openPath(activeOutputFolder);
      }
    }
  });

  input("#youtube-url").addEventListener("input", () => setActiveMode("youtube"));
  applyYoutubePreset();
  await refreshEnvironment();
});
