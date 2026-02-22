# Feature Roadmap

Prioritized feature plan for wsp, organized by shipping priority.

## P0 — Daily Workflow

### Hint System

**Complexity:** Small-Medium

Contextual and random tips to help users discover wsp features. Two hint types:

**Contextual hints** — triggered by command output state:

```
$ wsp st
api-gateway   feature  +2 ahead  1 changed
user-service  feature  (clean)

Tip: run `wsp push` to push all repos with unpushed commits
```

Examples:
- `wsp st` shows repos ahead → suggest `wsp push`
- `wsp st` shows repos behind → suggest `wsp sync`
- `wsp new` completes → suggest `wsp st` or `wsp sync`
- `wsp push` completes → suggest `wsp pr` (once implemented)

**Random hints** — shown occasionally (~20% of runs) when no contextual hint fires:

```
$ wsp ls
my-feature   3 repos  jganoff/my-feature

Tip: `wsp sync --strategy merge` uses merge instead of rebase
```

**Registration API** — adding a hint should be a one-liner:

```rust
// Contextual: fires when a condition on the Output is true
hints::contextual(
    |out| match out {
        Output::Status(s) => s.repos.iter().any(|r| r.ahead > 0),
        _ => false,
    },
    "run `wsp push` to push all repos with unpushed commits",
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
- [ ] Seed initial contextual hints (st→push, st→sync, new→sync)
- [ ] Seed initial random hint pool (~10 tips)
- [ ] Tests for hint selection and suppression logic

## P1 — High Value

### AGENTS.md Generation

**Complexity:** Medium | [Feature spec](features/agent-md.md)

Generate `AGENTS.md` (with `CLAUDE.md` symlink) at the workspace root so AI agents have context about repos, branches, and available `wsp` commands. Uses marked sections (`<!-- wsp:begin/end -->`) to preserve user-written content across updates.

- [ ] New `src/agentmd.rs` module with marked-section parser
- [ ] `CLAUDE.md` symlink management
- [ ] Config key `agent-md` (default on)
- [ ] Call from `new`, `repo add`, `repo rm`
- [ ] Table-driven tests for marker parsing and FS integration

### `wsp pr`

**Complexity:** Medium-Large

Open PRs across all active repos via `gh`, with cross-repo linking.

```
$ wsp pr --link
api-gateway    https://github.com/acme/api-gateway/pull/42      created
user-service   (no commits ahead)                                 skipped

$ wsp pr --title "Add billing" --draft --link
```

With `--link`, each PR body includes:

```
## Related PRs (wsp workspace: add-billing)
- acme/api-gateway#42
- acme/user-service#43
```

- [ ] Detect repos with commits ahead (reuse push logic)
- [ ] Shell out to `gh pr create`
- [ ] `--title`, `--body`, `--draft` flags
- [ ] `--link` cross-referencing (create PRs, then update bodies with links)
- [ ] `--json` output

## P2 — Team Adoption

### `wsp init`

**Complexity:** Small

First-time setup wizard and/or adopt-existing-directory flow. Could walk through initial config (workspaces dir, add repos) or retroactively adopt an existing directory of clones as a wsp workspace.

- [ ] First-time interactive setup
- [ ] Adopt existing directory as workspace
- [ ] Detect already-cloned repos and register them

### `wsp export` / `wsp new --from`

**Complexity:** Small-Medium

Shareable workspace templates for reproducible workspace creation. Supports both file-based and URL-based sharing.

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

## P3 — Later

### `wsp whatsnew` / Tips

**Complexity:** Small

Post-upgrade changelog highlights and contextual tips during normal usage.

- [ ] `wsp whatsnew` subcommand showing recent changes
- [ ] Contextual hints (e.g., suggest installing Claude Code hooks if detected)
- [ ] `wsp setup config set tips false` to silence
- [ ] `wsp setup config set whatsnew false` to silence

### `wsp cd` Sandbox

**Complexity:** Medium

`wsp cd` spawns a subshell with workspace env vars set. Exiting returns you to where you were. Gives workspaces a "sandbox" feel with isolated environment context.

- [ ] Subshell with `WSP_*` env vars
- [ ] Shell prompt integration showing active workspace
- [ ] `exit` returns to original directory

### `wsp import`

**Complexity:** Medium

Auto-discover and register repos from a GitHub/GitLab org.

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

## Design Principles

- Every command is **workspace-aware** (active vs. context repos, workspace vs. upstream branches)
- Daily ops are **top-level short commands** (`sync`, `push`, `log`, `pr`)
- **Always support `--json`** for scripting and AI agents
- **Parallel by default** for reads, serial for writes
- No new external dependencies unless justified (`gh` for PR ops is the exception)
