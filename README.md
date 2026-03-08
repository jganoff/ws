# wsp

Multi-repo workspace manager. One command to create an isolated workspace
across multiple repositories, all on the same branch.

## Quick start

```
brew install jganoff/tap/wsp

wsp registry add git@github.com:acme/api-gateway.git
wsp registry add git@github.com:acme/user-service.git

wsp new add-billing api-gateway user-service
```

That's it. You now have `~/dev/workspaces/add-billing/` with both repos cloned
on the `add-billing` branch. **Inside each repo, it's just git** — commit, push,
open PRs exactly as you normally would.

`registry add` is a one-time setup per repo — it creates a local mirror so
future clones are instant.

## Working in a workspace

```
wsp cd add-billing                    # jump into the workspace
wsp st                                # status across all repos
wsp diff                              # diff across all repos
wsp sync                              # fetch and rebase all repos
wsp exec add-billing -- make test     # run a command in every repo
wsp rm add-billing                    # clean up when done
```

Status at a glance:

```
$ wsp st
Workspace: add-billing  Branch: add-billing

Repository    Branch        Status
api-gateway   add-billing   1 ahead, 2 files changed
user-service  add-billing   clean
```

`wsp rm` is safe — it blocks if any repo has uncommitted work or unmerged
branches (including squash-merged PRs). Removed workspaces are recoverable
via `wsp recover`.

## Shell integration

For tab completion and auto-cd after `wsp new`, add to your shell rc:

```bash
# zsh
eval "$(wsp completion zsh)"

# bash
eval "$(wsp completion bash)"

# fish
wsp completion fish | source
```

## Templates

Save a set of repos so you don't have to remember them every time:

```
wsp template new backend api-gateway user-service
wsp new fix-billing -t backend
```

Share templates across machines:

```
wsp template export backend          # writes backend.wsp.yaml
wsp template new backend -f backend.wsp.yaml   # import on another machine
```

## Configuration

Prepend your name to all workspace branches:

```
wsp config set branch-prefix myname
# wsp new fix-billing → creates branch myname/fix-billing
```

## Commands

**Workspace lifecycle:**

| Command | Description |
|---------|-------------|
| `wsp new <name> [repos...] [-t template]` | Create a workspace |
| `wsp rm [workspace] [-f]` | Remove (recoverable by default) |
| `wsp ls` | List workspaces |
| `wsp cd <workspace>` | Jump into a workspace |
| `wsp recover [workspace]` | Restore a removed workspace |
| `wsp rename <old> <new>` | Rename a workspace |

**Daily workflow:**

| Command | Description |
|---------|-------------|
| `wsp st [workspace]` | Git status across repos |
| `wsp diff [workspace] [-- args]` | Git diff across repos |
| `wsp log [workspace] [-- args]` | Git log across repos |
| `wsp sync [workspace]` | Fetch and rebase all repos |
| `wsp exec <workspace> -- <cmd>` | Run a command in each repo |

**Repo and admin:**

| Command | Description |
|---------|-------------|
| `wsp repo add/rm/ls/fetch` | Manage repos in current workspace |
| `wsp registry add/ls/rm` | Manage registered repositories |
| `wsp template new/ls/show/rm/export` | Manage workspace templates |
| `wsp config ls/get/set/unset` | Manage settings |

All commands support `--json` for scripting and AI agents.
See [docs/usage.md](docs/usage.md) for the full reference.

## How it works

```
~/.local/share/wsp/
  mirrors/
    github.com/acme/
      api-gateway.git/       bare mirror (one network clone)
      user-service.git/      bare mirror

~/dev/workspaces/
  add-billing/
    .wsp.yaml                workspace metadata
    api-gateway/             local clone (branch: add-billing)
    user-service/            local clone (branch: add-billing)
```

Each repo is registered once as a bare mirror. Workspaces are directories of
local clones (via `git clone --local` hardlinks) that share a branch name.
Each clone has a single `origin` remote pointing to the real upstream — no
wsp-specific remotes or config leak into your repos.

## Other install methods

<details>
<summary>Binary download or build from source</summary>

Download a binary from the [latest release](https://github.com/jganoff/wsp/releases/latest), or build from source:

```
cargo install --git https://github.com/jganoff/wsp.git
```
</details>

## Development

Requires [Rust](https://www.rust-lang.org/tools/install) (stable) and
[just](https://github.com/casey/just).

```
just          # check (fmt + clippy)
just build    # build release binary
just test     # run all tests
just ci       # full CI pipeline
```

## License

[MIT](LICENSE)
