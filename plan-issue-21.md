# Plan: Command to Change `base_workspace_dir` (Issue #21)

## Context

Session/worktree mode requires all repos to be under a single `base_repo_dir`, which prevents repos in other locations from using session mode. After discussion on issue #16, the maintainer (0xferrous) and contributor agreed on a set of changes split across four issues, each to be submitted as a separate PR:

- **#16** (Phase 1): Default `base_repo_dir` to `/`, fix linked worktree resolution, add `ab dbg list`, add `ab new` hint for bare names.
- **#20** (Phase 2): Add `repo_discovery_dirs` config and `--repo-discovery-dir` CLI flag for repo name lookup.
- **#17** (Phase 3): Handle moved/renamed repos and repair existing workspaces.
- **#21** (Phase 4): Add command to change `base_workspace_dir`.

Phase 4 depends on Phase 1 (issue #16) for `scan_workspaces` infrastructure. It reuses plumbing from Phase 3 (issue #17) for git worktree and jj workspace metadata repair. It is independent of Phase 2 (issue #20).

---

## Phase 4: Command to change `base_workspace_dir` (Issue #21)

This phase adds a command to safely relocate workspace storage to a new directory.

Depends on Phase 1 (`scan_workspaces` infrastructure). Reuses plumbing from Phase 3 (git worktree and jj workspace metadata repair).

### 1. Add `ab dbg relocate` subcommand

**File:** [main.rs](ab/src/main.rs)

Add `DbgCommands::Relocate { new_workspace_dir, dry_run, force }`.

**How it works:**
- Scan all existing workspaces in current `workspace_dir` using `scan_workspaces`.
- Check for active sessions (via `*.lock` files or similar markers). If found, error unless `--force` is set.
- Move the entire workspace directory tree from old to new location.
- For git worktrees: update all `.git` file pointers in session directories to reference the new location, and update the main repos' `$GIT_DIR/worktrees/{session}/gitdir` entries to point to the new workspace paths.
- For jj workspaces: update jj workspace metadata to reference the new workspace paths.
- Update the config file to set `workspace_dir` to the new path.
- Supports `--dry-run` to preview the operation.

### 2. Tests

- `test_relocate_workspaces`: create workspaces, relocate, verify git/jj metadata is correctly updated.
- `test_relocate_with_active_sessions`: verify error when active sessions exist without `--force`.
- `test_relocate_dry_run`: verify preview without modification.

### 3. Update docs

Document the `ab dbg relocate` workflow, including when and why a user might want to change `base_workspace_dir`.

### Phase 4 verification

1. `cargo build` compiles
2. `cargo test` passes
3. Manual test: create workspaces, run `ab dbg relocate /new/path --dry-run`, verify preview
4. Manual test: run `ab dbg relocate /new/path`, verify workspaces are functional at new location
5. Manual test: verify config file is updated with new `workspace_dir`
