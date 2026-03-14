---
name: check-skill-registration
description: Verify all built-in skills are registered in check_claude_dir managed sets
user_invocable: true
---

# Check Skill Registration

Verify that every built-in skill installed by `install_skill()` in `src/agentmd.rs` is also registered in the `check_claude_dir()` managed sets in `src/workspace.rs`, so `wsp rm` and `wsp rename` don't falsely flag them as user content.

## Steps

### 1. Collect installed skills

Read `src/agentmd.rs` and find all skill directory paths created by `install_skill()`. These follow the pattern:

```rust
let <name>_dir = ws_dir.join(".claude/skills/<skill-name>");
```

Extract the list of `<skill-name>` values.

### 2. Collect registered managed paths

Read `src/workspace.rs`, find the `check_claude_dir()` function, and extract:
- The `managed` set — expected to contain `skills/<skill-name>/SKILL.md` for each skill
- The `managed_dirs` set — expected to contain `skills/<skill-name>` for each skill

### 3. Compare and report

For each installed skill from step 1, check that:
- `skills/<skill-name>/SKILL.md` is in `managed`
- `skills/<skill-name>` is in `managed_dirs`

If any are missing, report which entries need to be added and offer to fix them.

### 4. Also verify the skills/ directory itself

Confirm that `"skills"` is in `managed_dirs` (it's the parent directory needed for the walk).

## When to use

Run this skill after adding a new built-in skill to `skills/` and wiring it into `src/agentmd.rs`. This catches the easy-to-forget step of updating `check_claude_dir()`.
