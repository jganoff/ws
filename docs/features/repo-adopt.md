# Feature: Repo Adoption

Adopt an existing git directory into a workspace without re-cloning. When `wsp repo add` detects the target directory already exists, it validates and registers it in-place.

## Motivation

Users sometimes create repos locally inside a workspace before registering them with wsp:

```
cd ~/dev/workspaces/my-feature/
mkdir runtime && cd runtime && git init
# ... develop, commit, push to GitHub ...
git remote add origin git@github.com:acme/runtime.git
git push -u origin main
```

Today, `wsp repo add` only knows how to clone from a mirror. There is no way to retroactively manage an existing directory. The repo appears as "untracked" in `wsp st`, and while `wsp rm` correctly blocks on it, the user has no path to integrate it.

## Behavior

### Detection

In `workspace::add_repos`, before calling `clone_from_mirror`, check if the target directory already exists on disk. If it does, enter the adoption path instead of the clone path.

### Validation

An existing directory must pass three checks:

| Check | Error message |
|-------|---------------|
| Has `.git` | `directory "foo" exists but is not a git repository` |
| Has `origin` remote | `directory "foo" exists but has no origin remote` |
| Origin URL resolves to expected identity | `directory "foo" origin remote (X) doesn't match expected repo (Y)` |

Identity comparison uses `giturl::parse`, so `git@github.com:acme/foo.git` and `https://github.com/acme/foo.git` both resolve to `github.com/acme/foo` and are treated as equivalent.

### Origin URL prompt

After identity validation, if the origin URL differs from the registered URL (e.g., SSH vs HTTPS), prompt the user:

```
  warning: foo/ origin URL differs from registered URL
    clone:      git@github.com:acme/foo.git
    registered: https://github.com/acme/foo
    [1] Keep current origin URL (default)
    [2] Repoint origin to registered URL
  choice [1]:
```

Non-interactive (stdin is not a terminal): keep as-is with a warning.

### Branch prompt

If the repo is not on the workspace branch, prompt:

| State | Prompt |
|-------|--------|
| Already on workspace branch | Silent, no prompt |
| Workspace branch exists, not checked out | Offer: leave as-is (default) or switch |
| Workspace branch doesn't exist | Offer: leave as-is (default) or create from current HEAD |

Non-interactive: leave as-is with a warning.

### Mirror ref propagation

After adoption, propagate refs from the mirror into the adopted directory (steps 4-6 of `clone_from_mirror`):

1. `git fetch <mirror_path> +refs/remotes/origin/*:refs/remotes/origin/*` — populate remote-tracking refs
2. `git remote set-head origin <default_branch>` — set origin/HEAD
3. Fix default branch tracking — `git branch --set-upstream-to origin/<default> <default>`

This ensures `wsp rm` safety checks and `wsp sync` work correctly on adopted repos.

### Auto-registration from URL

`wsp repo add` currently only accepts registered shortnames. With adoption, it also accepts full URLs. If the URL is not yet registered globally:

1. Parse URL to derive identity
2. Create bare mirror from upstream (3-phase locking pattern)
3. Register in `config.yaml`
4. Proceed with normal workspace addition (which enters the adopt path)

This means a single command handles the full workflow:

```
wsp repo add git@github.com:acme/runtime.git
```

### What adoption does NOT do

- **Modify git hooks or config** — clones are the developer's space (design tenet 5)
- **Force checkout** — branch changes are always opt-in via prompt
- **Clone from mirror** — the directory is used as-is; only refs are propagated

## Implementation

### Files changed

- `src/git.rs` — `remote_get_url(dir, remote)` helper
- `src/workspace.rs` — `validate_existing_dir`, `prompt_origin_url_for_adopt`, `prompt_branch_for_adopt`, `propagate_mirror_refs`, modified `add_repos`
- `src/cli/add.rs` — URL argument support, auto-registration with 3-phase locking

### Phases

- [x] Phase 1: Adopt existing directory (validate, branch prompt, register in metadata)
- [x] Phase 2: Mirror ref propagation after adoption
- [x] Phase 3: Auto-registration from URL in `wsp repo add`
- [x] Phase 4: Non-interactive mode (terminal detection, safe defaults)
- [x] Phase 5: Origin URL prompt when clone URL differs from registered URL

## Edge cases

| Scenario | Behavior |
|----------|----------|
| Directory exists, not a git repo | Error |
| Directory exists, no origin remote | Error |
| Directory exists, identity mismatch | Error |
| Directory is a symlink | Follows symlink (same as normal path resolution) |
| Repo already in `.wsp.yaml` | Skip with message (existing behavior) |
| Concurrent `wsp repo add` registers same repo | Graceful continue — desired state achieved |
| Directory name collides with existing repo | Disambiguation via `compute_dir_names` (existing behavior) |
| Adoption with `@ref` syntax | Treated as context repo, branch prompt skipped |
