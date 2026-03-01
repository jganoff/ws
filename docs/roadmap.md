# Feature Roadmap

Prioritized feature plan for wsp, organized by shipping priority.

## P0 — Differentiation

### AGENTS.md Generation

**Complexity:** Medium | [Feature spec](features/agent-md.md)

Generate `AGENTS.md` (with `CLAUDE.md` symlink) at the workspace root so AI agents have context about repos, branches, and available `wsp` commands. Uses marked sections (`<!-- wsp:begin/end -->`) to preserve user-written content across updates.

- [ ] New `src/agentmd.rs` module with marked-section parser
- [ ] `CLAUDE.md` symlink management
- [ ] Config key `agent-md` (default on)
- [ ] Call from `new`, `repo add`, `repo rm`
- [ ] Table-driven tests for marker parsing and FS integration


## P1 — Adoption

### `wsp import`

**Complexity:** Medium

Auto-discover and register repos from a GitHub/GitLab org. Eliminates the biggest onboarding wall for teams with many repos.

```
$ wsp import github.com/acme --pattern "api-*,user-*"
Registered 5 repos.

$ wsp import github.com/acme --all
```

- [ ] `gh api` integration to list org repos
- [ ] `--pattern` glob filtering
- [ ] `--all` flag
- [ ] Interactive picker (nice-to-have)
- [ ] GitLab support (later)

### Git Subprocess Timeouts

**Complexity:** Small

Add a configurable timeout to `git::run()`. A hung SSH connection currently blocks the process forever. Parallel operations (`wsp sync`, `wsp repo fetch`) are also blocked when one repo hangs.

- [ ] Timeout parameter on `git::run()` (default 120s for network ops, no timeout for local)
- [ ] Config key `git-timeout` for user override
- [ ] Clear error message on timeout ("git fetch timed out after 120s")

## P2 — Team Adoption & Reliability

### `wsp export` / `wsp new --from`

**Complexity:** Small-Medium

Shareable workspace templates for reproducible workspace creation. Supports both file-based and URL-based sharing. The primary team-adoption vector: lets a developer send a colleague a one-liner or template file instead of a setup guide.

```
$ wsp export add-billing
wsp new add-billing api-gateway user-service@main proto@v1.0

$ wsp export add-billing --file
Wrote add-billing.wsp-template.yaml

$ wsp new --from add-billing.wsp-template.yaml
$ wsp new --from https://gist.github.com/.../template.yaml
```

Template format:

```yaml
name: add-billing
repos:
  - api-gateway
  - user-service@main
  - proto@v1.0
```

- [ ] `wsp export <name>` (prints `wsp new` one-liner)
- [ ] `wsp export <name> --file` (writes `.wsp-template.yaml`)
- [ ] `wsp new --from <file>` reads template
- [ ] `wsp new --from <url>` fetches and reads remote template
- [ ] Keep templates explicit (repo lists, not group references)

### `wsp init`

**Complexity:** Small

First-time setup wizard and/or adopt-existing-directory flow. Could walk through initial config (workspaces dir, add repos) or retroactively adopt an existing directory of clones as a wsp workspace.

- [ ] First-time interactive setup
- [ ] Adopt existing directory as workspace
- [ ] Detect already-cloned repos and register them

### `wsp doctor`

**Complexity:** Medium

Diagnostic and recovery command. Detects and optionally fixes common state problems.

- [ ] Detect orphaned clone directories (on disk but not in metadata)
- [ ] Detect metadata referencing missing directories
- [ ] Detect missing mirrors for workspace repos
- [ ] Detect stale `wsp-mirror` remotes
- [ ] `--fix` flag to auto-repair what it can
- [ ] Report disk usage for mirrors and workspaces

### `.code-workspace` Generation

**Complexity:** Small

Auto-generate VS Code multi-root workspace files when creating/modifying workspaces. Same architecture as the `go.work` language integration.

- [ ] New language integration implementing `LanguageIntegration` trait
- [ ] Detect VS Code presence or always generate (config toggle)
- [ ] Generate `<workspace>.code-workspace` with all repo directories
- [ ] Config key `language-integrations.vscode` (default true)

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

## Design Principles

- Every command is **workspace-aware** (active vs. context repos, workspace vs. upstream branches)
- Daily ops are **top-level short commands** (`sync`, `log`)
- **Always support `--json`** for scripting and AI agents
- **Parallel by default** for reads, serial for writes
- **Workspace as context** -- the workspace definition (`.wsp.yaml`, AGENTS.md, generated workspace files) is a coordination primitive consumed by AI agents, IDEs, and build tools
- No new external dependencies unless justified (`gh` for import is the exception)
