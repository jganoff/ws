# wsp

Multi-repo workspace manager. Create isolated, branch-per-feature workspaces
across multiple repositories in seconds.

## Why wsp?

Working across multiple repos on one feature usually means manually cloning,
branching, and keeping track of which repos are on which branch. `wsp` handles
all of that:

- **Instant** — local clones from bare mirrors via hardlinks, no network
- **Isolated** — every workspace is a set of fully independent git clones
- **One command** — create a workspace with consistent branches across repos
- **Safe cleanup** — detects uncommitted work, unmerged and squash-merged branches
- **Templates** — save and share workspace definitions for quick team onboarding
- **AI-agent aware** — `--json` on every command, auto-generated `AGENTS.md` in each workspace

## Quick start

### Install

```
brew install jganoff/tap/wsp
```

Or download a binary from the [latest release](https://github.com/jganoff/wsp/releases/latest), or build from source:

```
cargo install --git https://github.com/jganoff/wsp.git
```

### Shell integration

Add to your shell rc file:

```bash
# zsh (~/.zshrc)
eval "$(wsp completion zsh)"

# bash (~/.bashrc)
eval "$(wsp completion bash)"

# fish (~/.config/fish/config.fish)
wsp completion fish | source
```

This gives you tab completion and auto-cd into workspaces after `wsp new`.

### Register repos

Register your repos once. This creates a bare mirror for fast cloning:

```
$ wsp registry add git@github.com:acme/api-gateway.git
$ wsp registry add git@github.com:acme/user-service.git
```

### Create a workspace

```
$ wsp new add-billing api-gateway user-service
Creating workspace "add-billing" with 2 repos...
Workspace created: ~/dev/workspaces/add-billing
```

Every repo gets a local clone on the `add-billing` branch.

## Day-to-day

```
$ wsp cd add-billing          # jump into the workspace
$ wsp st                      # status across all repos
$ wsp diff                    # diff across all repos
$ wsp exec add-billing -- make test   # run a command in every repo
$ wsp rm add-billing          # clean up when done
```

Status shows branches, commits ahead, and changed files at a glance:

```
$ wsp st
Workspace: add-billing  Branch: add-billing

Repository    Branch        Status
api-gateway   add-billing   1 ahead, 2 files changed
user-service  add-billing   clean
```

`wsp rm` is safe by default — it blocks if any repo has uncommitted work or
unmerged branches (including squash-merged PRs). Removed workspaces are
recoverable via `wsp recover`. Use `--force` to override safety checks.

## Commands

| Command | Description |
|---------|-------------|
| `wsp new <name> [repos...] [-t template]` | Create a workspace |
| `wsp rm [workspace] [-f]` | Remove a workspace (recoverable) |
| `wsp ls` | List workspaces |
| `wsp st [workspace]` | Git status across repos |
| `wsp diff [workspace] [-- args]` | Git diff across repos |
| `wsp log [workspace] [-- args]` | Git log across repos |
| `wsp sync [workspace] [--strategy merge]` | Fetch and rebase/merge all repos |
| `wsp exec <workspace> -- <cmd>` | Run a command in each repo |
| `wsp cd <workspace>` | Change directory into a workspace |
| `wsp recover [workspace]` | List or restore removed workspaces |
| `wsp rename <old> <new>` | Rename a workspace |
| `wsp repo add [repos...] [-t template]` | Add repos to current workspace |
| `wsp repo rm <repos...> [-f]` | Remove repos from current workspace |
| `wsp repo fetch [--all] [--prune]` | Fetch updates (parallel) |
| `wsp registry add/ls/rm` | Manage registered repositories |
| `wsp template new/ls/show/export/rm` | Manage workspace templates |
| `wsp template repo add/rm` | Add or remove repos in a template |
| `wsp template config set/get/unset` | Manage template config overrides |
| `wsp template agent-md set/unset` | Manage template AGENTS.md content |
| `wsp config ls/get/set/unset` | Manage configuration |
| `wsp completion zsh\|bash\|fish` | Shell integration |

All commands support `--json` for structured output.

See [docs/usage.md](docs/usage.md) for the full reference.

## Configuration

**Branch prefix** — prepend your name to all workspace branches:

```
$ wsp config set branch-prefix myname
# wsp new fix-billing → creates branch myname/fix-billing
```

**Templates** — save a set of repos for quick workspace creation:

```
$ wsp template new backend api-gateway user-service
$ wsp new fix-billing -t backend
```

Templates are shareable files — export with `wsp template export backend` and
import on another machine with `wsp template new backend -f backend.wsp.yaml`.

**Go workspaces** — `wsp` auto-generates `go.work` when it detects `go.mod`
files. Disable with `wsp config set language-integrations.go false`.

## How it works

```
~/.local/share/wsp/
  config.yaml
  templates/               saved workspace templates
  mirrors/
    github.com/acme/
      api-gateway.git/       bare mirror (one network clone)
      user-service.git/      bare mirror
  gc/                        deferred deletions (recoverable via wsp recover)

~/dev/workspaces/
  add-billing/
    .wsp.yaml                workspace metadata
    AGENTS.md                auto-generated agent context
    api-gateway/             local clone (branch: add-billing)
    user-service/            local clone (branch: add-billing)
```

Each repo is registered once as a bare mirror. Workspaces are directories of
local clones (via `git clone --local` hardlinks) that share a branch name.
Each clone has a single `origin` remote pointing to the real upstream URL.
Fetches route through the local mirror transparently — no mirror-specific
remotes are visible in the clone.

## Development

Requires [Rust](https://www.rust-lang.org/tools/install) (stable) and
[just](https://github.com/casey/just).

```
just          # check (fmt + clippy)
just build    # build release binary
just test     # run all tests
just ci       # full CI pipeline
just fix      # auto-fix formatting and lint
```

## License

[MIT](LICENSE)
