# Design Tenets

Principles that guide wsp's design. When tenets conflict, higher-ranked tenets within a section win.

## Git & Mirror Management

1. **No leakage into clones.** A workspace clone looks like a normal `git clone`. No wsp-specific remotes, config, or refs inside `.git/`.
2. **Mirror as shared object cache.** All network fetches flow through the bare mirror. One fetch benefits all workspaces.
3. **Offline-first bootstrapping.** `wsp new` works without network if the mirror is populated.
4. **Mirrors are invisible infrastructure.** Users never manage mirrors. wsp creates, fetches, and garbage-collects them automatically as a side effect of normal operations.
5. **Clones are the developer's space.** wsp owns the mirror and `.wsp.yaml`. Inside a clone, the developer has full autonomy.

## Agent Use

1. **Self-discoverable.** An agent dropped into a workspace can discover and operate wsp without human guidance.
2. **Safe to explore.** Read-only commands have no side effects. Destructive operations require explicit opt-in.
3. **Structured output is the contract.** Every command supports `--json`. Agents never scrape terminal output.

## Human Use

1. **Daily ops are muscle memory.** Common commands are top-level, short, and need no flags for the default case.
2. **Progressive disclosure.** Simple surface, power underneath. Complexity is opt-in.
3. **Explicit side effects.** If a command modifies state, the user chose to run it. No silent mutations hiding inside read commands.
4. **Just workspace management.** wsp is not a build tool, CI system, or git replacement. It orchestrates multi-repo context — nothing more.
5. **Don't duplicate unix.** If something is easy to do by piping `--json` output through `jq`, `grep`, or other standard tools, don't add a flag for it. Compose, don't accumulate.
