# Plan: Default `base_repo_dir` to `/` (Issue #16)

## Context

This plan is part of a larger initiative to enable spawning containers from anywhere on the filesystem. Session/worktree mode currently requires all repos to be under a single `base_repo_dir`, which prevents repos in other locations from using session mode. The changes are split across four issues, each to be submitted as a separate PR:

- **#16** (Phase 1): Default `base_repo_dir` to `/`, fix linked worktree resolution, add `ab dbg list`, add `ab new` hint for bare names.
- **#20** (Phase 2): Add `repo_discovery_dirs` config and `--repo-discovery-dir` CLI flag for repo name lookup.
- **#17** (Phase 3): Handle moved/renamed repos and repair existing workspaces.
- **#21** (Phase 4): Add command to change `base_workspace_dir`.

Phases 3 and 4 reuse plumbing from earlier phases. Phase 2 depends on Phase 1's config changes. Phases 3 and 4 are independent of each other but both depend on Phase 1.

This document covers Phase 1 (Issue #16) in full.

---

## Cross-cutting concerns

These apply across all phases.

### Canonicalize repo paths

All repo path computations (in `find_git_root_from`, `from_repo_path`, and discovery) should canonicalize paths (resolve symlinks) before computing workspace paths. If a user accesses a repo via a symlink (e.g., `~/projects -> /mnt/data/projects`), the workspace path must be the same regardless of which path was used. Without canonicalization, the same repo could get duplicate workspaces.

**Error handling:** `std::fs::canonicalize()` fails if the path does not exist on disk. All canonicalization call sites must wrap the error with a descriptive message explaining the path that was being resolved, e.g., `.wrap_err_with(|| format!("failed to canonicalize repo path: {}", path.display()))`. This ensures that if a repo is deleted between discovery and path computation, the error is debuggable rather than a bare IO error.

**Migration note:** Adding canonicalization changes the workspace path for users who currently access repos through symlinks (e.g., `~/projects -> /mnt/data/projects`). Workspaces previously created via the symlink path become unresolved since the canonical (physical) path is now used. This is an acceptable trade-off for deduplication correctness. The `ab dbg list --unresolved` tooling (Phase 1, step 10) surfaces these cases, and Phase 1's migration documentation should call out symlinks as another reason workspaces may become unresolved.

**Assumption:** `path::expand_path` (the free function in [path.rs](common/src/path.rs) line 260, not the `expand_path` method in [config.rs](common/src/config.rs) line 196 which only does tilde expansion) calls `std::fs::canonicalize` when the path exists on disk, so `base_repo_dir` (and `repo_discovery_dirs`, added in Phase 2) is canonicalized at config load time. If a configured path does not exist at load time, it is stored without symlink resolution. This is acceptable because a non-existent directory cannot contain repos, so `strip_prefix` is never called against it.

**Where canonicalization must happen:**
- `find_git_root_from`: canonicalize the return value in all branches (linked worktree, normal repo, submodule).
- `from_repo_path`: do NOT canonicalize here. All callers already provide canonicalized paths (`find_git_root_from` canonicalizes its output, and `discover_repos_in_dir` canonicalizes discovered paths). Instead, add a `debug_assert!` that validates the precondition: `debug_assert!(repo_path.components().all(|c| !matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir)), "from_repo_path expects a canonical path, got: {}", repo_path.display())`. Note: this assert catches `.` and `..` components (the most common programming mistakes) but cannot detect unresolved symlinks without hitting the filesystem, which is inappropriate for a debug_assert. Add a comment on the function explaining this contract: callers must pass canonicalized paths (symlinks resolved, no `.`/`..`); `find_git_root_from` and `discover_repos_in_dir` handle canonicalization at their respective boundaries.
- `discover_repos_in_dir`: canonicalize discovered repo paths before calling `strip_prefix`.

### Verify all `find_git_root` / `gix::discover` call sites

Before implementing linked worktree resolution, grep for `find_git_root` and `gix::discover` across the codebase to confirm that the plan covers every call site. As of this writing, the known call sites are:
- [repo.rs](common/src/repo.rs): defines `find_git_root()`
- [repo.rs](common/src/repo.rs) line 76: calls `find_git_root()` in `resolve_repo_id()` (used by `new_workspace` for session mode). After Phase 1 step 4 redefines `find_git_root()` as a wrapper around `find_git_root_from(cwd)`, this automatically resolves linked worktrees to the main repo root, which is the desired session-mode behavior.
- [config.rs](common/src/config.rs) line 808: calls `find_git_root()` in `load_config()` to locate repo-local config
- [main.rs](ab/src/main.rs) line 260: calls `find_git_root()` in local mode
- [display.rs](common/src/display.rs) lines 19-22: calls `gix::discover` directly

The `config.rs` call site does not need a separate step. After Phase 1 step 4 redefines `find_git_root()` as a wrapper around `find_git_root_from(cwd)`, `load_config()` automatically resolves to the main repo root when called from a linked worktree. This is the desired behavior: repo-local config should be shared across all worktrees of the same repo, not loaded per-worktree.

If additional call sites are found, add them to the relevant steps before proceeding.

---

## Phase 1: Default `base_repo_dir` to `/` (Issue #16)

This phase makes session mode work for repos anywhere on the filesystem without upfront configuration.

### 1. Make `base_repo_dir` optional with `/` default

**File:** [config.rs](common/src/config.rs)

- Add a `default_base_repo_dir()` function returning `PathBuf::from("/")`
- Add `#[serde(default = "default_base_repo_dir")]` to the `base_repo_dir` field (line 583)
- No change to `load_config()` needed; `expand_path("/")` canonicalizes to `/`

### 2. Track whether `base_repo_dir` was explicitly set

**File:** [config.rs](common/src/config.rs)

Needed to distinguish "user set `base_repo_dir`" from "defaulted to `/`" for discovery fallback behavior.

- Add `#[serde(skip)] pub base_repo_dir_explicit: bool` to `Config`
- In `load_config()`, before extracting config, check `figment.find_value("base_repo_dir").is_ok()` and store the result. Then after extracting config, set `config.base_repo_dir_explicit` to that stored value. Note: `figment.extract()` takes `&self` (not `self`), so the figment remains available after extraction; calling `find_value` either before or after `extract` works. The `test_config_without_base_repo_dir` test (step 8) validates that `find_value` correctly returns `Err` when the key is absent from all providers (not filled by serde defaults).

### 3. Rewrite `discover_repo_ids` for new discovery logic

**File:** [path.rs](common/src/path.rs) lines 162-168

With `base_repo_dir` defaulting to `/`, the existing `discover_repo_ids` would scan the entire filesystem. Rewrite to only scan when safe:

```
Priority 1: If base_repo_dir was explicitly set AND is not "/", scan it (backward compat)
             -> discover_repos_in_dir(config.base_repo_dir, is_repo) [unchanged signature]
Priority 2: Otherwise, return error with guidance
```

Priority 1 must check `base_repo_dir != "/"` in addition to `base_repo_dir_explicit`. Without this guard, a user who explicitly writes `base_repo_dir = "/"` in their TOML (perhaps misunderstanding the new defaults) would trigger a `WalkDir` scan of the entire filesystem.

Priority 2 is the default out-of-box state (since `base_repo_dir` defaults to `/`). It also covers the case where `base_repo_dir` is explicitly set to `/`. The error message should include an example TOML snippet showing how to configure discovery:

```
Error: no repository discovery directories configured

To use the -r flag, add one of the following to your config:

  repo_discovery_dirs = ["~/repos", "~/work"]

or:

  base_repo_dir = "/home/user/repos"
```

Note: this error message references `repo_discovery_dirs` which is added in Phase 2. This is intentional; the error guides users toward the preferred solution even before Phase 2 lands. Users who only have Phase 1 installed can use `base_repo_dir` instead.

Also add canonicalization to `discover_repos_in_dir`: canonicalize discovered repo paths before calling `strip_prefix`. Unlike `find_git_root_from` (which fails hard), discovery should warn and skip on canonicalization failure, since one dangling symlink or permission issue should not abort discovery of all other repos:
```rust
let canonical = match discovered_path.canonicalize() {
    Ok(p) => p,
    Err(e) => {
        eprintln!("warning: skipping repo, failed to canonicalize path: {}: {e}", discovered_path.display());
        continue;
    }
};
```

### 4. Fix linked worktree resolution in `find_git_root()`

**File:** [repo.rs](common/src/repo.rs) lines 9-24

The current `find_git_root()` uses `repo.workdir()`, which returns the linked worktree directory when called from inside a session workspace, not the main repo root. This causes `ab new`/`ab spawn` from inside a session workspace to treat the worktree as a separate repo.

Refactor into `find_git_root_from(path: &Path)` that detects linked worktrees and resolves to the main repo root. gix 0.77 exposes both `repo.kind()` returning `Kind::WorkTree { is_linked: bool }` and `repo.common_dir() -> &Path` (returns the main git dir for linked worktrees), so use the gix API directly:

```rust
use gix::repository::Kind;

fn find_git_root_from(path: &Path) -> Result<PathBuf> {
    let repo = gix::discover(path).wrap_err_with(|| {
        format!("Failed to discover git repository in {}", path.display())
    })?;
    let root = match repo.kind() {
        Kind::WorkTree { is_linked: true } => {
            // Linked worktree: resolve to main repo root.
            // common_dir() returns the main repo's .git directory.
            // Open it directly rather than re-discovering, since we
            // already know the exact git directory location.
            let common = repo.common_dir().canonicalize()
                .wrap_err_with(|| format!(
                    "failed to canonicalize common_dir: {}",
                    repo.common_dir().display()
                ))?;
            let main_repo = gix::open(&common).wrap_err_with(|| {
                format!("failed to open main repo from common_dir: {}", common.display())
            })?;
            main_repo.workdir()
                .ok_or_else(|| eyre!(
                    "linked worktree's main repository at {} is bare \
                    and has no working directory",
                    common.display()
                ))
                .map(|p| p.to_path_buf())?
        }
        Kind::Bare => {
            bail!(
                "bare repository at {} has no working directory",
                repo.git_dir().display()
            )
        }
        _ => {
            // Normal repo or submodule: use workdir directly.
            // Note: submodules are intentionally treated as independent repos
            // and get their own workspaces. If a user runs `ab new` from inside
            // a submodule, the submodule root is used, not the parent superproject.
            repo.workdir()
                .ok_or_eyre("repository has no working directory")
                .map(|p| p.to_path_buf())?
        }
    };
    // Canonicalize to ensure consistent workspace paths regardless of symlinks
    root.canonicalize()
        .wrap_err_with(|| format!("failed to canonicalize repo root: {}", root.display()))
}
```

**Error messages are intentionally generic.** `find_git_root_from` returns neutral errors like "has no working directory" rather than mentioning session mode, since it is also called by `load_config()` and other non-session contexts. Callers that need session-specific guidance (e.g., `resolve_repo_id`) should wrap the error with `.wrap_err("bare repositories are not supported for session mode; use a non-bare clone instead")`.

**Why `gix::open` instead of `common_dir().parent()`:** For a normal repo, `common_dir()` is `.git`, so `.parent()` gives the repo root. But for a linked worktree of a *bare* repository (e.g., at `/path/to/repo.git/`), `common_dir()` is `/path/to/repo.git/`, and `.parent()` gives `/path/to/` instead of the bare repo root. Using `gix::open(common_dir)` directly opens the git directory as a repository, which correctly handles both bare and non-bare main repos. We then check `workdir()` to produce a clear error for the bare case. Note: `gix::open` is preferred over `gix::discover` here because we already know the exact git directory location; `discover` would redundantly search upward from the path.

Redefine `find_git_root()` as `find_git_root_from(&std::env::current_dir()?)`.

**jj workspace resolution:** 0xferrous noted that linked worktree resolution should also be implemented for jj workspaces. jj's equivalent of a linked worktree is a workspace created via `jj workspace add`. When `ab new`/`ab spawn` is run from inside a jj workspace, it should resolve to the main workspace root rather than treating the workspace as a separate repo. The resolution approach depends on what jj_lib exposes for workspace detection; investigate `jj_lib::workspace::Workspace::load` and related APIs to determine how to detect and resolve linked jj workspaces. If jj_lib does not provide a straightforward mechanism, document the limitation and defer to a follow-up.

### 5. Add `find_git_workdir()` for local mode

**File:** [repo.rs](common/src/repo.rs)

Local mode (`--local`) already works correctly in workspaces/worktrees (confirmed by 0xferrous). It should preserve current behavior: use whatever directory the user is in, even if it is a linked worktree. Add `find_git_workdir()` / `find_git_workdir_from(path)` that call `gix::discover()` and return `workdir()` directly without linked worktree resolution. This is distinct from `find_git_root()` which resolves to the main repo.

### 6. Fix worktree resolution in `display.rs`

**File:** [display.rs](common/src/display.rs) lines 19-22

Same linked worktree bug as `find_git_root()`. Currently uses `gix::discover(&cwd)` + `workdir()` directly. Replace with `find_git_root_from(&cwd).ok()` from repo.rs so `ab info` shows the source repo's workspaces when run from inside a session workspace. Important: use `.ok()` (not `?`) to preserve the existing graceful handling when the user is not inside a git repo. The existing `let Some(repo_path) = ... else { eprintln!("Not in a git repository"); return Ok(()); }` pattern on the following lines stays the same.

- Line 29: `RepoIdentifier::from_repo_path(config, &repo_path)` stays the same (still needs config for `base_repo_dir`)
- Line 33: `repo_id.git_worktrees(config)` stays the same
- Line 62: `repo_id.jj_workspaces(config)` stays the same

Add a repo identity header at the top of `info()` output showing the full source path:
```
Repository: /home/user/path/to/my-repo
```
Use `repo_id.source_path(config).display()`.

### 7. Update `main.rs` local mode to use `find_git_workdir()`

**File:** [main.rs](ab/src/main.rs) lines 255-261

In local mode, replace the current `find_git_root()` call with `find_git_workdir()` (or `find_git_workdir_from(path)` if `--repo` is provided). This preserves the intentional distinction: session mode resolves to the main repo root, local mode uses the directory as-is.

### 8. Clean up stale debug print

**File:** [repo.rs](common/src/repo.rs) line 80

Remove `println!("debug: {repo_id:?}");`

### 9. Add `ab new` hint for bare names

**File:** [repo.rs](common/src/repo.rs) or [main.rs](ab/src/main.rs)

When `ab new my-project` is run and `gix::discover()` fails (because `my-project` is a bare name, not a valid path), include a hint in the error message:
```
Error: could not find a git repository at 'my-project'
Hint: use 'cd my-project && ab new' or pass a full path like 'ab new /path/to/my-project'
```
This applies when a positional arg is provided to `ab new` and discovery fails.

### 10. Add `ab dbg list` subcommand

**File:** [main.rs](ab/src/main.rs)

Add a new `DbgCommands::List` variant that scans all workspace directories and displays a summary. Extract the core scanning logic into a shared `scan_workspaces(config) -> Result<Vec<WorkspaceInfo>>` function (or similar) that both `list` and `remove --unresolved` (step 11) can call. `list` displays the results; `remove --unresolved` filters to unresolved entries and deletes them. This avoids duplicating the scanning, status-checking, and path-reconstruction logic between the two commands.

**How it works:**
- Walk `{workspace_dir}/git/` and `{workspace_dir}/jj/` recursively.
- Identify session directories by their markers: `.git` *file* (for git worktrees) or `.jj/working_copy/` (for jj workspaces). Important: use `path.join(".git").is_file()`, not `.exists()`, to distinguish git linked worktrees (which have a `.git` file pointing to the main repo) from full git repositories (which have a `.git` directory). Only `.git` files are valid session markers.
- **Stop descending after finding a session marker.** Once a `.git` file or `.jj/working_copy/` directory is found in a directory, do not walk into that directory's children. Session worktrees/workspaces are leaf nodes in the workspace tree; descending further would risk false positives from submodule `.git` files if submodules are initialized inside a session worktree. Implement this using `WalkDir::into_iter()` and calling `iter.skip_current_dir()` after detecting a session marker. Do NOT use `filter_entry` for this, because `filter_entry` returning `false` both excludes the entry from iteration and skips its children, meaning the session directory itself would never be yielded and could not be counted. The same `skip_current_dir()` pattern applies to both git and jj session markers.
- **Also skip `.git` directories.** If a full git repository (with a `.git` *directory*, not file) is found inside the workspace tree (e.g., someone manually cloned a repo there), call `iter.skip_current_dir()` to avoid descending into it. Without this guard, linked worktrees inside that nested repo could produce false-positive session matches.
- The session directory is the immediate parent of the `.git` file marker. Its name is the session name. Everything between `{workspace_dir}/git/` and the session directory name is the repo's relative path. For example, given `{workspace_dir}/git/home/user/repos/myproject/my-session/.git`, the session name is `my-session` and the repo relative path is `home/user/repos/myproject`.
- Reconstruct the source repo path using `config.base_repo_dir.join(relative_path)` (when `base_repo_dir` is `/`, this is equivalent to prepending `/`).
- Check if the source repo still exists on disk to determine status.

**Output columns (in order):**
- **Source path:** full reconstructed path. Leads the line since it is the most distinguishing field for scanning.
- **Type:** git/jj.
- **Sessions:** count of session subdirectories.
- **Status:** `healthy` (source repo exists) or `unresolved` (source repo not found at reconstructed path). Placed last so that the common case (`healthy`) does not dominate the left edge; `unresolved` entries stand out as the exception.

**Example output:**
```
/home/user/repos/agent-box        git  3 sessions  healthy
/home/user/work/my-project        git  1 session   healthy
/home/user/repos/old-repo         git  2 sessions  unresolved
```

If any workspaces are `unresolved`, print a footer note:
```
Note: "unresolved" means no repo was found at the reconstructed path.
This can happen if the repo was deleted or if base_repo_dir changed since the workspace was created.
To clean up: ab dbg remove --unresolved
```

**Flags:**
- `--unresolved`: filter to only unresolved workspaces.

### 11. Enhance `ab dbg remove` with `--unresolved` flag

**File:** [main.rs](ab/src/main.rs) lines 152-162

Add an `--unresolved` flag to the existing `Remove` subcommand. When set, scan all workspace directories (same logic as `ab dbg list`), find unresolved workspaces (source path not found at reconstructed path), and remove them. Respects existing `--dry-run` and `--force` flags. Without `--force`, prompt for confirmation listing what will be deleted.

**Concurrency note:** No locking is implemented for workspace removal. If another `ab` process is actively using a workspace, removal could break it. In practice this is low-risk: unresolved workspaces have no source repo on disk, so they are unlikely to be in active use. The confirmation prompt (without `--force`) is the primary safety mechanism.

### 12. Regenerate JSON schema

**File:** [config.schema.json](common/config.schema.json)
**Generator:** [generate_schema.rs](common/src/bin/generate_schema.rs)

The schema is auto-generated from the `Config` struct via `JsonSchema` derive. After config struct changes in steps 1-2, regenerate:
```bash
cargo run --bin generate_schema > common/config.schema.json
```
Do not hand-edit the schema file.

### 13. Update and add tests

#### Fix existing tests

**File:** [path.rs](common/src/path.rs) lines 309-501
- Add `base_repo_dir_explicit: true` to `make_test_config()` and all inline `Config` structs in tests (lines 380, 422, 464)

**File:** [runtime/mod.rs](ab/src/runtime/mod.rs) - add new field to the ~5 full `Config` structs constructed in tests (lines ~1328, ~1422, ~1498, ~1597, ~1670). Since `base_repo_dir_explicit` uses `#[serde(skip)]` (defaults to `false`), tests that construct `Config` via deserialization get correct defaults automatically. For tests that build `Config` structs manually, add the field explicitly.

#### New tests for nested directory mirroring

**File:** [path.rs](common/src/path.rs)

- `test_nested_directory_mirroring`: with `base_repo_dir = "/"`, verify `from_repo_path` on `/home/user/repos/myproject` gives `relative_path = "home/user/repos/myproject"`, `source_path` reconstructs to `/home/user/repos/myproject`, and `git_workspace_path` returns `{workspace_dir}/git/home/user/repos/myproject/{session}`
- `test_nested_mirroring_with_explicit_base`: with `base_repo_dir = "/home/user/repos"`, verify existing behavior unchanged (relative_path = `myproject`, workspace path = `{workspace_dir}/git/myproject/{session}`)

#### New tests for repo discovery

**File:** [path.rs](common/src/path.rs)

- `test_discover_fallback_to_explicit_base_repo_dir`: set `base_repo_dir` explicitly (no discovery dirs), verify it scans `base_repo_dir` (backward compat)
- `test_discover_no_dirs_configured`: both at defaults (`base_repo_dir = "/"`, empty discovery dirs), verify `discover_repo_ids` returns an error
- `test_discover_explicit_root_base_repo_dir`: set `base_repo_dir = "/"` explicitly, verify `discover_repo_ids` returns an error (same as default; explicit `/` does not trigger a filesystem scan)

#### New tests for linked worktree resolution

**File:** [repo.rs](common/src/repo.rs)

- `test_find_git_root_from_main_worktree`: create a real git repo with `git init`, verify `find_git_root_from` returns the repo root
- `test_find_git_root_from_linked_worktree`: create a git repo, add a linked worktree with `git worktree add`, verify `find_git_root_from` called from inside the linked worktree resolves to the main repo root (not the worktree directory)
- `test_find_git_workdir_from_linked_worktree`: same setup, verify `find_git_workdir_from` returns the linked worktree directory (not the main repo root)
- `test_find_git_root_from_linked_worktree_for_config`: verify that `find_git_root()` (the wrapper used by `load_config()`) resolves to the main repo root when called from a linked worktree, confirming repo-local config is shared across worktrees
- `test_find_git_root_from_linked_worktree_of_bare_repo`: create a bare repo with `git init --bare`, add a linked worktree with `git worktree add`, verify `find_git_root_from` called from inside the linked worktree returns a clear error (since the main repo is bare and has no working directory)

#### New tests for config deserialization

**File:** [config.rs](common/src/config.rs)

- `test_config_without_base_repo_dir`: TOML without `base_repo_dir`, verify it defaults to `/` and `base_repo_dir_explicit` is false
- `test_config_with_explicit_base_repo_dir`: TOML with `base_repo_dir = "/home/user/repos"`, verify `base_repo_dir_explicit` is true

#### New tests for `ab dbg list` logic

**File:** [path.rs](common/src/path.rs) or new test file

- `test_dbg_list_finds_workspaces`: create a workspace directory structure mirroring a repo path with session subdirectories containing `.git` files, verify the list logic correctly identifies workspaces, reconstructs source paths, and counts sessions
- `test_dbg_list_detects_unresolved`: same setup but with the source repo path not existing on disk, verify status is reported as unresolved.
- `test_dbg_list_ignores_submodule_git_files`: create a session worktree with an initialized submodule inside it (submodule has its own `.git` file), verify the walker does not count the submodule as a separate session.

### 14. Update docs

- [config.md](docs/src/reference/agent-box/config.md): mark `base_repo_dir` optional with `/` default, document path length trade-off
- [first-run.md](docs/src/tutorials/agent-box/first-run.md): remove `base_repo_dir` from required fields
- [agent-box-overview.md](docs/src/explanation/architecture/agent-box-overview.md): update wording
- [agent-box-workflow.md](docs/src/explanation/architecture/agent-box-workflow.md): update wording

### 15. Migration note and stale workspace warning

Existing users with an explicit `base_repo_dir` will continue to work unchanged. However, if a user removes `base_repo_dir` from their config (to use the new `/` default), their old workspaces at `{workspace_dir}/git/{short_relative_path}/` become unresolved, since the tool will now look for them at `{workspace_dir}/git/{full_absolute_path}/`. Document this: users should either keep their existing `base_repo_dir` setting, or recreate workspaces with `ab new` after removing it.

**Automated migration (deferred):** An `ab dbg migrate` command that renames workspace directories from the old layout to the new layout is intentionally deferred. Moving git worktree directories requires rewriting internal `.git` file pointers and the main repo's `$GIT_DIR/worktrees/` entries, which is error-prone. Since keeping the existing `base_repo_dir` setting preserves full backward compatibility, automated migration is only worth building if users actually request it.

**Path length trade-off:** The `/` default produces longer workspace paths (e.g., `{workspace_dir}/git/home/user/repos/myproject/session` vs. `{workspace_dir}/git/myproject/session`). Users who find these unwieldy can set `base_repo_dir` to a common ancestor of their repos for shorter paths. This should be called out in the config reference (step 14).

Similarly, workspaces created via a symlinked path before canonicalization was added will become unresolved, since the tool now resolves symlinks to physical paths before computing workspace paths. Users can identify these with `ab dbg list --unresolved` and clean them up with `ab dbg remove --unresolved`.

**No startup warning.** A startup warning for the current repo was considered but would not trigger in practice: `find_git_root_from` discovers the repo from an existing path, and the round-trip through `from_repo_path` + `source_path` always reconstructs that same path. Stale workspaces from a changed `base_repo_dir` or pre-canonicalization symlink paths are only detectable by scanning the workspace directory tree, which is what `ab dbg list --unresolved` already does. Users are directed to that command via the migration documentation above.

### Phase 1 implementation order

Tests are written alongside each step rather than batched at the end, so regressions are caught incrementally.

1. Steps 1-2 (config changes) + config deserialization tests from step 13, fix existing test `Config` structs in path.rs and runtime/mod.rs. Compile, all tests pass.
2. Steps 4-5 and 7 (linked worktree resolution in repo.rs, `find_git_workdir` for local mode, and main.rs local mode update) + worktree resolution tests from step 13. These must be atomic: step 4 changes `find_git_root()` to resolve linked worktrees to the main repo root, which would break local mode if step 7's switch to `find_git_workdir()` is not applied in the same step.
3. Step 6 (display.rs worktree fix)
4. Step 8 (remove debug print)
5. Step 3 (rewrite `discover_repo_ids` + canonicalization in `discover_repos_in_dir`) + nested directory mirroring tests and discovery tests from step 13.
6. Step 10 (`ab dbg list` subcommand) + `ab dbg list` tests from step 13.
7. Step 11 (`ab dbg remove --unresolved`)
8. Step 9 (`ab new` hint for bare names)
9. Step 12 (schema)
10. Step 14 (docs)

### Phase 1 verification

1. `cargo build` compiles
2. `cargo test` passes (all existing + new tests)
3. Manual test: remove `base_repo_dir` from `~/.agent-box.toml`, run `ab new -s test --git` from inside a repo, verify workspace created at `{workspace_dir}/git/home/.../repo-name/test`
4. Manual test: set `base_repo_dir` explicitly, verify existing behavior unchanged
5. Manual test: `cd` into a session workspace (linked worktree), run `ab info`, verify it shows the source repo's workspaces
6. Manual test: `cd` into a session workspace (linked worktree), run `ab new -s another --git`, verify it creates a workspace for the source repo (not for the worktree itself)
7. Manual test: `ab spawn --local` from inside a session workspace, verify it mounts the worktree directory (not the main repo root)
8. Manual test: `ab dbg list` shows all workspaces with correct source paths and session counts
9. Manual test: `ab dbg list --unresolved` shows only workspaces whose source repo is not found at the reconstructed path
10. Manual test: `ab dbg remove --unresolved --dry-run` previews unresolved workspace cleanup
11. Manual test: `ab dbg remove --unresolved` removes unresolved workspaces (with confirmation)
12. Manual test: `ab info` shows repo identity header with full path
13. Manual test: `ab new my-project` (bare name, not a path) shows helpful hint in error
