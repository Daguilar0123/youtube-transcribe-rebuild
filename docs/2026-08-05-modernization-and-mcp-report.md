# Modernization + MCP Report

**Date:** 2026-08-05
**Author:** Claude (Opus 5), at Danny's request
**Host:** Mac mini / MacBook, Apple M2 Pro, macOS 26.6 (build 25G5028f)
**Repo:** `~/Developer/youtube-transcribe-rebuild` — `main` @ `2240c67`, `origin/main` @ `e0d9226`

Everything below was verified by direct inspection on 2026-08-05. Claims that are
inference rather than measurement are marked as such.

---

## Post-verification corrections (2026-08-05, later the same day)

A five-agent verification pass (workflow `wf_8a094bb1-81e`: git forensics, code map,
environment audit, external research, adversarial plan critic) checked this report's
claims. Three corrections, on record per the dated-provenance convention:

1. **§4's "the app is broken" was over-claimed.** The app does not resolve the pip
   `yt-dlp` my shell test exercised. `path_entries()` checks `/usr/local/bin` ahead of
   the user's `PATH`, and an Intel-brew `yt-dlp` **2026.03.17** lives there (installed
   2026-05-07). Tested directly: that copy downloads captions fine. So: the *pip* copy
   (2025.04.30) was broken and is now fixed (2026.07.04), but the *app* was riding the
   brew copy and its caption path likely worked. Three `yt-dlp` installs now coexist;
   the implementation plan converges everything on ARM-brew.
2. **§2's "6 unique lines" undercounted.** I examined only the tracked half of the
   stash. The full audit (`git stash show` needs `-u` to reveal the untracked half,
   stored in `stash@{0}^3`) finds **19** raw-unique lines — 6 in `lib.rs`, 13 in
   `hybrid.rs`. The verdict is unchanged and now stronger: every one is a
   comment-stripped duplicate of a line `origin/main` carries with a trailing comment;
   normalized semantic content unique to the stash is **zero** in every file.
3. **All `lib.rs` line references below are stale.** They describe the pre-pull tree
   (`lib.rs` @ `2240c67`, 1,356 lines). Post-pull `lib.rs` is 1,665 lines — e.g.
   `check_environment` moved from :905 to :1163. The current, verified symbol map lives
   in `docs/2026-08-05-implementation-plan.yaml`, which supersedes §6–§7 of this report
   as the execution source of truth.

---

## 1. Chat-session archaeology

### What I searched

Every local transcript under `~/.claude/projects/**/*.jsonl` (48 project directories),
grepped for `youtube-transcribe` and for repo-relative file paths, then ranked by
reference count and filtered to sessions containing actual `Edit`/`Write` tool calls
against files in this repo.

### What I found

| Session | Location | Verdict |
|---|---|---|
| `a33d573f-268c-4adf-b330-c81d5f10ffdd` | this project dir | **This session** (2026-08-05). The only transcript in the project directory. |
| `90ca6bb9-c14c-45bd-b505-361b9ba226bc` | `-Users-danielaguilar-Developer/` | **False positive.** 2026-06-06/07, a legal-research session about AI/IP ownership. Mentions the repo only because it appeared in a `ls ~/Developer` listing. |
| 4 sessions in `TheTatteredRose-com-2025-platner-project` | — | **False positives.** Same cause — directory listings. |

**The important finding is a negative one:** there is no local transcript of the work
that actually built this app. The commit history shows substantial development
(12 commits, plus a 1,924-line PR), but none of it was done in a session whose
transcript is on this disk.

### Where that work actually happened

The evidence that survives locally:

- `.claude/worktrees/condescending-faraday-eb54fa/` — a full duplicate checkout, untracked.
- Local branch `claude/condescending-faraday-eb54fa` @ `fcfe46f`, plus its remote twin.
- `.claude/worktrees/condescending-faraday-eb54fa/.claude/settings.local.json` — the
  permission allowlist that session accumulated (`cargo check`, `cargo test`, `npx tsc`,
  `npm run *`, `git push *`, and a read grant for `~/Downloads/**`).
- GitHub PR #1, merged 2026-05-16T04:08:07Z, +1,924 / −34.

That naming pattern (`claude/<adjective>-<scientist>-<hash>`) is a **Claude Code cloud/web
session**. Its transcript lives server-side, not in `~/.claude/projects/`. So the
recoverable record of that work is the PR body and the diff — both of which are good,
and the PR body in particular reads like a proper memory entry.

**Implication for the workflow in `CLAUDE.md`:** cloud sessions break the "commit history
is the agent's timestamped memory" system at exactly one point — the conversation is
unrecoverable locally, so the PR body has to carry the whole record. PR #1's body does.
Worth keeping that standard deliberately rather than accidentally.

---

## 2. Git tree — a three-way divergence, and it is dangerous

```
e0d9226  origin/main, origin/HEAD   Add hybrid transcript mode with metadata sidecars (#1)
│  fcfe46f  claude/condescending-faraday-eb54fa   Commit Tauri icons …
│  af3b369  Add layman-readable documentation comments to Rust source
│  d8a9e5f  Replace machine-specific paths in README …
│  ae6f5a3  Add hybrid transcript mode with metadata sidecars
├──┘
2240c67  HEAD -> main  [behind 1]   Remove stale GitHub setup note
```

Three versions of the hybrid feature exist simultaneously:

| | `hybrid.rs` | `main.rs` | Status |
|---|---|---|---|
| `origin/main` | 1,018 lines | documented, 23 lines | **Canonical. Richest.** |
| local `main` (`2240c67`) | absent | 6-line stub | 1 commit behind |
| **working tree (uncommitted)** | **761 lines** | 6-line stub | **Stale earlier draft** |

Direction of divergence, measured on `lib.rs`:

- Lines present in the working tree but **not** in `origin/main`: **6** — and all six are
  ordinary struct fields (`level`, `stage`, `message`, `media_action`, `transcript_source`, `mode`).
- Lines present in `origin/main` but **not** in the working tree: **315**.

`index.html`, `src/main.ts`, `src/styles.css`, and `docs/hybrid-transcript-feature.md`
are already byte-identical to `origin/main`.

### The risk, stated plainly

The uncommitted working tree contains **nothing of value that `origin/main` lacks**. It is
an earlier, smaller draft of a feature that was subsequently finished, documented,
tested (13/13 unit tests per the PR), and merged.

A `git pull` today will conflict on `lib.rs` and `hybrid.rs`. Resolving those conflicts
"toward my local changes" — the intuitive move, since they're your uncommitted edits —
**silently deletes 315 lines of merged, tested work**, including the documentation pass
from `af3b369` and the whole richer half of the merge engine.

**Recommended resolution (destroys nothing):**

```bash
git stash push -u -m "2026-08-05 stale pre-PR#1 hybrid draft"   # keep a rescue copy
git pull --ff-only                                              # main -> e0d9226
# verify, then eventually: git stash drop
```

The `git stash` is belt-and-braces; the analysis says the draft is strictly subsumed.
I'd keep the stash around for a week, then drop it.

### Housekeeping also visible in `git status`

- `.claude/worktrees/` — 100+ untracked files from a duplicate checkout, cluttering every
  `git status`. Should be added to `.gitignore` (and the worktree pruned if finished).
- `docs/` is untracked locally but **already committed on `origin/main`** — another symptom
  of the un-pulled merge.
- `/Applications/YouTube Transcribe.app` is an **Automator stub from 2025-05-13**, unrelated
  to the Tauri app. Leftover from a predecessor; not the app this repo builds.

---

## 3. Rosetta compliance — the Apple article, and where this app stands

### What the article actually says

[HT102527, "Using Intel-based apps on a Mac with Apple silicon"](https://support.apple.com/en-us/102527),
published 2026-02-16:

> Rosetta is currently available for any Mac with Apple silicon, and it will remain
> available through the forthcoming **macOS 27** — the next major macOS release. Starting
> with computers using **macOS 28**, Rosetta functionality will be available only for
> certain older, unmaintained games that rely on Intel-based frameworks.

You are on macOS 26.6. So the runway is: **macOS 27 works, macOS 28 does not.** The article's
own compliance test is the Finder **Get Info → Kind:** field — `Application (Intel)` is the
failing state; `Application (Universal)` or `Application (Apple silicon)` passes.

The article also explicitly extends the requirement past the app itself:

> Some apps might include or use components, such as extensions or updaters, that need to
> be updated separately.

That sentence is the whole problem here. This app is a thin Tauri shell around three
external executables, and **the app being native is not sufficient** — `yt-dlp`, `ffmpeg`,
and `whisper-cli` each have to be native too.

### Measured architecture of the full pipeline

| Component | Path | Arch | Verdict |
|---|---|---|---|
| **Rust toolchain** | `~/.rustup/toolchains/stable-x86_64-apple-darwin` | **x86_64** | ❌ **Root cause** — only toolchain installed |
| `cargo` | `~/.cargo/bin/cargo` | **x86_64** | ❌ runs under Rosetta |
| Built app binary | `target/release/bundle/macos/…/youtube-transcribe-rebuild` | **x86_64** | ❌ `Application (Intel)` |
| `ffmpeg` | `/usr/local/Cellar/ffmpeg/7.1_4/bin/ffmpeg` | **x86_64** | ❌ Intel Homebrew |
| `ffprobe` | same Cellar | **x86_64** | ❌ |
| `whisper-cli` | `~/whisper.cpp/build/bin/whisper-cli` | **x86_64** | ❌ **and no Metal** — see below |
| `yt-dlp` | `~/Library/Python/3.12/bin/yt-dlp` | Python script on universal `python3` | ✅ runs arm64 |
| `node` | nvm v24.15.0 | arm64 | ✅ |

Homebrew is installed twice: `/usr/local` (Intel) carries **122 packages**; `/opt/homebrew`
(Apple silicon) carries **0**. Every native CLI dependency on this machine is Intel.

### The `whisper-cli` finding is the expensive one

`otool -L` shows `whisper-cli` links `libggml-metal.dylib` — it was *built* with Metal
support. But the binary is x86_64, so under Rosetta it **cannot use the M2 Pro GPU at all**.
Transcription is currently running CPU-only, through binary translation, with translated
SIMD.

I did not benchmark this, so I won't quote a speedup figure — but rebuilding
`whisper.cpp` as arm64 with Metal is almost certainly the single largest performance
change available to this project, independent of the Rosetta deadline.

### One nuance worth knowing

The app's own dependency search in `lib.rs:99-116` already lists `/opt/homebrew/bin`
**first** in `path_entries()`. So the moment an arm64 `ffmpeg` is installed there, the app
picks it up with **zero code changes**. That was good foresight.

Also note that PR #1's body already flagged this issue:

> The release build was previously x64 due to a Rust toolchain quirk in the README; I had
> to install `@tauri-apps/cli-darwin-arm64` to get the rebuild going, and the resulting
> bundle is now `aarch64`. Worth confirming that's intentional before tagging a release.

`node_modules/@tauri-apps/cli-darwin-arm64` **is** present. But the Rust toolchain here is
still x86_64-only, and `src-tauri/target/` contains no `aarch64-apple-darwin` directory —
so a local rebuild *today* still produces an Intel binary. The cloud builder that produced
the PR evidently had an arm64 toolchain; this machine does not.

---

## 4. A more urgent problem than Rosetta: the app is broken right now

While testing the caption path I hit a present-tense failure, not a future one.

**Installed `yt-dlp`: `2025.04.30`. Current release: `2026.07.04`.** Roughly 14 months stale.

Live test of the exact caption invocation the app uses (`lib.rs:402-417`):

```
[info] Downloading subtitles: en, en-en, en-orig, en-de-DE, …
[info] Writing video subtitles to: cap.en.vtt
ERROR: Did not get any data blocks
[info] Writing video subtitles to: cap.en-en.vtt
ERROR: Did not get any data blocks
[info] Writing video subtitles to: cap.en-orig.vtt
ERROR: Did not get any data blocks
[info] Writing video subtitles to: cap.en-de-DE.vtt
ERROR: Did not get any data blocks
ERROR: Unable to download video subtitles for 'en-ja': HTTP Error 429: Too Many Requests
```

Zero caption files were written. `--list-subs` correctly *enumerates* the tracks, so
discovery works and only retrieval fails — the signature of a stale extractor against a
changed YouTube caption endpoint.

Two caveats, stated honestly: the trailing `429` is rate limiting and is at least partly
my own doing (I made several requests in a row). But the four `Did not get any data blocks`
errors occurred **before** the 429 and are a distinct failure. I did not re-test after a
cooldown, so I'd call this **strongly indicated, not conclusively isolated**.

Either way the consequence is the same and it is severe:

- **Captions-only mode:** broken.
- **Captions-fallback mode:** silently degrades to Whisper every time.
- **Hybrid mode — the feature PR #1 was built for:** cannot produce a merge at all, because
  it needs the YouTube SRT as one of its two inputs. It will hit the
  `"No YouTube captions found — falling back to Whisper-only"` branch (`lib.rs:994-1001`)
  on every video.

The main format-download path also emits `Signature extraction failed` and SABR warnings,
so media downloads are degrading too.

Commit `47952bd` "Document yt-dlp update guidance" already anticipated exactly this. The
guidance is in the README; it just hasn't been run.

### RESOLVED — 2026-08-05

Upgraded `2025.04.30` → `2026.07.04` and installed the missing `charset_normalizer`.
Re-ran the app's exact caption invocation against a different video: **four `.srt` files,
a `.description`, and an `.info.json` all written successfully.** Caption retrieval works
again, which unblocks hybrid mode. The `RequestsDependencyWarning` noise is also gone.

One new, non-blocking warning appeared:

> The extractor specified to use impersonation for this download, but no impersonate
> target is available.

Captions downloaded fine regardless. If YouTube tightens further, the fix is
`pip install "yt-dlp[default,curl-cffi]"`. Noting it here so the next person doesn't
have to rediscover it.

**This confirms the diagnosis above was the extractor, not the rate limit.**

### Other decay found

- `yt-dlp` is missing `chardet`/`charset_normalizer`, so every single invocation prints a
  `RequestsDependencyWarning` to stderr — which the app faithfully surfaces into its log
  pane as noise on every run.
- Only **one** usable Whisper model is present: `ggml-medium.en.bin` (1.5 GB). Everything
  else in `~/whisper.cpp/models/` is a `for-tests-*` stub. The app's
  `is_plausible_whisper_model()` correctly filters those out (10 MB floor + `for-tests`
  exclusion, `lib.rs:182-199`), so the model dropdown has exactly one real entry. That's
  correct behavior, but worth knowing it's not a bug in the picker.

---

## 5. What Claude Code actually does with a bare YouTube URL

This is the section you asked for specifically. Short version: **Claude Code has no YouTube
capability. There is no YouTube tool.** What follows is what it does instead.

### Measured, not guessed

I extracted every tool call across all local transcripts whose *input* contained a YouTube
URL:

| Tool | Calls |
|---|---|
| `WebFetch` | **28** |
| `WebSearch` | 2 |
| `mcp__plugin_chrome-devtools-mcp__new_page` | 1 |
| `mcp__Claude_Browser__preview_start` | 1 |
| `Bash` (a `curl` to the oEmbed endpoint) | 1 |
| **`yt-dlp`** | **0** |
| **this app** | **0** |

`WebFetch` is the overwhelming default reflex — ~85% of all attempts.

### Live A/B test, run today on the same URL

**Path A — `WebFetch` on `youtube.com/watch?v=…`.** Asked for title, channel, upload date,
view count, description, and transcript. It returned:

> **Video Title:** "Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)"
>
> **Missing Information:** Channel name · Upload date · View count · Full description ·
> Transcript/captions
>
> The provided page content only contains YouTube's footer navigation and legal links.

One field, scraped out of the `<title>` tag. `youtube.com/watch` is a JavaScript shell, so
the markdown conversion yields chrome and legal links.

**Path B — `yt-dlp`, i.e. what this app already does.** Title, channel, and duration in
**4.0 seconds**, plus a full caption inventory (`en`, `de-DE`, `ja`, `pt-BR`, `es-419`, …
in `vtt`/`ttml`/`srv3`/`srv2`/`srv1`/`json3`).

### The four fallback paths and their real costs

1. **`WebFetch`** — the default. Returns a title and nothing else. Its worst property is
   not that it fails but that it *appears* to succeed: it returns a well-formed answer
   containing one true fact, so the failure is easy to miss and easy to build on.

2. **`WebSearch`** — finds third-party writing *about* the video, never the words in it.
   Anything downstream is a summary of a summary, with the paraphrase and hallucination
   risk that implies. This is the most dangerous path, because it produces confident,
   fluent, unsourced content.

3. **`Bash` + `curl` to `youtube.com/oembed`** — title, author, thumbnail. Cheap, honest,
   still no transcript.

4. **Browser automation** (`chrome-devtools` or `claude-in-chrome`) — navigate, expand the
   description, click "Show transcript", read the DOM panel. This one genuinely works. It
   also: requires a logged-in Chrome, breaks whenever YouTube reshuffles its layout, often
   needs screenshots to find the controls, and pulls the entire caption panel into context
   as raw tokens — including YouTube's rolling-caption duplication, which the model then
   has to mentally de-duplicate.

5. **`Bash` + ad-hoc `yt-dlp`** — works, and is what Claude *should* reach for. But every
   session re-derives the flag incantation from scratch, no session dedupes rolling
   captions, none does the hybrid merge, and the result lands in context as raw SRT.

### Why raw SRT is the wrong thing to put in context

An SRT block costs three lines to deliver one line of speech:

```
147
00:12:34,560 --> 00:12:37,880
so the question becomes what we owe each other
```

The index line and the timing line are pure overhead, and YouTube's auto-captions repeat
each phrase across successive rolling frames on top of that. For transcript *analysis* —
which is what you actually want — none of that carries meaning.

This app already solves it. `captions_to_txt()` (`lib.rs:546-577`) strips indices, timing
lines, `WEBVTT`/`NOTE`/`Kind:`/`Language:` headers and `<c>` markup, and drops consecutive
duplicate lines — and the app writes that `.txt` alongside the `.srt` on every run. The
deduping logic in `hybrid.rs` goes further.

**Measured, after the `yt-dlp` fix in §4.** Real auto-captions from a 10:28 news segment
(*"Reporter who broke the story on new Graham Platner sexual assault accusation SPEAKS
OUT"*, MS NOW), run through the app's own `captions_to_txt` logic:

| Artifact | Chars | ≈Tokens | Lines |
|---|---:|---:|---:|
| Raw SRT (what an ad-hoc `yt-dlp` dumps into context) | 16,837 | ~4,209 | 1,156 |
| Deduped prose `.txt` (what the app already writes) | 8,236 | ~2,059 | 424 |

**51.1% reduction — 2.04× smaller — on a ten-minute video, with zero loss of meaning.**
The ratio holds or improves with length, since rolling-caption duplication scales with
runtime. On an hour-long talk this is the difference between ~25k and ~12k tokens.

So the token argument for the MCP is not speculative: **the app already produces the cheap,
clean artifact; Claude just has no way to ask for it.**

**One sharp finding for the MCP design:** the `.info.json` sidecar for that same 10-minute
video is **1,060,762 characters (~265,000 tokens)** — it carries every available format
variant, thumbnail, and caption track URL. It is genuinely useful to the *merge* (the
proper-noun vocabulary comes from it) but it must **never** be returned to a model. Any MCP
tool that touches `.info.json` has to project a handful of fields out of it (title, channel,
uploader, description, duration) and discard the rest.

### The strategic point

Your instinct is right, and it's sharper than "save tokens." The app doesn't just do this
*cheaper* than Claude — it does things Claude **cannot do at all** at any token price:

- run Whisper locally over the actual audio (works with no captions, and on any local file)
- the hybrid merge — Whisper's clean prose corrected against YouTube's proper-noun spellings
- hallucination flagging (`.hybrid.flagged.txt`)
- metadata sidecars (`.info.json`, `.description`) that make the proper-noun detection sharper
- deterministic, reproducible output written to disk instead of living in a context window

---

## 6. What needs to be done

Ordered by dependency, not by ambition. Each phase is independently valuable.

### P0 — Stop the bleeding — ✅ DONE 2026-08-05

1. ✅ **Resolved the git divergence** per §2. Stashed the stale draft as
   `stash@{0}` (deliberately scoped to the six stale paths so this report wasn't swept in),
   then `pull --ff-only`. `main` is now at `e0d9226`. Verified the merged work landed:
   `hybrid.rs` 1,018 lines, `lib.rs` 1,665, `main.rs` 22.
   *The stash is a rescue copy — drop it once you're satisfied nothing was lost.*
2. ✅ **Updated `yt-dlp`** `2025.04.30` → `2026.07.04`, plus `charset_normalizer`.
   Caption retrieval verified working; hybrid mode unblocked. See §4.
3. ✅ **`.gitignore`d `.claude/worktrees/`.**

### P1 — Rosetta compliance (hours, mostly unattended compiles)

4. **Install the arm64 Rust toolchain.** This is the root cause; everything else follows.
   ```bash
   rustup toolchain install stable-aarch64-apple-darwin
   rustup default stable-aarch64-apple-darwin
   rustup target add aarch64-apple-darwin x86_64-apple-darwin
   ```
5. **Install arm64 `ffmpeg`:** `/opt/homebrew/bin/brew install ffmpeg`. No code change
   needed — `path_entries()` already prefers `/opt/homebrew/bin`.
6. **Rebuild `whisper.cpp` as arm64 with Metal.** The performance win, and the reason to do
   this even without a deadline.
7. **Build Universal** so the bundle reports `Application (Universal)` in Get Info:
   `npm run tauri build -- --target universal-apple-darwin`.
8. Verify with `lipo -archs` on every binary, and with Finder Get Info on the `.app`.

### P1.5 — Make the app *enforce* compliance (the actual code change)

This is what "update the app to comply" means beyond recompiling. Right now
`check_environment()` (`lib.rs:905-928`) reports only `{name, found, path}`. It should also
report **architecture**, and warn when a dependency is Intel-only.

That turns the Apple article's manual "Get Info → Kind:" check into something the app does
for itself, on the machines that matter — and it's honest UX: a user on macOS 28 with an
Intel `ffmpeg` currently gets a confusing mid-job failure instead of a clear warning up front.

Concretely: extend `DependencyStatus` with an `arch` field, populate it by reading the
Mach-O header (or shelling to `lipo -archs`), and surface a banner in the UI when anything
comes back `x86_64`-only. Small change, high value, directly on-brief.

### P2 — The MCP server

Design recommendation, with reasoning, in §7.

---

## 7. MCP design — recommendation

### Topology: add a second binary to the existing Rust crate

Not a separate Node/Python server. The Rust code in `lib.rs` + `hybrid.rs` **is the asset** —
1,018 lines of merge engine with 13 passing tests. Reimplementing it in a wrapper language
guarantees the two copies drift.

`Cargo.toml` already declares `crate-type = ["staticlib", "cdylib", "rlib"]`, so the
pipeline is importable as a library today. Add `src-tauri/src/bin/mcp.rs`, speak MCP over
stdio, call the same functions. One language, one implementation, one arm64 binary, and the
GUI and the MCP can never disagree about what a hybrid transcript is.

**The one real refactor this requires:** the pipeline functions currently take
`&AppHandle` in order to call `emit()` for the UI log pane (`run_youtube_job`,
`download_captions`, `run_command`, …). That's a Tauri type, unavailable to a headless
binary. It needs to come out from behind a small trait — something like a
`ProgressSink` with one `emit(level, stage, message)` method — implemented twice: once
forwarding to `AppHandle::emit`, once writing MCP progress notifications (or `/dev/null`).
Mechanical, but it touches most function signatures in `lib.rs`, so it should be its own
commit.

### Tool surface

| Tool | Purpose |
|---|---|
| `youtube_metadata(url)` | Title, channel, duration, description, caption inventory. Fast, no download. The cheap "what is this" call. |
| `youtube_transcript(url, mode)` | The primary tool. `mode` ∈ `captions` / `whisper` / `hybrid`. Returns clean prose. |
| `transcribe_local(path, …)` | Local audio/video file → transcript. |
| `hybrid_merge(whisper_srt, youtube_srt, info_json?)` | Exposes the existing `run_hybrid_merge_job`. |

### The one design decision that actually matters

**How results come back.** For a 3-hour video, returning the full transcript inline defeats
the entire purpose — you'd have moved the tokens, not saved them.

My recommendation: **return a compact envelope, not the payload.** Metadata, a short
summary, token/word counts, and the *paths* to the written files. Claude then uses `Read`
(which supports `offset`/`limit`) to pull only the parts it needs. Long transcripts stay on
disk where they belong; short ones can be inlined under a configurable threshold.

### Making Claude actually reach for it

Worth naming explicitly, because it's the part that most often gets missed: **an MCP server
existing does not change Claude's reflex.** Given a YouTube URL, the measured behavior in
§5 is that Claude reaches for `WebFetch` 85% of the time. Fixing that needs one of:

- a rule in `~/.claude/CLAUDE.md` — "given a YouTube URL, always use the transcribe MCP,
  never `WebFetch`" — which is the blunt, reliable option; or
- a **skill** whose description triggers on YouTube URLs, which is the better-behaved
  option and composes with the rest of your setup.

I'd do both: the skill for good behavior, the CLAUDE.md line as a backstop.

---

## 8. Decisions on record

**Danny confirms (2026-08-05):** the app is *"just me on this mac, for now."* Two future
intentions, both explicitly deferred until the current changes land:

1. **An iPhone version, personal use only.**
2. **A GitHub release** so others can download it.

Sequencing: *"do what you recommend"* — P0 first, then reassess.
Stale draft: stash, then pull.

### What those future intentions imply (recorded now, not acted on)

**The GitHub release** retroactively makes the Universal build the right call rather than an
optional one. Someone downloading a release in 2027 may be on either architecture, and per
§3 the `.app` must read `Application (Universal)` in Get Info. It also promotes bundling and
notarization from "nice" to "required" — an unsigned `.app` from the internet is blocked by
Gatekeeper by default, and a downloader will not have `~/whisper.cpp` or a 1.5 GB model on
their disk. That's a real project, not a flag.

**The iPhone version is the harder constraint, and it is worth knowing early.** Tauri v2 does
support iOS targets, so the *shell* ports. The pipeline does not. This app's entire design is
"shell out to three external executables," and iOS permits no such thing — no `yt-dlp`, no
`ffmpeg` binary, no spawning subprocesses at all. A real iOS version would need:

- transcription via on-device Apple Speech / a Core ML Whisper build, or a call out to the Mac;
- caption fetching reimplemented in-process rather than by invoking `yt-dlp`;
- and no `ffmpeg` — AVFoundation instead.

The pragmatic path, and the one I'd recommend when you get there: **don't port the pipeline —
make the phone a client.** The Mac keeps doing the work; the iPhone app (or even a Shortcut)
hands it a URL and reads back the result. Which is, notably, the *same* architecture as the
MCP server in §7 — both are thin clients over a headless local pipeline. If the `ProgressSink`
refactor is done properly, the MCP server and the future iOS backend are the same work.

### Still open

- **Bundle dependencies, or keep discovering them on `PATH`?** Deferred with the GitHub
  release. Current discover-on-`PATH` is right for a single-user Mac; bundling becomes
  necessary the moment a stranger downloads it, and complicates `yt-dlp` updates (§4 shows
  how badly this app degrades when `yt-dlp` goes stale — bundling would have hidden that
  failure behind a release cycle).
