# Plan: Remove `base_repo_dir` from agent-box

## Context

[**GitHub issue draft**](./github-issue.md)

**Problem:** Session/worktree mode in agent-box requires all repositories to be under a single `base_repo_dir` config field. This is limiting because repositories are often spread across multiple locations, and the requirement adds upfront configuration before the tool can be used. Workarounds don't hold up:

- `--local` mode bypasses the check, but loses worktree sandboxing.
- Setting `base_repo_dir` to a common ancestor like `~` slows discovery by scanning the entire home directory.
- Symlinking repos into `base_repo_dir` doesn't work because agent-box resolves symlinks to their real paths, which then fall outside `base_repo_dir`.

**Goal:** Remove `base_repo_dir` entirely. `ab new` will detect the git/jj root from the current directory. Workspace paths will use a content-addressable hash of the full canonical path (e.g., `{workspace_dir}/git/a1b2c3d4e5f6a7b8/{session}`), with a `.source-repo` sentinel file providing the human-readable mapping.

## Changes

### 1. `Cargo.toml` -- Add rapidhash dependency

Add `rapidhash = "4.4"` to workspace dependencies in the root `Cargo.toml` and to `common/Cargo.toml`. (Version 4.4 has been verified to exist on crates.io.)

### 2. `common/src/path.rs` -- RepoIdentifier redesign

**RepoIdentifier struct** (line 35-39): Change `relative_path: PathBuf` to `repo_path: PathBuf` (absolute canonical path).

**Add `workspace_slug()` function**: Computes a content-addressable slug from an absolute repo path: the full 16 hex characters of the `rapidhash` `u64` hash of the canonical path's raw bytes. The hash input is `path.as_os_str().as_bytes()` (via `std::os::unix::ffi::OsStrExt`), not a UTF-8 string conversion — this avoids collisions for paths that differ only in non-UTF-8 byte sequences (where `to_string_lossy()` would produce identical replacement characters). For example, `/home/user/repos/my-project` → `a1b2c3d4e5f6a7b8`. 16 hex chars = 64 bits, giving a birthday-bound collision probability of ~50% at ~4 billion repos. The sentinel file (below) catches any collision that does occur.

The slug contains no human-readable component. A name+hash hybrid (e.g., `my-project-a1b2c3d4`) was considered but rejected: repo directory names can contain non-UTF-8 bytes, characters invalid in filesystem paths on other platforms, or sequences that complicate slug parsing (e.g., embedded hyphens conflicting with the name-hash separator). Keeping the slug as pure hex avoids all sanitization and canonicalization edge cases — the sentinel file and `name()` provide human readability where needed. `ab info` and `ab dbg list` display the human-readable repo name via `name()`.

**Note on rapidhash stability:** The rapidhash algorithm has a fixed specification, so output should be stable across crate versions. If this assumption is ever violated, the sentinel-verified scan fallback (below) still finds the workspace — it just requires an explicit `ab dbg migrate` to restore the fast-path lookup.

**Sentinel file (`.source-repo`) — single source of truth**: When creating a slug directory (e.g., `{workspace_dir}/git/a1b2c3d4e5f6a7b8/`), write a `.source-repo` file containing the canonical path of the source repo followed by a trailing newline (`\n`). Read with `fs::read_to_string(...).trim_end_matches('\n')` for comparison — using `trim_end_matches('\n')` rather than `trim_end()` to avoid stripping trailing whitespace that may be part of the canonical path (rare but legal on Unix filesystems). Write atomically (write to a temp file in the same directory, then `fs::rename`) to avoid partial reads from concurrent `ab new` calls.

The sentinel file is the **authoritative** mapping from workspace directory to source repo. The hash slug is a fast-path optimization for directory naming and lookup, not the source of truth. This means:

- **On creation (`ab new`):** Compute the slug. If the computed slug dir already exists and its sentinel points to the same repo, proceed (idempotent). If it points to a different repo, error with a message naming both conflicting repo paths and a manual workaround:
  ```
  Error: workspace slug "a1b2c3d4e5f6a7b8" is already mapped to /home/user/other/my-repo
  (requested: /home/user/repos/my-repo). This is a hash collision.
  Workaround: manually edit .source-repo in {workspace_dir}/git/a1b2c3d4e5f6a7b8/
  or remove the conflicting workspace with 'ab dbg remove /home/user/other/my-repo'.
  Please report this at <issue tracker URL>.
  ```
  Then: create dir → write sentinel. **No scan of all slug dirs.** The "workspace exists under a different slug" case (from migration or hash algorithm change) is handled by `ab dbg migrate` and by the scan fallback on lookup (`ab spawn`). Avoiding the O(n) scan keeps `ab new` fast regardless of how many workspace directories exist.
- **On lookup (`ab spawn`):** compute slug → check if `{slug}/.source-repo` exists and matches the current repo path. If it matches, proceed. If the slug dir doesn't exist, or exists but the sentinel doesn't match (e.g., hash algorithm changed across crate versions), fall back to scanning all slug dirs under `workspace_dir/{type}/`, reading each `.source-repo`, and finding the one that matches. When the scan fallback finds a match, **use the found path directly without renaming** — this avoids breaking active sessions that have files open under the old slug directory. Log a one-time suggestion: `"Workspace found at legacy slug {old_slug}; run 'ab dbg migrate' to update."` The degraded state (repeated scan on each lookup) persists until the user explicitly runs `ab dbg migrate`, which is safe to run when no sessions are active.
- **On listing (`ab dbg list`):** scan all slug dirs, read sentinels, display all workspaces with status. `--orphans` filters to only those where the source path no longer exists on disk or where no sentinel exists (old-layout).

The cost of sentinel-verified lookup is one `fs::read_to_string` per spawn (a few microseconds). The scan fallback is triggered if the hash-computed slug dir is absent or its sentinel doesn't match. This should effectively never happen in normal operation — only after migration or a hash algorithm change. When the scan fallback is hit, the found path is used directly (no automatic rename) to avoid disrupting active sessions. The degraded state persists until the user runs `ab dbg migrate` explicitly.

**Methods to change:**
- `from_repo_path(config, full_path)` -> `from_path(full_path)`: canonicalize and store. No config needed. This is the single canonicalization point — callers must not pre-canonicalize.
- `source_path(&self, config)` -> `source_path(&self)`: returns `&self.repo_path`. No config needed.
- `relative_path()` -> `name()`: returns `Cow<str>` via `repo_path.file_name()` + `to_string_lossy()`. If `file_name()` returns `None` (e.g., path is `/`), return `Cow::Borrowed("(unknown)")` — `/` is never a valid repo root, and `gix::discover()` would fail before reaching this point anyway. Using `to_string_lossy()` instead of a fallible `to_str()` makes `name()` infallible, which is appropriate since it's only used for display labels (e.g., the `ab info` header). Non-UTF-8 repo directory names get a lossy representation with replacement characters, which is acceptable for a display-only value.
- **Display convention:** All user-facing messages that identify a repo should include the full canonical path from `source_path()` — e.g., `'/home/user/repos/my-project'`. `name()` is only used where a short label is needed alongside the full path (e.g., the `ab info` header). Never use `name()` alone in error messages, since repos can share a basename (e.g., `/work/project` vs `/personal/project`).
- `git_workspace_path`: use `workspace_slug()` (content-addressable 16-hex-char hash) instead of `self.relative_path` in path construction. Still needs config for `workspace_dir`. Verify via sentinel (see below).
- `jj_workspace_path`: same.
- `workspace_path`: delegates to `git_workspace_path`/`jj_workspace_path` — no signature change needed beyond what those two require.

`git_workspace_path` and `jj_workspace_path` use the hash-computed path directly (for creation). For lookup, callers use `resolve_workspace_dir()` in `repo.rs` (see Section 3).
- `jj_workspaces(config)` -> `jj_workspaces()`: calls `self.source_path()` (no config).
- `git_worktrees(config)` -> `git_worktrees()`: same.

**Remove:**
- `find_matching()`, `discover_repo_ids()`, `discover_repos_in_dir()` -- no global discovery.
- `calculate_relative_path()` -- no longer needed. Note: this is currently a public function; removal is a breaking API change (acceptable since project is young).

**Tests:** Remove `test_find_matching_*` (4 tests). Rewrite `test_repo_identifier_from_repo_path` and `test_repo_identifier_path_builders`. Add tests for `workspace_slug` (uniqueness, determinism, format — 16 hex chars, no human-readable component). Add test for `name()` returning lossy representation for non-UTF-8 directory names (use `OsStr::from_bytes` on Linux to construct one). Sentinel and scan fallback tests belong in `repo.rs` alongside `resolve_workspace_dir()` — see Section 3.

### 3. `common/src/repo.rs` -- Simplify repo resolution

**Remove:** `locate_repo()` (line 44-67), `prompt_select_repo()` (line 27-41).

**`find_git_root()` generalization:** Refactor `find_git_root()` (line 9-24) into `find_git_root_from(path: &Path) -> Result<PathBuf>` that runs `gix::discover()` from the given path, then resolves to the **main repository root** via `common_dir()`. Redefine `find_git_root()` as `find_git_root_from(&std::env::current_dir()?)`. This is used by both the CWD and explicit-path code paths in `resolve_repo_id()`.

**Linked worktree resolution:** The current `find_git_root()` uses `repo.workdir()`, which returns the *linked worktree* directory when called from inside a session workspace — not the main repo root. This is the same bug fixed in `display.rs` (Section 4). Fix: compare `git_dir()` and `common_dir()` to detect linked worktrees specifically. In a linked worktree, `git_dir()` points to `.git/worktrees/<name>` while `common_dir()` points to the shared `.git` dir — they differ. In a normal repo or submodule, they are equal. This distinction is important: unconditionally using `common_dir().parent()` would break submodules, where `common_dir()` is something like `/parent-repo/.git/modules/my-sub` (parent gives the wrong path).
```rust
fn find_git_root_from(path: &Path) -> Result<PathBuf> {
    let repo = gix::discover(path).wrap_err_with(|| {
        format!("Failed to discover git repository in {}", path.display())
    })?;
    if repo.git_dir() != repo.common_dir() {
        // Linked worktree: git_dir is .git/worktrees/<name>, common_dir is .git
        // Resolve to main repo root via common_dir's parent.
        repo.common_dir()
            .parent()
            .ok_or_else(|| eyre::eyre!("common_dir has no parent: {}", repo.common_dir().display()))
            .map(|p| p.to_path_buf())
    } else {
        // Normal repo or submodule: use work_dir directly.
        repo.work_dir()
            .ok_or_else(|| eyre::eyre!("bare repository at {} has no working directory", repo.git_dir().display()))
            .map(|p| p.to_path_buf())
    }
}
```
This ensures `ab new` and `ab spawn` from inside a session workspace (linked worktree) resolve to the source repo, not the worktree itself. Without this fix, the worktree path would be canonicalized and hashed to a different slug than the main repo, causing `ab new` to create a workspace-of-a-workspace and `ab spawn` to fail to find the existing workspace. The `git_dir() != common_dir()` check also correctly handles submodules (where both point to the same location under `.git/modules/`), avoiding the bug that unconditional `common_dir().parent()` would introduce.

**`resolve_repo_id()`** (line 69-82): Change `repo_name: Option<&str>` to `repo_path: Option<&Path>`. In both cases, discover the git/jj root via `gix::discover()` — when a path is provided, discover from that path; when `None`, discover from CWD. This ensures that passing a subdirectory (e.g., `ab new ~/repos/foo/src/`) correctly resolves to the repo root, matching the CWD behavior. No config needed. Note: `gix::discover()` works for colocated jj repos (they have `.git`). Non-colocated jj repos are not supported elsewhere in the codebase (`create_jj_workspace` requires `.jj` alongside `.git`), so this is fine.
```rust
pub fn resolve_repo_id(repo_path: Option<&Path>) -> Result<RepoIdentifier> {
    let root = match repo_path {
        Some(path) => find_git_root_from(path)?,
        None => find_git_root()?,
    };
    // from_path handles canonicalization — don't pre-canonicalize here.
    RepoIdentifier::from_path(&root)
}
```
This requires a new `find_git_root_from(path: &Path)` helper (or generalizing `find_git_root()` to accept an optional starting path). The existing `find_git_root()` becomes `find_git_root_from(&std::env::current_dir()?)`. Both use `gix::discover()` internally.

**Cleanup:** Remove the stale `println!("debug: {repo_id:?}")` at line 80.

**Add `resolve_workspace_dir()` free function**: This lives in `repo.rs` (not on `RepoIdentifier`) to keep `RepoIdentifier` as a simple value type and co-locate workspace resolution logic with workspace creation logic. Signature: `resolve_workspace_dir(workspace_dir: &Path, wtype: WorkspaceType, repo_id: &RepoIdentifier) -> Result<Option<PathBuf>>`. Returns `Ok(Some(path))` if an existing workspace is found, `Ok(None)` otherwise, `Err` on I/O failures other than not-found:
1. Compute slug via `workspace_slug()` on `repo_id.source_path()`.
2. Check if `{workspace_dir}/{type}/{slug}/.source-repo` exists and contains the repo path.
3. If yes → return `Ok(Some({workspace_dir}/{type}/{slug}/))`.
4. Otherwise (slug dir absent, or sentinel missing/mismatched) → scan all dirs under `{workspace_dir}/{type}/`, read each `.source-repo`. If found, **use the found path directly without renaming** — return `Ok(Some(found_path))` and log a suggestion: `"Workspace found at legacy slug {old_slug}; run 'ab dbg migrate' to update."` No automatic rename is performed, to avoid breaking active sessions that may have files open under the old slug directory.
5. If no match found anywhere → return `Ok(None)`.

Callers decide the semantics: `ab new` treats `None` as "create at hash-computed path"; `ab spawn` treats `None` as an error ("workspace not found, run `ab new` first").

**`new_workspace()`** (line 85-128): Change `repo_name: Option<&str>` to `repo_path: Option<&Path>`. Pass through to `resolve_repo_id()`. Compute workspace path using the hash-computed slug (no scan for existing workspaces under different slugs — that case is handled by `ab dbg migrate` and by the scan fallback in `resolve_workspace_dir()` on lookup). Update `source_path()` calls to drop config where applicable. Still needs config for `workspace_dir`.

**`create_git_worktree()`** (line 177-224): Update `repo_id.source_path()` (drop config), keep config for workspace path. Write `.source-repo` sentinel file atomically (write temp file, then `fs::rename`) to the slug directory after creating the parent directory. Sentinel format: canonical path followed by `\n`.

**`create_jj_workspace()`** (line 131-174): Same pattern (including atomic sentinel file write with trailing `\n`).

**`remove_repo()`** (line 266-318): Call `resolve_workspace_dir(workspace_dir, wtype, &repo_id)` to find the actual workspace directory for each type (git/jj), handling mismatched slugs from migration. Display `repo_id.source_path()` instead of `repo_id.relative_path().display()` (full canonical path in all user-facing output).

**Integration test:** Add an automated test for the full `new_workspace` → `resolve_workspace_dir` round-trip. Create a temp directory with a real git repo (`git init`), call `new_workspace` to create a workspace with the new content-addressable slug layout, then verify: (1) slug directory exists with a 16-hex-char name, (2) `.source-repo` sentinel file contains the correct canonical path (with trailing newline), (3) `resolve_workspace_dir()` (the free function in `repo.rs`) finds the workspace via the fast path (slug match). Add a second test for the scan fallback path: create a workspace dir with a fake slug and valid `.source-repo`, then call `resolve_workspace_dir()` and verify the workspace is found at the old slug path (no rename occurs — the found path is returned as-is).

### 4. `common/src/display.rs` -- Update info command

**Fix worktree resolution:** The current code uses `gix::discover(&cwd)` followed by `workdir()`, which returns the *linked worktree* directory when run from inside a session workspace — not the source repo. Replace this with `find_git_root_from(&cwd)?` from `repo.rs`, which uses the `git_dir() != common_dir()` check to detect linked worktrees and resolve to the main repo root (see Section 3). This ensures `ab info` shows correct results regardless of whether the user is in the source repo or a session workspace. Add a test that runs `ab info` logic from inside a linked worktree path and verifies it resolves to the main repo.

Line 29: `RepoIdentifier::from_repo_path(config, &repo_path)` -> `RepoIdentifier::from_path(&repo_path)` (using the resolved main repo path).
Line 33: `repo_id.git_worktrees(config)` -> `repo_id.git_worktrees()`.
Line 62: `repo_id.jj_workspaces(config)` -> `repo_id.jj_workspaces()`.

Keep `config` in the `info()` function signature for forward-compatibility (e.g., displaying workspace paths in the future would need `config.workspace_dir`).

**Add repo identity header**: Add a line at the top of `info()` output showing the repo name and path, e.g.:
```
Repository: my-repo (/home/user/path/to/my-repo)
```
Use `repo_id.name()` and `repo_id.source_path()` to populate this.

### 5. `ab/src/main.rs` -- CLI changes

**`Commands::New`** (line 27-39): Change `repo_name: Option<String>` to `repo_path: Option<PathBuf>` (positional arg is now a filesystem path, not a search string). Call `new_workspace(&config, repo_path.as_deref(), session.as_deref(), workspace_type)`. When `gix::discover()` fails for a user-provided path argument, always include a hint in the error — no heuristic needed to detect "bare name" vs. path, since the hint is helpful in all failure cases:
```
Error: could not find a git repository at 'my-project'
Hint: use 'cd my-project && ab new' or pass a full path like 'ab new /path/to/my-project'
```

**`Commands::Spawn`** (line 54-55): Change `--repo` from `Option<String>` to `Option<PathBuf>` (a filesystem path).

**`Commands::Spawn` local mode** (line 225-236): In local mode, `RepoIdentifier` is not needed — the workspace path *is* the source path. Replace the `locate_repo` call: if `--repo` is provided, discover the git root from that path via `find_git_root_from(path)`. Otherwise call `find_git_root()` (discovers from CWD). This is consistent with session mode — explicit paths always resolve to the repo root, not the literal path provided. The result is used as both `workspace_path` and `source_path` without constructing a `RepoIdentifier`.

**`Commands::Spawn` session mode** (line 237-253): Pass `repo.as_deref()` as `Option<&Path>` to `resolve_repo_id()` and `new_workspace()`. When `resolve_workspace_dir()` returns `None` (workspace not found), before erroring, scan sentinels for orphaned workspaces whose source path shares the same `file_name()` as the current repo. If any are found, include them in the error message as a hint:
```
Error: no workspace found for '/home/user/repos/my-project-v2'
Note: found orphaned workspace for '/home/user/repos/my-project' — same repo?
  Run 'ab dbg list a1b2c3d4e5f6a7b8' to inspect, or manually update
  .source-repo in ~/.agent-box/workspaces/git/a1b2c3d4e5f6a7b8/
```
This catches the common case of a repo being moved or renamed.

**`DbgCommands::Locate`** (line 118-121): Remove entirely.

**`DbgCommands::Migrate`** (new): Migrates old-layout workspace directories to the new content-addressable slug format. See Section 11 for details. Flags: `--dry-run` (preview only), `--clean-orphans` (remove old-layout dirs whose source repos no longer exist), `--base-repo-dir <path>` (override for the deprecated config field — allows migration without restoring the field in the config file; falls back to config value if not provided).

**`DbgCommands::List`** (new): Scans all immediate subdirectories of `workspace_dir/{git,jj}/`, reads each `.source-repo` sentinel, and displays all workspaces with a status column. For each workspace directory, shows:
- **Status**: `healthy` (sentinel exists, source repo on disk), `orphaned` (sentinel exists, source path gone), or `old-layout` (no sentinel file)
- The repo name (via `file_name()` of the source path, or `(unknown)` for old-layout)
- The full canonical source path (from sentinel, or `(no sentinel)` for old-layout)
- The slug
- The workspace type (git/jj)
- Number of sessions (count of subdirectories under the slug dir, excluding `.source-repo`)

Output is sorted by status (healthy first, then orphaned, then old-layout), then by repo name. Example:
```
healthy     agent-box    /home/user/repos/agent-box       a1b2c3d4e5f6a7b8  git  3 sessions
healthy     my-project   /home/user/work/my-project       c9d0e1f2a3b4c5d6  git  1 session
orphaned    old-repo     /home/user/repos/old-repo        e7f8a9b0c1d2e3f4  git  2 sessions
old-layout  (unknown)    (no sentinel)                    my-project         git  1 session
```

Flags:
- `--orphans`: Filter to only orphaned and old-layout workspaces.

Accepts an optional positional `slug: String` argument — when provided, shows detailed info for that slug only: `.source-repo` contents, session directories, and whether the source repo still exists on disk. This provides both the "which repos have workspaces?" discoverability (lost when global repo discovery under `base_repo_dir` was removed) and orphan inspection in a single read-only command.

**`DbgCommands::Remove`** (line 123-132): Change `repo: String` to `repo_path: Option<PathBuf>`. Two modes:

1. **Repo removal** (default): When `repo_path` is provided, pass to `RepoIdentifier::from_path()`, call `resolve_workspace_dir()` (the free function in `repo.rs`) to find the actual workspace directory (handles mismatched slugs), remove. When not provided, detect repo from CWD.
2. **Orphan cleanup** (`--orphans`): Scan all workspace directories, find orphaned (sentinel points to non-existent path) and old-layout (no sentinel) directories, and remove them. Accepts an optional `--slug <slug>` to target a specific orphaned workspace directory instead of all orphans.

Shared flags:
- `--force`: Skip interactive confirmation.
- `--dry-run`: Preview what would be deleted without acting.

Without `--force`, both modes show what will be deleted and prompt for interactive confirmation. This gives `list` and `remove` a clean read/write split: `list` shows things (with `--orphans` to filter), `remove` deletes things (with `--orphans` to target broken workspaces).

**Imports** (line 7): Remove `locate_repo` from imports.

### 6. `common/src/config.rs` -- Config struct

Line 583: Change `pub base_repo_dir: PathBuf` to:
```rust
#[serde(default)]
pub base_repo_dir: Option<PathBuf>,
```

In `load_config()` (around line 822-823): Remove `expand_path` call for `base_repo_dir`. Add deprecation warning:
```rust
if config.base_repo_dir.is_some() {
    eprintln!("Warning: 'base_repo_dir' is deprecated. Run 'ab dbg migrate' to migrate existing workspaces, then remove it from your config.");
}
```

**Tests and fixtures (~55 references across the project):** Remove `base_repo_dir` from all test TOML strings and `Config` struct literals. This spans config.rs (~23 references), path.rs (~12), runtime/mod.rs (3), and portal/tests (2), plus docs and schema. Keep one test that verifies the deprecation path (field present but ignored).

### 7. `common/config.schema.json` -- Regenerate

The schema is auto-generated from the `Config` struct via `JsonSchema` derive (see `common/src/bin/generate_schema.rs`). After changing the struct in step 6, regenerate the schema rather than hand-editing:
```bash
cargo run --bin generate_schema > common/config.schema.json
```
This will automatically remove `base_repo_dir` from `properties` and `required`.

### 8. `ab/src/runtime/mod.rs` -- Test fixtures

Lines 1322, 1415, 1490: Change `base_repo_dir: PathBuf::from("/repos")` to `base_repo_dir: None`.

### 9. `portal/tests/host_integration.rs`

Line 23: Remove `base_repo_dir` from test config TOML. Line 41: Remove `fs::create_dir_all(home.join("repos"))`.

### 10. Documentation

- `docs/src/reference/agent-box/config.md` line 21: Remove `base_repo_dir` entry, add deprecation note.
- `docs/src/tutorials/agent-box/first-run.md` line 11: Remove `base_repo_dir` from example config. Change `ab new myrepo` to `cd myrepo && ab new`.
- `docs/src/explanation/architecture/agent-box-overview.md` line 7: "Detect repository root from working directory".
- `docs/src/explanation/architecture/agent-box-workflow.md` line 7: "Source repositories are detected from the current working directory."

### 11. Migration / breaking changes

**Workspace path layout change:** Workspace paths change from `{workspace_dir}/git/{relative_path}/{session}` to `{workspace_dir}/git/{hash16}/{session}`. Existing workspaces created under the old layout will not be found or cleaned up by the new code.

**Old-layout and orphan detection:** No warnings on the hot path (`ab new`, `ab spawn`). Old-layout directories and orphaned workspaces are surfaced via `ab dbg list` (all workspaces with status) or `ab dbg list --orphans` (only broken ones). Users can clean them up with `ab dbg remove --orphans` (all orphans) or `ab dbg remove --orphans --slug <slug>` (a specific slug).

**`ab spawn` error when workspace doesn't exist:** If `ab spawn -s session --git` is called and `resolve_workspace_dir()` returns `None` (no workspace found for this repo), produce a clear error message including the full canonical repo path (via `source_path()`) and session name. Suggest running `ab new` first. Additionally, scan sentinels for orphaned workspaces with matching `file_name()` and include them as a hint if found — this catches the common case of a repo move/rename and points the user toward the orphaned workspace (see Section 5, `Commands::Spawn` session mode).

**Migration subcommand (`ab dbg migrate`):** A compiled Rust subcommand that migrates old-layout workspaces to the new content-addressable slug format. Using Rust (not a shell script) ensures the slug computation uses the actual `rapidhash` implementation, eliminating hash divergence. The subcommand:
1. Reads `base_repo_dir` from the `--base-repo-dir` CLI flag, falling back to the config file value. Expand the path via `expand_path()` (handles `~` and relative paths) regardless of source — the config value is no longer expanded by `load_config()` since the field is now `Option<PathBuf>`. If neither CLI flag nor config value is set, error with a message explaining that the old base path is needed for migration. Reads `workspace_dir` from the config file.
2. Walks the workspace tree under `workspace_dir/{git,jj}/` looking for git worktree markers (`.git` *file*, not directory) and jj workspace markers (`.jj/working_copy/`) to identify session directories. Each session's parent directory is the old-layout repo directory, and the relative path between `workspace_dir/{type}/` and that parent is the old `relative_path`. This correctly handles multi-component relative paths (e.g., `work/project`) that create nested directory structures under the old layout. Directories that already contain a `.source-repo` sentinel are skipped (already migrated).
3. For each discovered old-layout repo directory, reconstructs the source repo path as `base_repo_dir / relative_path`.
4. Verifies the source repo still exists on disk. If not, reports it as orphaned and skips (or deletes with `--clean-orphans`).
5. Computes the new slug via `workspace_slug()` (the same `rapidhash`-based function used by all other code paths).
6. Renames the directory to the new slug name. Handles `ENOENT` on rename gracefully (concurrent migration). If the target slug already exists with a `.source-repo` pointing to a different repo, this is a hash collision: report both conflicting paths, skip this repo, and continue migrating the rest. (Same collision error as `ab new` — see Section 2.)
7. Writes the `.source-repo` sentinel file (canonical path + trailing `\n`, written atomically).
8. Supports `--dry-run` to preview changes without acting.

Since the subcommand uses the same hash implementation as the rest of the codebase, migrated workspaces are immediately on the fast path — no scan fallback needed on first spawn.

**Partial failure safety:** Migration is idempotent and safe to re-run after interruption (e.g., power loss, disk full). Each workspace is migrated independently: already-migrated dirs have a `.source-repo` sentinel and are skipped. If the process crashes midway, re-running `ab dbg migrate` picks up where it left off. No journal or transaction log is needed.

The subcommand is removed from the codebase in a follow-up release once the migration window has passed.

**Action:** Add a "Breaking Changes" section to the CHANGELOG/README:

1. **Before upgrading (recommended):** Back up your `workspace_dir` (e.g., `cp -a ~/.agent-box/workspaces ~/.agent-box/workspaces.bak`), then run `ab dbg migrate --dry-run` to preview migration, then `ab dbg migrate` to migrate. Ensure no active sessions are running during migration. If `base_repo_dir` has already been removed from the config, pass it via `--base-repo-dir <path>`. Old-layout workspaces will be renamed to the new content-addressable slug format and sentinel files written.
2. **After upgrading (alternative):** If you have no active sessions worth preserving, simply delete `workspace_dir/git/` and `workspace_dir/jj/` directories and recreate workspaces with `ab new`.

Additional notes:
- `base_repo_dir` config field is deprecated and ignored. It can be removed from config files.
- `--repo` flag on `ab spawn` and positional `repo_name` on `ab new` now accept a filesystem path (not a search string). The fuzzy repo search (`ab new agent-box` matching repos under `base_repo_dir`) is removed with no replacement — users should `cd` into the repo first (the common case) or pass an explicit path. Shell completion and tools like `zoxide` make explicit paths ergonomic. This is an intentional simplification: CWD detection covers the primary workflow, and removing the search avoids the need for a configured scan directory.
- **Repo moves/renames:** If a repository is moved or renamed after workspaces are created, the canonical path changes and the old workspace slug becomes orphaned. The sentinel file makes this detectable (`.source-repo` will point to a non-existent path). When `ab spawn` fails to find a workspace, it scans for orphans with matching `file_name()` and includes them in the error as a hint. Users can discover all orphans with `ab dbg list --orphans` and clean them up with `ab dbg remove --orphans` (all) or `ab dbg remove --orphans --slug <slug>` (specific slug).

## Dependency order

1. Add `rapidhash` to Cargo.toml (no deps)
2. `workspace_slug()` in path.rs (depends on 1)
3. `RepoIdentifier` redesign in path.rs (depends on 2)
4. Config struct change in config.rs + schema regeneration (no deps, parallel with 1-3)
5. repo.rs updates incl. `find_git_root_from()`, `resolve_workspace_dir()`, sentinel file logic, integration test (depends on 3)
6. display.rs updates (depends on 3)
7. main.rs CLI changes incl. `DbgCommands::Migrate`, `List`, and `Remove` (depends on 4, 5)
8. Test fixtures (depends on all above)
9. Documentation (depends on all above)
10. Migration subcommand `ab dbg migrate` (depends on finalized slug format from 2 and CLI structure from 7, can be written in parallel with 5-9)

## Verification

1. `cargo check --workspace` -- compiles
2. `cargo clippy --workspace --all-targets -- -D warnings` -- no warnings
3. `cargo test --workspace` -- all tests pass (including new integration test for `new_workspace` → `resolve_workspace_dir` round-trip)
4. `cargo fmt --all -- --check` -- formatted
5. Manual test: `cd` into a repo, run `ab new -s test --git`, then `ab spawn -s test --git`
6. Manual test: `ab new /path/to/repo/src/ -s test --git` (explicit subdirectory path — should resolve to repo root)
7. Manual test: verify old config with `base_repo_dir` prints deprecation warning but works
8. Manual test: verify sentinel file is created and collision detection works
9. Manual test: `ab dbg list` shows all workspaces with correct status (healthy/orphaned/old-layout)
10. Manual test: `ab dbg list --orphans` filters to only orphaned and old-layout workspaces
11. Manual test: `ab dbg list <slug>` shows detailed info for a specific slug
12. Manual test: `ab dbg remove --orphans` cleans up all orphaned workspaces (with confirmation prompt)
13. Manual test: `ab dbg remove --orphans --slug <slug>` removes a specific orphaned slug directory (with confirmation)
14. Manual test: `ab dbg remove` with no args detects repo from CWD and prompts for confirmation
15. Manual test: `ab spawn -s nonexistent --git` produces clear error with suggestion
16. Manual test: sentinel-verified lookup — rename a slug directory manually, verify `ab spawn` finds it via scan fallback (uses found path directly, logs suggestion to run `ab dbg migrate`), and verify no automatic rename occurs
17. Manual test: `ab dbg migrate --dry-run` on an old-layout workspace directory previews correct migration
18. Manual test: `ab dbg migrate` migrates old-layout workspaces to content-addressable slugs, writes sentinel files, and `ab spawn` finds them via fast path (no scan fallback needed)
19. Manual test: `ab dbg migrate --base-repo-dir /path/to/repos` works when `base_repo_dir` has been removed from config
20. Manual test: `ab dbg migrate` with two old-layout repos that hash-collide — verify first migrates, second reports collision and is skipped
21. Manual test: move a repo, run `ab spawn`, verify error message includes hint about orphaned workspace with matching name
22. Manual test: `cd` into a session workspace (linked worktree), run `ab info`, verify it shows the source repo's workspaces
23. Manual test: `cd` into a session workspace (linked worktree), run `ab new -s another --git`, verify it creates a workspace for the source repo (not for the worktree itself)
24. Manual test: `cd` into a session workspace (linked worktree), run `ab spawn -s <session> --git`, verify it resolves the source repo and finds the existing workspace
25. Manual test: `ab dbg migrate` correctly handles old-layout workspaces with multi-component relative paths (e.g., `work/project`)
26. Manual test: `ab new my-project` (bare name, not a valid repo path) produces helpful hint
