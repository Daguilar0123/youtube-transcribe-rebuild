export const meta = {
  name: 'verify-implementation',
  description: 'Phase gate for docs/2026-08-05-implementation-plan.yaml: arch audit, build+tests, caption e2e, MCP protocol + security probes, docs/CI',
  whenToUse: 'After completing each plan phase. Invoke: Workflow({name:"verify-implementation", args:{phase:"P1"|"P1.5"|"P2"|"P3"|"all"}}). Do not advance phases while pass=false.',
  phases: [{ title: 'Verify' }],
}

const REPO = '/Users/danielaguilar/Developer/youtube-transcribe-rebuild'
const PHASE = (args && typeof args === 'object' && args.phase) ? String(args.phase) : 'all'
const want = (p) => PHASE === 'all' || PHASE === p

const SCHEMA = {
  type: 'object',
  properties: {
    pass: { type: 'boolean' },
    checks: {
      type: 'array',
      items: {
        type: 'object',
        properties: { name: { type: 'string' }, pass: { type: 'boolean' }, evidence: { type: 'string' } },
        required: ['name', 'pass', 'evidence'],
      },
    },
    problems: { type: 'array', items: { type: 'string' } },
  },
  required: ['pass', 'checks', 'problems'],
}

const COMMON = `Report via StructuredOutput: pass=true ONLY if every check passes. Each check needs literal command output as evidence — never assert without output. Repo: ${REPO}. Read-only for git state; you may run builds/tests. If a command is unavailable, that check FAILS (with the error as evidence) — do not skip silently.`

const thunks = []

if (want('P1')) {
  thunks.push(() => agent(`Native-architecture audit after plan phase P1 (docs/2026-08-05-implementation-plan.yaml). ${COMMON}
Checks (name each exactly):
1. rustup-shim-arm64: file -b "$(readlink -f ~/.cargo/bin/rustup)" contains arm64.
2. rustc-host-arm64: rustc -vV reports host: aarch64-apple-darwin AND 'rustup show' prints no emulation warning.
3. ffmpeg-arm64: lipo -archs /opt/homebrew/bin/ffmpeg == arm64.
4. ytdlp-converged: /opt/homebrew/bin/yt-dlp exists, --version >= 2026.07, AND it shadows /usr/local/bin/yt-dlp in the app's search order (app checks /opt/homebrew/bin first — verify the file exists there; that is sufficient).
5. cmake-arm64: file -b "$(readlink -f /opt/homebrew/bin/cmake)" contains arm64.
6. whisper-arm64: lipo -archs ~/whisper.cpp/build/bin/whisper-cli == arm64.
7. whisper-flags-stable: ~/whisper.cpp/build/bin/whisper-cli --help still offers -m, -f, -otxt, -osrt, -of, --max-len, --prompt (run_whisper's exact surface).
8. whisper-metal-live: run whisper-cli -m ~/whisper.cpp/models/ggml-medium.en.bin -f ~/whisper.cpp/samples/jfk.wav and grep the log for Metal/ggml_metal init lines. Whisper falls back to CPU SILENTLY — absence of Metal lines is a FAIL even if transcription succeeds.`,
    { label: 'arch-audit', phase: 'Verify', schema: SCHEMA }))

  thunks.push(() => agent(`Caption-path e2e using the EXACT binary the app resolves (its search order: /opt/homebrew/bin, then /usr/local/bin, then PATH — pick the first yt-dlp that exists in that order and SAY which one you used). ${COMMON}
In a scratch directory (mktemp -d), run the app's exact caption invocation:
<resolved-yt-dlp> --skip-download --write-subs --write-auto-subs --write-description --write-info-json --sub-langs "en.*" --sub-format "srt/vtt/best" --convert-subs srt -o "captions.%(ext)s" -- "https://www.youtube.com/watch?v=SK90kEdX_Is"
Checks: 1. srt-written: at least one non-empty .srt file produced. 2. sidecars-written: .description and .info.json both produced. 3. no-dependency-warnings: stderr free of RequestsDependencyWarning noise. Clean up the scratch dir.`,
    { label: 'caption-e2e', phase: 'Verify', schema: SCHEMA }))
}

if (want('P1.5')) {
  thunks.push(() => agent(`Arch self-reporting feature verification (plan step A1/A2). ${COMMON}
Checks:
1. macho-module-exists: src-tauri/src/macho.rs exists and defines detect_arch.
2. dependencystatus-arch: DependencyStatus in src-tauri/src/lib.rs has an arch field and check_environment populates it.
3. macho-tests-pass: cd src-tauri && cargo test macho 2>&1 — the macho unit tests exist and pass (must cover thin-arm64, thin-x86_64, fat-universal big-endian, java-class-lookalike->unknown, shebang resolution).
4. ui-banner-wired: src/main.ts (or index.html) renders the arch value and contains the x86_64 warning-banner logic.
5. bundle-arm64: lipo -archs on "src-tauri/target/release/bundle/macos/YouTube Transcribe Rebuild.app/Contents/MacOS/youtube-transcribe-rebuild" == arm64 (if the bundle has not been rebuilt yet, this check FAILS — that is the point of the gate).`,
    { label: 'arch-feature', phase: 'Verify', schema: SCHEMA }))
}

if (want('P2')) {
  thunks.push(() => agent(`MCP server protocol + security verification (plan steps M1–M5). ${COMMON}
Setup: locate the binary — prefer ~/.cargo/bin/yt-transcribe-mcp, else ${REPO}/src-tauri/target/release/yt-transcribe-mcp (build with: cd src-tauri && cargo build --release --features mcp --bin yt-transcribe-mcp if absent).
Drive it over stdio with a Python harness (newline-delimited JSON-RPC 2.0, one object per line; read stdout lines, ignore stderr):
1. initialize (protocolVersion "2026-07-28", capabilities {}, clientInfo) -> expect a result with serverInfo; then notifications/initialized.
2. tools/list -> expect exactly these tools: youtube_metadata, youtube_transcript, transcribe_local, hybrid_merge.
3. tools/call youtube_metadata {"url":"https://www.youtube.com/watch?v=SK90kEdX_Is"} -> result contains title and channel; result must NOT contain raw info.json bulk (fail if response > 20000 chars).
SECURITY PROBES — each must be REJECTED (isError/error result, and the server must not crash; verify it still answers a follow-up tools/list after each):
4. reject-dash-url: youtube_metadata url "-o/tmp/pwn".
5. reject-http: youtube_metadata url "http://www.youtube.com/watch?v=x".
6. reject-ssrf-host: youtube_metadata url "https://169.254.169.254/latest/meta-data".
7. reject-playlist: youtube_metadata url "https://www.youtube.com/playlist?list=PL123".
8. reject-outdir-escape: youtube_transcript {url:<valid>, mode:"captions", output_dir:"/tmp/escape-test"} -> must be rejected (outside ~/Documents/Transcripts).
9. reject-input-escape: transcribe_local {path:"/etc/hosts", mode:"transcribe_only"} -> must be rejected (outside $HOME).
Also check: 10. refactor-clean: grep AppHandle src-tauri/src/lib.rs — appears only in TauriSink + the three #[tauri::command] wrappers. 11. gui-intact: cd src-tauri && cargo test (all pre-existing tests still pass).`,
    { label: 'mcp-e2e', phase: 'Verify', schema: SCHEMA }))
}

if (want('P3')) {
  thunks.push(() => agent(`Docs + CI verification (plan steps D1–D2). ${COMMON}
Checks:
1. readme-current: README.md documents the arm64/Apple-silicon requirements, the MCP server, and a dated release-prerequisites section; spot-check that three commands it gives actually exist/work.
2. report-corrections-present: docs/2026-08-05-modernization-and-mcp-report.md contains the 'Post-verification corrections' block.
3. ci-green: gh run list --limit 3 shows the CI workflow concluded success on the latest main push (if no runs exist because nothing was pushed, FAIL with that evidence).
4. mcp-json-valid: .mcp.json at repo root parses as JSON and points at an existing binary path (expand \${HOME} manually when checking).`,
    { label: 'docs-ci', phase: 'Verify', schema: SCHEMA }))
}

// Always: core build health — every phase must keep this green.
thunks.push(() => agent(`Build-health gate (runs for every phase). ${COMMON}
Checks:
1. cargo-check: cd ${REPO}/src-tauri && cargo check — clean.
2. cargo-tests: cargo test — ALL tests pass; report the exact pass/fail counts (baseline was 13 before P1.5 added macho tests and P2 added guard tests — count must never go below the current committed test set).
3. tsc: cd ${REPO} && npx tsc --noEmit — clean.
4. git-clean: git -C ${REPO} status --porcelain — empty, or ONLY files the current plan step is expected to have touched (list them as evidence).
Use timeout: 600000 for the cargo commands; a first native-toolchain build recompiles everything.`,
  { label: 'build-health', phase: 'Verify', schema: SCHEMA }))

log(`verify-implementation: phase=${PHASE}, running ${thunks.length} check agents`)
const results = (await parallel(thunks)).filter(Boolean)
const pass = results.length === thunks.length && results.every((r) => r.pass)
const failed = results.flatMap((r) => r.checks.filter((c) => !c.pass).map((c) => c.name))
log(pass ? 'verify-implementation: ALL CHECKS PASS' : `verify-implementation: FAILING checks: ${failed.join(', ') || 'agent(s) died'}`)
return { phase: PHASE, pass, failed_checks: failed, agents: results }
