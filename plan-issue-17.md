# Plan: Handle Moved/Renamed Repos (Issue #17)

## Context

This plan is part of a larger initiative to enable spawning containers from anywhere on the filesystem, split across four issues as separate PRs:

- **#16** (Phase 1): Default `base_repo_dir` to `/`, fix linked worktree resolution, add `ab dbg list`, add `ab new` hint for bare names.
- **#20** (Phase 2): Add `repo_discovery_dirs` config and `--repo-discovery-dir` CLI flag for repo name lookup.
- **#17** (Phase 3): Handle moved/renamed repos and repair existing workspaces.
- **#21** (Phase 4): Add command to change `base_workspace_dir`.

Phase 3 depends on Phase 1 (issue #16) for `ab dbg list` scanning infrastructure and `scan_workspaces`. It is independent of Phase 2 (#20) and Phase 4 (#21).

---

## Phase 3: Handle moved/renamed repos (Issue #17)

This phase adds support for both proactive renames/moves and retrospective repair of workspaces when a source repo has been relocated.

Depends on Phase 1 (`ab dbg list` scanning infrastructure, `scan_workspaces`).

### 1. Add `ab dbg remap` subcommand

**File:** [main.rs](ab/src/main.rs)

Add `DbgCommands::Remap { old_path, new_path, dry_run }`. This updates a workspace's directory structure after a repo has been moved or renamed.

**How it works:**
- Validate `new_path` exists and is a git/jj repo.
- Compute old and new relative paths via `strip_prefix(base_repo_dir)`.
- Locate old workspace directory at `{workspace_dir}/{type}/{old_relative_path}/`.
- Rename the workspace directory to `{workspace_dir}/{type}/{new_relative_path}/`.
- For git worktrees: rewrite each session's `.git` file to point to the updated main repo's `$GIT_DIR/worktrees/` entries, and update the main repo's `$GIT_DIR/worktrees/{session}/gitdir` to point back to the new workspace location.
- For jj workspaces: update jj workspace metadata to reflect the new source repo path.
- Supports `--dry-run` to preview changes without making them.

### 2. Add `ab repo repair` subcommand

For after-the-fact repairs when the source repo has already moved and the user knows the old and new locations.

**How it works:**
- Takes `<old-repo-id> <new-repo-id>` as arguments.
- Calls the same underlying remap logic as `ab dbg remap`.
- Repairs git worktree metadata (`.git` file pointers, `$GIT_DIR/worktrees/` entries).
- Repairs jj workspace metadata.

### 3. Enhance `ab dbg list` for move detection

When `ab spawn` cannot find a workspace, scan sentinels/workspaces for orphans with a matching directory name and suggest `ab dbg remap` in the error message.

### 4. Tests

- `test_remap_git_worktree`: create workspace, simulate repo move, run remap, verify `.git` file pointers and `$GIT_DIR/worktrees/` entries are updated.
- `test_remap_jj_workspace`: same for jj workspaces.
- `test_remap_dry_run`: verify dry run previews changes without modifying anything.
- `test_remap_nonexistent_old_path`: verify clear error when old workspace does not exist.

### 5. Update docs

Document the `ab dbg remap` and `ab repo repair` workflows. Include examples showing the before/after state.

### Phase 3 verification

1. `cargo build` compiles
2. `cargo test` passes
3. Manual test: move a source repo, run `ab dbg remap <old> <new>`, verify `ab spawn` works with the repo at its new location
4. Manual test: `ab dbg list` shows the repaired workspace as healthy
5. Manual test: `--dry-run` previews without modifying
