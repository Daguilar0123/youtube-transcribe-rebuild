# youtube-transcribe-rebuild — project rules (2026-08-06)

Tauri v2 (Rust) + Vite/TS app wrapping yt-dlp + ffmpeg + whisper.cpp.
Active plan: `docs/2026-08-05-implementation-plan.yaml` — the execution source of
truth. Phase gate: `Workflow({name:"verify-implementation", args:{phase:"…"}})`.

## Branch + worktree protocol — worktrees by FEATURE, not by agent

- `main` has exactly ONE writer: the current main-writer session. Nobody else
  commits or merges to main.
- All repo-code feature work happens in a feature worktree:
  `git worktree add .worktrees/<slug> -b feature/<slug>`
  (`.worktrees/` is gitignored — verify with `git check-ignore -q .worktrees`.)
- Do NOT use `.claude/worktrees/` for interactive feature work: worktrees nested
  under `.claude/` break slash-command and skill discovery
  (anthropics/claude-code#48967, closed not-planned). Enter a feature worktree
  with `EnterWorktree {path: ".worktrees/<slug>"}` (approve the one-time prompt)
  or by starting a session in that directory.
- Environment-only work (rustup, brew, whisper.cpp — plan phase P1) touches no
  repo files and needs no worktree.
- Fresh-worktree checklist before any work: `npm ci`, then baseline
  `cargo test` + `npx tsc --noEmit`. Expect cold builds: `target/` and
  `node_modules/` are per-worktree (optional: shared CARGO_TARGET_DIR trades
  cold builds for build-lock contention — default is per-worktree).
- Merge-back (main-writer only), tests run ON THE MERGED RESULT before cleanup:
  `git checkout main && git merge --no-ff feature/<slug>` → `cargo test` +
  `npx tsc --noEmit` → push → phase gate → only then
  `git worktree remove .worktrees/<slug> && git branch -d feature/<slug>`.
- `git branch -D` (force) only on Danny's explicit instruction.
- One feature per session. A session that shipped a feature retires; it does
  not start the next one.

## Context lifecycle — compact once, then retire

- Auto-compact is DISABLED for this project (`.claude/settings.json`).
  Compaction is manual and happens AT MOST ONCE per session.
- At ~60% context (Danny watches the statusline; treat his "60" or any harness
  context-pressure warning as the signal): finish the current plan step, then
  run `/compact` WITH focus instructions naming the active step id, modified
  files, and test commands.
- After that single compaction: continue to the next natural boundary (feature
  merged, phase gate green), then RETIRE — update the handoff, name the session
  (`/rename <feature-slug>`), stop.
- NEVER compact twice. If context pressure returns after the one compaction,
  retire instead — even mid-feature (the incremental handoff makes this safe).
- Returning to a retired session: `claude --resume <name-or-id>` restores the
  FULL transcript. Early retirement is what keeps that transcript lean. If the
  resume dialog offers "Resume from summary", decline it — pick full session.

## Handoffs — JSON, machine-first, written continuously

- Any document intended for another agent is JSON/JSONL, never prose. Markdown
  is for humans.
- One handoff file per session: `docs/handoffs/<utc yyyymmddThhmmZ>_<slug>.handoff.json`.
  Never edit another session's handoff file.
- Write it INCREMENTALLY at every milestone during the session — not only at
  retirement. A session that dies mid-step must still leave a usable handoff
  (context exhaustion mid-implementation is the documented top failure mode).
- On retirement, append ONE line to `docs/handoffs/INDEX.jsonl` (append-only;
  a single shared handoff file is last-writer-wins under parallel sessions).
- Mark a step `"done"` ONLY when its verify commands passed — record the
  evidence. Premature "done" is the failure mode the ledger exists to prevent.
- Handoff schema (handoff/v1):

```json
{
  "schema": "handoff/v1",
  "session": {"id": "", "name": "", "started_utc": "", "retired_utc": null},
  "role": "main-writer | feature:<slug>",
  "git": {"branch": "", "worktree": null, "head": "", "pushed": false, "ci": "green|red|none"},
  "plan_ref": "docs/2026-08-05-implementation-plan.yaml",
  "steps": [{"id": "", "status": "done|in_progress|blocked|todo", "verified": false, "evidence": ""}],
  "in_flight": {"doing": "", "next_action": "", "blockers": []},
  "decisions": [{"what": "", "why": "", "date": ""}],
  "files_touched": [],
  "gotchas": [],
  "user_pending": [],
  "resume_hint": "claude --resume <session-name>"
}
```

- INDEX.jsonl line shape:
  `{"ts": "<utc>", "file": "docs/handoffs/<file>", "session": "<id-or-name>", "role": "", "summary": "<one line>"}`

## Compact Instructions

When compacting, always preserve: the active plan step id and its verify
commands; the full list of files modified this session; unresolved errors
verbatim; the handoff file path; pending user confirmations; and the rule that
this session may not compact again and must retire at the next boundary.

## Commit convention

Commit messages are memory entries for future agents — what changed, why, and
what was decided (Danny's global convention, restated because the handoff
system depends on it). Commit at decision points, not only at the end.
