# Plan: Improve `ab dbg remove` (follow-up from Phase 1 manual testing)

## Context

Manual testing of Phase 1 (issue #16) identified two usability issues:

1. `ab dbg remove <name>` fails when discovery is not configured (no `base_repo_dir` or `repo_discovery_dirs`), because the repo argument always routes through `locate_repo` -> `discover_repo_ids`.
2. `ab dbg remove` does not clean up git worktree metadata. After removing workspace directories, stale entries persist in the source repo's `.git/worktrees/` and show up in `ab info` and `git worktree list`.

## Fix 1: Accept absolute source paths in `ab dbg remove`

### What changes

In the `DbgCommands::Remove` handler in [main.rs](ab/src/main.rs) (the `else` branch at line 489), before calling `locate_repo`, check if the `repo` argument starts with `/`. If it does, treat it as an absolute source path and resolve it to a `RepoIdentifier` directly via `strip_prefix(base_repo_dir)` instead of going through discovery.

### Implementation

**File:** [main.rs](ab/src/main.rs), lines 489-494

Replace:

```rust
let repo = repo.expect("repo is required by clap unless --unresolved is set");
let repo_id = locate_repo(&config, Some(&repo))?;
```

With:

```rust
let repo = repo.expect("repo is required by clap unless --unresolved is set");
let repo_id = if repo.starts_with('/') {
    // Absolute path: resolve directly without discovery.
    // This allows removing workspaces even when discovery
    // is not configured (e.g., base_repo_dir defaults to "/").
    let path = std::path::Path::new(&repo);
    RepoIdentifier::from_repo_path(&config, path)?
} else {
    locate_repo(&config, Some(&repo))?
};
```

### Update clap help text

**File:** [main.rs](ab/src/main.rs), line 162

Change:

```rust
/// Repository identifier (e.g., "fr/agent-box" or "agent-box")
```

To:

```rust
/// Repository identifier or absolute source path (e.g., "agent-box", "fr/agent-box", or "/home/user/repos/myproject")
```

### Tests

**Automated:** Unit testing this is difficult since it's in `main.rs`. The `from_repo_path` function is already tested. The new code path is a two-line branch that delegates to a tested function.

**Manual:**
1. Remove `base_repo_dir` from config (or comment it out).
2. `cd` into any git repo, run `ab new -s path-test --git`.
3. Run `ab dbg list` to see the full source path.
4. Run `ab dbg remove /home/jessea/Documents/projects/agent-box --dry-run`.
5. **Expected:** Shows the workspace that would be removed, prints `[DRY RUN]`.
6. Run `ab dbg remove /home/jessea/Documents/projects/agent-box`.
7. **Expected:** Prompts for confirmation, removes the workspace.
8. Restore `base_repo_dir`, verify `ab dbg remove agent-box` still works (existing behavior).

### Regenerate CLI docs

Run `nu docs/scripts/generate-cli-reference.nu` to update the `ab dbg remove` help text in the CLI reference.

---

## Fix 2: Clean up git worktree metadata on removal

### What changes

When removing workspace directories, also remove the corresponding git worktree entries. Use `git worktree remove` when the source repo exists (healthy workspaces), and print a note when it doesn't (unresolved workspaces).

### Implementation

#### Step 1: Add a `remove_git_worktree` helper

**File:** [repo.rs](common/src/repo.rs)

Add a function that removes a git worktree entry for a session:

```rust
/// Remove a git worktree by running `git worktree remove`.
/// Falls back gracefully if the source repo is missing.
fn remove_git_worktree(session_path: &Path) -> Result<bool> {
    // Parse the .git file to find the source repo's git dir
    let dot_git = session_path.join(".git");
    if !dot_git.is_file() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&dot_git)?;
    // Format: "gitdir: /path/to/repo/.git/worktrees/session-name"
    let gitdir = content
        .strip_prefix("gitdir: ")
        .map(|s| s.trim())
        .ok_or_else(|| eyre!("unexpected .git file format in {}", dot_git.display()))?;

    // The source repo's .git dir is two levels up from the worktrees entry:
    // /repo/.git/worktrees/session -> /repo/.git
    let git_dir = Path::new(gitdir)
        .parent()  // /repo/.git/worktrees
        .and_then(|p| p.parent());  // /repo/.git

    let Some(git_dir) = git_dir else {
        return Ok(false);
    };

    if !git_dir.exists() {
        // Source repo is gone; cannot clean up worktree metadata.
        return Ok(false);
    }

    // Run git worktree remove. Use --force since the worktree may
    // have uncommitted changes (we're removing it regardless).
    let output = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(session_path)
        .current_dir(git_dir.parent().unwrap_or(git_dir))
        .output()?;

    Ok(output.status.success())
}
```

#### Step 2: Call the helper before `remove_dir_all`

**File:** [repo.rs](common/src/repo.rs), in `remove_repo`

Before `std::fs::remove_dir_all(path)` (line 402), iterate session subdirectories and call `remove_git_worktree` for each:

```rust
// Clean up git worktree metadata for each session before deleting.
if label == &"Git worktrees" {
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let session_path = entry.path();
            if session_path.join(".git").is_file() {
                match remove_git_worktree(&session_path) {
                    Ok(true) => println!("  Pruned git worktree: {}", session_path.display()),
                    Ok(false) => {} // Source repo missing or not a worktree; skip
                    Err(e) => eprintln!("  Warning: failed to prune worktree {}: {e}", session_path.display()),
                }
            }
        }
    }
}
```

Note: When `git worktree remove --force` succeeds, it deletes the session directory AND removes the `.git/worktrees/{session}/` entry. So `remove_dir_all` afterward handles any remaining files that `git worktree remove` didn't touch (e.g., jj files, non-git content).

#### Step 3: Handle the `--unresolved` path

**File:** [main.rs](ab/src/main.rs), in the `--unresolved` removal loop

Before `remove_dir_all` in the unresolved removal loop (line 477), attempt worktree cleanup for git workspaces. Since unresolved workspaces have no source repo, `remove_git_worktree` will return `Ok(false)` and fall through to `remove_dir_all`. Add a note after removal:

```rust
if ws.workspace_type == WorkspaceType::Git {
    println!("  Note: source repo not found; git worktree metadata could not be cleaned up.");
    println!("  If the repo is restored, run `git worktree prune` to remove stale entries.");
}
```

### Tests

**Automated (repo.rs):**

- `test_remove_git_worktree_cleans_metadata`: Create a git repo, add a worktree, call `remove_git_worktree` on the worktree path, verify the `.git/worktrees/{session}/` directory no longer exists and the worktree directory is removed.
- `test_remove_git_worktree_missing_source`: Create a fake `.git` file pointing to a nonexistent gitdir, call `remove_git_worktree`, verify it returns `Ok(false)` without erroring.
- `test_remove_git_worktree_not_a_worktree`: Call on a directory without a `.git` file, verify it returns `Ok(false)`.

**Manual:**
1. Create a workspace: `ab new -s prune-test --git`
2. Verify worktree exists: `git worktree list` (shows `prune-test`).
3. Run `ab dbg remove agent-box`.
4. **Expected:** Output includes `Pruned git worktree: ...prune-test` for each session.
5. Verify: `git worktree list` no longer shows `prune-test`.

---

## Implementation order

1. Fix 1 (absolute path support), since it's a two-line change.
2. Fix 2 (worktree metadata cleanup), since it's more involved and benefits from the absolute path support for manual testing.
3. Regenerate CLI docs.

## Verification

1. `cargo build` compiles.
2. `cargo test --lib --bins` passes.
3. Manual tests from Fix 1 above.
4. Manual tests from Fix 2 above.
5. `ab dbg list` shows no stale workspaces after cleanup.
