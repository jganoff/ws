# Feature: CLI Restructure

Flatten the `wsp setup` umbrella into separate top-level nouns, normalize verb naming, and use Clap help headings for visual grouping.

## Motivation

The current `wsp setup` namespace bundles unrelated concerns (repo registry, groups/templates, key-value settings, shell completions) under one umbrella because they are "not daily use." This creates problems:

1. **No major CLI uses this pattern.** A survey of 18 popular CLIs (git, Docker, gh, kubectl, gcloud, cargo, npm, brew, mise, terraform, fly, heroku, asdf, etc.) found zero that use `setup` or a similar umbrella for heterogeneous admin commands.
2. **`setup` implies a one-time wizard** (like `aws configure`), not an ongoing management surface.
3. **Extra nesting for no semantic gain.** `wsp setup repo add` is 4 tokens deep. The word "setup" adds indirection without clarity.
4. **Verb inconsistency.** `delete` vs `rm`/`remove`, missing `ls` aliases, primary/alias direction varies by subcommand.

## Industry patterns

### Dominant: separate top-level nouns per concern

Used by gh, gcloud, mise, asdf, Docker, flyctl, Heroku, kubectl.

```
gh config list/get/set              # key-value settings
gh auth login/logout/status         # auth management
gh extension install/list/remove    # plugin resources
```

Each resource or concern gets its own top-level noun. `config` universally means key-value settings. Registrable resources (plugins, extensions, remotes, components) get dedicated nouns.

### Secondary: completely flat

Used by git, brew, npm, cargo, terraform. Works when total command count is under ~20.

### Rare: umbrella namespace

Only Docker `system` comes close, and it is specifically for daemon-level operations, not a catch-all for admin commands.

## Proposed structure

### Before (current)

```
wsp setup repo add/list/remove           # global repo registry
wsp setup group new/list/show/delete      # groups (becoming templates)
wsp setup config list/get/set/unset       # key-value settings
wsp setup completion zsh|bash|fish        # shell completions
wsp setup skill install/generate          # AI skills (to be removed)
```

### After

```
wsp registry add/ls/rm                   # global repo registry (mirrors)
wsp template new/ls/show/edit/rm              # workspace templates
wsp template import/export                    # sharing templates
wsp config ls/get/set/unset              # key-value settings
wsp completion zsh|bash|fish             # shell completions
```

Daily workflow commands are unchanged:

```
wsp new/rm/ls/st/diff/log/sync/exec/cd
wsp repo add/rm/ls/fetch
```

### Verb normalization

Every destructive command uses the same pattern: primary `rm`, visible alias `remove`.

Every list command uses the same pattern: primary `ls`, visible alias `list`.

| Verb | Primary | Visible alias | Hidden alias |
|------|---------|---------------|--------------|
| Remove/delete | `rm` | `remove` | `delete` (for group migration) |
| List | `ls` | `list` | |
| Create | `new` | | |
| Show details | `show` | | |
| Modify | `edit` / `update` | | |

### Help output grouping

Use Clap's `help_heading` to visually separate daily and admin commands in `--help` without nesting:

```
Workspace Commands:
  new        Create a new workspace
  rm         Remove a workspace
  ls         List workspaces
  st         Show status across repos
  diff       Show diffs across repos
  log        Show git log across repos
  sync       Fetch and rebase all repos
  exec       Run a command in each repo
  cd         Enter a workspace shell

Repo Commands:
  repo       Manage repos in current workspace

Admin:
  registry   Manage the global repo registry
  template   Manage workspace templates
  config     Manage wsp settings
  completion Generate shell completions
```

This gives the visual grouping benefit of an umbrella without the command depth cost.

### Naming rationale

- **`registry`** (not `mirror`): Users register repos; mirroring is an implementation detail (design tenet: "mirrors are invisible infrastructure"). `registry add` reads naturally. Alternative: `repo-registry` is more explicit but verbose.
- **`template`**: The universally understood word for "stamp-and-go creation artifact." The subcommand is 8 characters, but template management is infrequent — the high-frequency path is `wsp new -t <name>` which is short.
- **`config`** (not `settings`): Universal convention across CLIs. Strictly key-value — no resource management under this noun.
- **`completion`**: standalone utility, no nesting needed.
- **`skill`**: removed. `wsp new` and `wsp repo add/rm` auto-install skills into the workspace via `agentmd::update()`. The `generate` subcommand (dev-only, codegen feature) moves to `just skill`.

### The `repo` vs `registry` distinction

- `wsp repo add/rm/ls/fetch` — workspace-scoped. Operates on repos within the current workspace.
- `wsp registry add/ls/rm` — global. Manages the bare mirror registry that all workspaces draw from.

Different nouns make the scope unambiguous.

## Migration

### Backward compatibility

`wsp setup` continues to work as a hidden alias that dispatches to the new top-level commands. This avoids breaking scripts or muscle memory during the transition.

```
wsp setup repo add ...    ->  wsp registry add ...
wsp setup config get ...  ->  wsp config get ...
wsp setup group ...       ->  wsp template ...
```

Each dispatched invocation prints a deprecation warning to stderr:

```
warning: `wsp setup repo` is deprecated, use `wsp registry` instead
```

### Removal timeline

Remove `wsp setup` alias in the next major version.

### File renames

| Current | New | Reason |
|---------|-----|--------|
| `src/cli/delete.rs` | `src/cli/ws_rm.rs` or keep as-is | Implements `wsp rm`, filename should match verb |
| `src/cli/repo.rs` | keep | Admin repo commands move to `src/cli/registry.rs` |

## Implementation phases

### Phase 1: Verb normalization (standalone, no breaking changes) ✅

- ~~Rename `wsp setup group delete` to `rm` with `remove` and `delete` as aliases~~
- ~~Add `ls` alias to `wsp setup repo list`, `wsp setup group list`, `wsp setup config list`~~
- ~~Add `rm` alias to `wsp setup repo remove`~~
- ~~Consistent primary/alias direction everywhere~~

### Phase 2: Flatten structure

- Create `src/cli/registry.rs` for `wsp registry add/ls/rm`
- Create `src/cli/config.rs` (rename from `cfg.rs`) for `wsp config ls/get/set/unset`
- Promote `completion` to top-level
- Remove `skill` subcommand (workspace creation auto-installs skills; `generate` is dev-only via `just skill`)
- Add Clap `help_heading` grouping
- Wire `wsp setup` as hidden alias with deprecation warnings

### Phase 3: Coordinate with workspace templates

- `wsp template` commands land as part of the workspace templates feature
- `wsp setup group` deprecated alongside `-g` flag (per workspace templates migration plan)
- Update SKILL.md, docs, CLAUDE.md

## Relationship to other features

- **Workspace templates**: `wsp template` is a new top-level noun introduced by that feature. This restructure provides the namespace for it. Phase 2 here and Phase 1 of templates should be coordinated.
- **`wsp init`**: First-time setup wizard. Would become `wsp init` at top level (already planned that way).
- **Hint system**: Hints referencing `wsp setup` commands need updating.
