# Feature Roadmap

Prioritized feature plan for wsp, organized by shipping priority.

## P1 — Adoption

### Workspace Templates (remaining phases)

**Design doc:** [`docs/features/workspace-templates.md`](features/workspace-templates.md)

Phases 1-4 have shipped (CRUD, `-w`/`-f` source flags, export, file import, group migration, template config).

- [ ] Phase 5: Format unification — `.wsp.yaml` gains URLs, becomes the template format
- [ ] Phase 6: Agent context — workspace definition repos with CLAUDE.md + skills alongside `.wsp.yaml`

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
- [ ] Phase 5: P2 checks (orphaned mirrors, default branch tracking, gc disk usage)
- [ ] Phase 6: Detect interrupted operations via transaction journal (see below)
- [ ] wspignore diagnostics: show effective ignore patterns (global + per-workspace merged), detect stale global wspignore (missing defaults added after initial seed), offer to add them via `--fix`

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

### PR Awareness

**Complexity:** Small-Medium

Surface open PR status in workspace commands. Requires `gh` CLI (already an accepted dependency for `wsp repo add --from`).

- [ ] `wsp st` shows PR URL/status per repo (if `gh` is available)
- [ ] `wsp rm` warns "this workspace has N open PRs" before trashing
- [ ] `--json` output includes PR metadata
- [ ] Graceful degradation when `gh` is not installed

### Team Bootstrap

**Complexity:** Small-Medium

Higher-level orchestration on top of workspace templates. A team bootstrap file bundles multiple templates + global defaults for onboarding. Depends on workspace templates (P1) landing first. See deferred decisions in [`docs/features/workspace-templates.md`](features/workspace-templates.md).

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
- Suppress via `wsp config set hints false`
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
- [ ] `wsp config set whatsnew false` to silence

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

## P4 — Ideas (Needs Design)

Features that surfaced during analysis but need more thought before committing to a design.

### `wsp snapshot` / `wsp restore`

Capture and restore full workspace state (HEAD SHAs, dirty/staged state via `git stash create`) across all repos. Enables safe exploration — agents and humans can snapshot before risky operations and revert cleanly. Needs design around: what exactly is captured, how stashes interact with branches, storage format, expiration.

### Dependency Graph in AGENTS.md

Emit a `## Dependency Graph` section in generated AGENTS.md showing which repos depend on which. Could be derived from package manifests (`go.mod`, `package.json`, `Cargo.toml`) or explicit user annotations. Helps agents understand cross-repo impact before making changes.

### Per-Repo Build/Test/Lint Commands

Allow users to declare per-repo commands (`wsp repo configure api-server --test "make test"`), emit them in AGENTS.md, expose via `wsp repo info --json`. Gives agents and humans a single source of truth for how to build/test each repo. Needs design around: where to store, how to discover automatically vs require declaration, how to keep in sync.

## Design Principles

See [`docs/design-tenets.md`](design-tenets.md) for the authoritative list. Summary:

- Every command is **workspace-aware** (workspace vs. upstream branches)
- Daily ops are **top-level short commands** (`sync`, `log`)
- **Always support `--json`** for scripting and AI agents
- **Parallel by default** for reads, serial for writes
- **Prevent data loss by default** -- destructive operations use deferred cleanup; permanent deletion is opt-in
- **Surface hidden state** -- wrong-branch, detached HEAD, and other mismatches are surfaced, not hidden
- **Workspace as context** -- the workspace metadata (`.wsp.yaml`, AGENTS.md, generated workspace files) is a coordination primitive consumed by AI agents, IDEs, and build tools
- No new external dependencies unless justified (`gh` for import/PR awareness is the exception)
