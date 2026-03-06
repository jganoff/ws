# Feature Roadmap

Prioritized feature plan for wsp, organized by shipping priority.

## P1 — Adoption

### Workspace Definitions

**Complexity:** Medium

**Design doc:** [`docs/features/workspace-definitions.md`](features/workspace-definitions.md)

Sharable, self-contained files that describe how to create a workspace — repos, context repos, and settings. Replaces groups with a richer, portable concept. One file gets a new teammate fully set up.

```yaml
# dash.wsp.yaml
repos:
  - url: https://github.com/docker/api-gateway.git
  - url: https://github.com/docker/user-service.git
context:
  - url: https://github.com/docker/proto.git
    ref: main
settings:
  language_integrations:
    go: true
  sync_strategy: rebase
```

```
wsp new my-feature -d dash              # create from definition
wsp setup def export dash               # share with teammate
wsp setup def import dash.wsp.yaml      # teammate imports + auto-registers repos
```

- [ ] Phase 1: Definition CRUD (`wsp setup def new/ls/show/rm`) and `wsp new -d`
- [ ] Phase 2: Import/export with auto-registration of unknown repos
- [ ] Phase 3: Migrate existing groups to definitions, deprecate `-g`
- [ ] Phase 4: Wire definition settings into workspace creation

### `wsp exec --json`

**Complexity:** Small

`wsp exec` is the only command without `--json` output. Add structured output to satisfy Agent Use tenet 3 ("Structured output is the contract").

```json
{
  "results": [
    { "repo": "api-gateway", "directory": "api-gateway", "exit_code": 0, "ok": true },
    { "repo": "user-service", "directory": "user-service", "exit_code": 1, "ok": false }
  ]
}
```

- [ ] New `ExecResult` / `ExecOutput` struct in `src/output.rs`
- [ ] `Output::Exec` variant with JSON rendering
- [ ] Capture stdout/stderr per repo when `--json` is set (don't interleave with JSON)

### `wsp new` Timing Output

**Complexity:** Small

Show elapsed time after workspace creation. Seeing "1.2s for 5 repos" is the word-of-mouth trigger that communicates the mirror-based speed advantage.

```
$ wsp new add-billing -g backend
Creating workspace "add-billing" (branch: jganoff/add-billing) with 5 repos...
Workspace created: ~/dev/workspaces/add-billing (1.2s)
```

- [ ] Wrap workspace creation in `Instant::now()` / `elapsed()`
- [ ] Print timing in human-readable output
- [ ] Include `duration_ms` in `--json` output

### Workspace Descriptions

**Complexity:** Small

Record the purpose of a workspace so `wsp ls` remains interpretable at scale. Stored in `.wsp.yaml`.

```
$ wsp new add-billing -g backend --description "migrating billing to stripe v3"
$ wsp ls
add-billing   3 repos  jganoff/add-billing  "migrating billing to stripe v3"
```

- [ ] `--description` flag on `wsp new`
- [ ] `description` field in `.wsp.yaml` metadata
- [ ] Show in `wsp ls` (human and `--json`)
- [ ] `wsp describe <workspace> <text>` to set/update after creation

### Enrich `wsp st` with Agent Context

**Complexity:** Small-Medium

Make `wsp st --json` the single-call entry point for AI agents. Include enough context that an agent doesn't need to call `wsp repo ls`, `wsp log`, etc. separately.

- [ ] Add `workspace_branch`, `workspace_dir` fields to status JSON root
- [ ] Add `behind` count per repo (commits behind default branch)
- [ ] Add `role` per repo (`active` vs `context`)
- [ ] Include `description` if set

### `wsp rename`

**Complexity:** Small-Medium

Rename a workspace without destroying it. Renames the directory, updates `.wsp.yaml`, renames the git branch in each active repo, and regenerates AGENTS.md.

```
$ wsp rename fix-typo refactor-auth
Renamed workspace "fix-typo" -> "refactor-auth"
  api-gateway    branch: fix-typo -> refactor-auth
  user-service   branch: fix-typo -> refactor-auth
```

- [ ] Rename workspace directory
- [ ] Update `.wsp.yaml` name field
- [ ] `git branch -m` in each active repo
- [ ] Skip context repos (pinned to ref, no workspace branch)
- [ ] Regenerate AGENTS.md/CLAUDE.md
- [ ] Update `.code-workspace` and `go.work` if they exist

### `wsp sync --abort`

**Complexity:** Small

Abort an in-progress rebase/merge across all repos. The recovery command for when `wsp sync` hits conflicts.

```
$ wsp sync --abort
  skip  api-gateway    (no rebase in progress)
  ok    user-service   rebase aborted
```

- [ ] Detect rebase/merge in progress per repo (`.git/rebase-merge`, `.git/MERGE_HEAD`)
- [ ] Run `git rebase --abort` or `git merge --abort` as appropriate
- [ ] Structured `--json` output

### `wsp doctor`

**Complexity:** Medium

**Design doc:** [`docs/features/doctor.md`](features/doctor.md)

Diagnostic command that checks workspace and global state for invariant violations and optionally auto-fixes them. Follows the `brew doctor`/`flutter doctor` pattern.

```
$ wsp doctor
Checking global state...
  ✓ config is valid
  ✓ 5 registered repos, 5 mirrors

Checking workspace my-feature...
  ✓ api-gateway: ok
  ⚠ bar: origin URL differs from registered URL
  ✓ utils: ok

1 warning. Run `wsp doctor --fix` to auto-fix.
```

- [ ] Phase 1: Command skeleton with P0 checks (config parseable, origin URL match, repo dirs exist)
- [ ] Phase 2: `--fix` for origin URL repoint
- [ ] Phase 3: `--json` output
- [ ] Phase 4: P1 checks (mirror exists, origin remote exists, identity matches, orphaned dirs)
- [ ] Phase 5: P2 checks (orphaned mirrors, default branch tracking, disk usage)
- [ ] Phase 6: Detect interrupted operations via transaction journal (see below)

### Transaction Journal

**Complexity:** Small-Medium

Record multi-repo operation progress so partial failures are visible and recoverable. Before `wsp sync` touches 5 repos, write a journal. As each completes, update its entry. If wsp crashes or is interrupted, the journal survives for `wsp doctor` to read.

```
# .wsp.yaml.journal (transient, deleted on clean completion)
operation: sync
started: 2026-03-04T10:00:00Z
repos:
  api-gateway: ok
  user-service: ok
  proto: failed (conflict in src/auth.rs)
  billing: pending
  payments: pending
```

- [ ] Write journal before multi-repo operations (`sync`, `rm`, `repo add`)
- [ ] Update per-repo status as operations complete
- [ ] Delete journal on clean completion
- [ ] `wsp doctor` reads stale journals and reports/retries

### Soft-Delete (`wsp rm` → Trash)

**Complexity:** Small-Medium

Default `wsp rm` to moving workspaces to trash instead of permanent deletion. Recoverable for a configurable period.

```
$ wsp rm add-billing
Workspace "add-billing" moved to trash (recoverable for 14 days)

$ wsp trash ls
add-billing   trashed 2026-03-04  expires 2026-03-18

$ wsp trash restore add-billing

$ wsp trash purge          # remove expired
$ wsp trash purge --all    # remove everything
```

- [ ] Trash directory: `~/.local/share/wsp/trash/`
- [ ] `wsp rm` moves to trash by default
- [ ] `wsp rm --permanent` for immediate deletion
- [ ] `wsp trash ls`, `wsp trash restore <name>`, `wsp trash purge`
- [ ] Config key `trash.retention-days` (default 14)
- [ ] `wsp doctor` reports trash disk usage

### PR Awareness

**Complexity:** Small-Medium

Surface open PR status in workspace commands. Requires `gh` CLI (already an accepted dependency for `wsp repo add --from`).

- [ ] `wsp st` shows PR URL/status per repo (if `gh` is available)
- [ ] `wsp rm` warns "this workspace has N open PRs" before trashing
- [ ] `--json` output includes PR metadata
- [ ] Graceful degradation when `gh` is not installed

### Team Bootstrap

**Complexity:** Small-Medium

Higher-level orchestration on top of workspace definitions. A team bootstrap file bundles multiple definitions + global defaults for onboarding. Depends on workspace definitions (P1) landing first. See deferred decisions in [`docs/features/workspace-definitions.md`](features/workspace-definitions.md).

### `wsp init`

**Complexity:** Small

First-time setup wizard and/or adopt-existing-directory flow. Should be a funnel that ends with a working workspace (reach the "aha moment" during setup, not after).

- [ ] First-time interactive setup
- [ ] Adopt existing directory as workspace
- [ ] Detect already-cloned repos and register them
- [ ] End with `wsp new` to create first workspace

### Lifecycle Hooks

**Complexity:** Small-Medium

Shell-script hooks that run at key points in the workspace lifecycle. Enables teams to run `npm install`, `docker-compose up`, or other setup after clone without forking wsp.

```
~/.local/share/wsp/hooks/
  post-create.sh    # runs after wsp new, receives workspace metadata as JSON on stdin
  post-remove.sh    # runs after wsp rm
  post-sync.sh      # runs after wsp sync
```

- [ ] Hook discovery in `~/.local/share/wsp/hooks/`
- [ ] Per-workspace hooks in `.wsp.yaml` (optional)
- [ ] Pass workspace metadata as JSON on stdin
- [ ] Timeout and error handling (hook failure = warning, not abort)
- [ ] Trust model: per-workspace hooks from `.wsp.yaml` require explicit `wsp hooks trust` with content hash verification
- [ ] No shell interpolation of workspace variables — pass as env vars (`WSP_WORKSPACE_NAME`, etc.)

## P2 — Agent & Ecosystem

### Staleness Signals in `wsp ls`

**Complexity:** Small

Enrich `wsp ls` with signals to identify stale workspaces at scale.

- [ ] `last_activity` — most recent commit timestamp across all repos
- [ ] `merged` — are all active branches merged into their default branch?
- [ ] `wsp ls --stale` filter (all branches merged)
- [ ] Include in `--json` output

### Cross-Repo Search (`wsp grep`)

**Complexity:** Small

Repo-tagged search across all workspace repos. Wraps ripgrep.

```
$ wsp grep "ValidateToken"
[shared-lib] src/auth.go:15: func ValidateToken(...
[api-server] handlers/auth.go:42: token, err := shared.ValidateToken(...
```

- [ ] Wrap `rg` across all workspace repo directories
- [ ] Tag each match with repo name
- [ ] `--json` output with repo/file/line/text per match
- [ ] Passthrough common rg flags (`-i`, `-w`, `--type`)

## P3 — Polish

### Hint System

**Complexity:** Small-Medium

Contextual and random tips to help users discover wsp features. Two hint types:

**Contextual hints** -- triggered by command output state:

```
$ wsp st
api-gateway   feature  +2 ahead  1 changed
user-service  feature  (clean)

Tip: run `wsp sync` to fetch and rebase all repos
```

Examples:
- `wsp st` shows repos behind -> suggest `wsp sync`
- `wsp new` completes -> suggest `wsp st` or `wsp sync`

**Random hints** -- shown occasionally (~20% of runs) when no contextual hint fires:

```
$ wsp ls
my-feature   3 repos  jganoff/my-feature

Tip: `wsp sync --strategy merge` uses merge instead of rebase
```

**Registration API** -- adding a hint should be a one-liner:

```rust
// Contextual: fires when a condition on the Output is true
hints::contextual(
    |out| match out {
        Output::Status(s) => s.repos.iter().any(|r| r.behind > 0),
        _ => false,
    },
    "run `wsp sync` to fetch and rebase all repos",
);

// Random: just a static string
hints::random("use `wsp log --oneline` for a flat view across all repos");
```

All hints registered in one place (`src/hints.rs`) via a `pub fn all() -> HintRegistry` builder. Adding a new hint = one function call, no boilerplate.

**Rules:**
- Print to stderr (never pollute `--json` output)
- Suppress when `--json` flag is set
- Suppress via `wsp setup config set hints false`
- One hint per invocation max (contextual takes priority over random)

- [ ] New `src/hints.rs` with `HintRegistry` builder and `contextual()`/`random()` API
- [ ] Wire into `main.rs` after `output::render()`
- [ ] Config key `hints` (default true)
- [ ] Seed initial contextual hints (st->sync, new->sync)
- [ ] Seed initial random hint pool (~10 tips)
- [ ] Tests for hint selection and suppression logic

### `wsp whatsnew`

**Complexity:** Small

Post-upgrade changelog highlights during normal usage.

- [ ] `wsp whatsnew` subcommand showing recent changes
- [ ] Contextual hints (e.g., suggest installing Claude Code hooks if detected)
- [ ] `wsp setup config set whatsnew false` to silence

### MCP Server

**Complexity:** Medium-Large

Model Context Protocol server so AI agents can manage workspaces natively through structured tool calls rather than shelling out and parsing JSON. Upgrades wsp from "tool agents can use" to "tool agents integrate with."

- [ ] MCP server binary or mode (`wsp mcp` or separate `wsp-mcp`)
- [ ] Tools: list workspaces, create/remove workspace, status, sync, repo add/remove
- [ ] Resources: workspace metadata, repo status
- [ ] Stdio transport (standard for local MCP servers)

### `.code-workspace` Generation

**Complexity:** Small

Auto-generate VS Code multi-root workspace files when creating/modifying workspaces. Same architecture as the `go.work` language integration.

- [ ] New language integration implementing `LanguageIntegration` trait
- [ ] Detect VS Code presence or always generate (config toggle)
- [ ] Generate `<workspace>.code-workspace` with all repo directories
- [ ] Config key `language-integrations.vscode` (default true)

### `.wspignore`

**Complexity:** Small

Suppress workspace root safety check warnings for specific paths. Two layers: global (`~/.local/share/wsp/wspignore`) and per-workspace (`.wspignore` at workspace root). Simple format: one path per line, `#` comments, trailing `/` for directory prefix match. No globs.

- [ ] Per-workspace `.wspignore` file
- [ ] Global `~/.local/share/wsp/wspignore` file
- [ ] Integrate with `check_root_content()` in `src/workspace.rs`

## P4 — Ideas (Needs Design)

Features that surfaced during analysis but need more thought before committing to a design.

### `wsp snapshot` / `wsp restore`

Capture and restore full workspace state (HEAD SHAs, dirty/staged state via `git stash create`) across all repos. Enables safe exploration — agents and humans can snapshot before risky operations and revert cleanly. Needs design around: what exactly is captured, how stashes interact with branches, storage format, expiration.

### Dependency Graph in AGENTS.md

Emit a `## Dependency Graph` section in generated AGENTS.md showing which repos depend on which. Could be derived from package manifests (`go.mod`, `package.json`, `Cargo.toml`) or explicit user annotations. Helps agents understand cross-repo impact before making changes.

### Per-Repo Build/Test/Lint Commands

Allow users to declare per-repo commands (`wsp repo configure api-server --test "make test"`), emit them in AGENTS.md, expose via `wsp repo info --json`. Gives agents and humans a single source of truth for how to build/test each repo. Needs design around: where to store, how to discover automatically vs require declaration, how to keep in sync.

## Design Principles

- Every command is **workspace-aware** (active vs. context repos, workspace vs. upstream branches)
- Daily ops are **top-level short commands** (`sync`, `log`)
- **Always support `--json`** for scripting and AI agents
- **Parallel by default** for reads, serial for writes
- **Workspace as context** -- the workspace definition (`.wsp.yaml`, AGENTS.md, generated workspace files) is a coordination primitive consumed by AI agents, IDEs, and build tools
- No new external dependencies unless justified (`gh` for import/PR awareness is the exception)
