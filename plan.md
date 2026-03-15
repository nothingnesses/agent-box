# Plan: Remove `base_repo_dir` from agent-box

## Context

[**GitHub issue draft**](./github-issue.md)

**Problem:** Session/worktree mode in agent-box requires all repositories to be under a single `base_repo_dir` config field. This is limiting because repositories are often spread across multiple locations, and the requirement adds upfront configuration before the tool can be used. Workarounds don't hold up:

- `--local` mode bypasses the check, but loses worktree sandboxing.
- Setting `base_repo_dir` to a common ancestor like `~` slows discovery by scanning the entire home directory.
- Symlinking repos into `base_repo_dir` doesn't work because agent-box resolves symlinks to their real paths, which then fall outside `base_repo_dir`.

**Goal:** Remove `base_repo_dir` entirely. `ab new` will detect the git/jj root from the current directory. Workspace paths will use a content-addressable hash of the full canonical path (e.g., `{workspace_dir}/git/a1b2c3d4e5f6a7b8/{session}`), with a `.source-repo` sentinel file providing the human-readable mapping.

## Changes

### 0. `Cargo.toml` -- Add rapidhash dependency

Add `rapidhash = "4.4"` to workspace dependencies in the root `Cargo.toml` and to `common/Cargo.toml`. (Version 4.4 has been verified to exist on crates.io.)

### 1. `common/src/path.rs` -- RepoIdentifier redesign

**RepoIdentifier struct** (line 35-39): Change `relative_path: PathBuf` to `repo_path: PathBuf` (absolute canonical path).

**Add `workspace_slug()` function**: Computes a content-addressable slug from an absolute repo path: the full 16 hex characters of the `rapidhash` `u64` hash of the canonical path string. For example, `/home/user/repos/my-project` → `a1b2c3d4e5f6a7b8`. 16 hex chars = 64 bits, giving a birthday-bound collision probability of ~50% at ~4 billion repos. The sentinel file (below) catches any collision that does occur.

The slug contains no human-readable component. A name+hash hybrid (e.g., `my-project-a1b2c3d4`) was considered but rejected: repo directory names can contain non-UTF-8 bytes, characters invalid in filesystem paths on other platforms, or sequences that complicate slug parsing (e.g., embedded hyphens conflicting with the name-hash separator). Keeping the slug as pure hex avoids all sanitization and canonicalization edge cases — the sentinel file and `name()` provide human readability where needed. `ab info` and `ab dbg orphans` display the human-readable repo name via `name()`.

**Note on rapidhash stability:** The rapidhash algorithm has a fixed specification, so output should be stable across crate versions. The sentinel-verified fallback (below) provides self-healing if this assumption is ever violated.

**Sentinel file (`.source-repo`) — single source of truth**: When creating a slug directory (e.g., `{workspace_dir}/git/a1b2c3d4e5f6a7b8/`), write a `.source-repo` file containing the canonical path of the source repo followed by a trailing newline (`\n`). Read with `fs::read_to_string(...).trim_end()` for comparison. Write atomically (write to a temp file in the same directory, then `fs::rename`) to avoid partial reads from concurrent `ab new` calls.

The sentinel file is the **authoritative** mapping from workspace directory to source repo. The hash slug is a fast-path optimization for directory naming and lookup, not the source of truth. This means:

- **On creation (`ab new`):** Before creating, scan all slug dirs under `{workspace_dir}/{type}/` reading each `.source-repo` to check if a workspace for this repo already exists under a different slug. If found under a different slug (e.g., from migration or a hash algorithm change), atomically rename the slug dir to the current hash, then proceed. Handle `ENOENT` on rename gracefully (another concurrent `ab new` may have already renamed it) — if the rename fails because the source is gone, re-check whether the destination slug dir now exists with the correct sentinel, and proceed if so. If the computed slug dir already exists and its sentinel points to the same repo, proceed (idempotent). If it points to a different repo, error with a message naming both conflicting repo paths:
  ```
  Error: workspace slug "a1b2c3d4e5f6a7b8" is already mapped to /home/user/other/my-repo
  (requested: /home/user/repos/my-repo). This is a hash collision.
  Please report this at <issue tracker URL>.
  ```
  Then: create dir → write sentinel.
- **On lookup (`ab spawn`):** compute slug → check if `{slug}/.source-repo` exists and matches the current repo path. If it matches, proceed. If the slug dir doesn't exist, or exists but the sentinel doesn't match (e.g., hash algorithm changed across crate versions), fall back to scanning all slug dirs under `workspace_dir/{type}/`, reading each `.source-repo`, and finding the one that matches. When the scan fallback finds a match, **auto-heal**: atomically rename the old slug dir to the current hash and update the sentinel, so subsequent lookups use the fast path. Handle `ENOENT` on rename (concurrent heal) the same way as in creation. This makes hash algorithm changes fully self-healing — the degraded state is transient and fixes itself on first access, with no user intervention required.
- **On orphan detection (`ab dbg orphans`):** scan all slug dirs, read sentinels, report any where the source path no longer exists on disk or where no sentinel exists (old-layout).

The cost of sentinel-verified lookup is one `fs::read_to_string` per spawn (a few microseconds). The scan fallback is triggered if the hash-computed slug dir is absent or its sentinel doesn't match. This should effectively never happen in normal operation — only after migration or a hash algorithm change — and auto-heals on first access by renaming the slug dir to the current hash, so subsequent lookups use the fast path with no user intervention required.

**Methods to change:**
- `from_repo_path(config, full_path)` -> `from_path(full_path)`: canonicalize and store. No config needed. This is the single canonicalization point — callers must not pre-canonicalize.
- `source_path(&self, config)` -> `source_path(&self)`: returns `&self.repo_path`. No config needed.
- `relative_path()` -> `name()`: returns `repo_path.file_name()` as `&str` via `to_str()`. If `file_name()` returns `None` (e.g., path is `/`), return an error — `/` is never a valid repo root, and `gix::discover()` would fail before reaching this point anyway. If `to_str()` returns `None` (non-UTF-8 directory name), return an error with a descriptive message including the lossy representation of the path — non-UTF-8 repo directory names are pathological on systems where this tool runs.
- **Display convention:** All user-facing messages that identify a repo should include the full canonical path from `source_path()` — e.g., `'/home/user/repos/my-project'`. `name()` is only used where a short label is needed alongside the full path (e.g., the `ab info` header). Never use `name()` alone in error messages, since repos can share a basename (e.g., `/work/project` vs `/personal/project`).
- `git_workspace_path`: use `workspace_slug()` (content-addressable 16-hex-char hash) instead of `self.relative_path` in path construction. Still needs config for `workspace_dir`. Verify via sentinel (see below).
- `jj_workspace_path`: same.
- `workspace_path`: delegates to `git_workspace_path`/`jj_workspace_path` — no signature change needed beyond what those two require.

**Add `resolve_workspace_dir()` helper**: Given a `workspace_dir`, workspace type (`git`/`jj`), and `repo_path`, returns `Option<PathBuf>` — `Some` if an existing workspace is found, `None` otherwise:
1. Compute slug via `workspace_slug()`.
2. Check if `{workspace_dir}/{type}/{slug}/.source-repo` exists and contains `self.repo_path`.
3. If yes → return `Some({workspace_dir}/{type}/{slug}/)`.
4. Otherwise (slug dir absent, or sentinel missing/mismatched) → scan all dirs under `{workspace_dir}/{type}/`, read each `.source-repo`. If found, **auto-heal**: atomically rename the old slug dir to the current hash (handle `ENOENT` for concurrent heal), return `Some(new_path)`.
5. If no match found anywhere → return `None`.

Callers decide the semantics: `ab new` treats `None` as "create at hash-computed path"; `ab spawn` treats `None` as an error ("workspace not found, run `ab new` first").

`git_workspace_path` and `jj_workspace_path` delegate to this helper (for lookup) or use the hash-computed path directly (for creation).
- `jj_workspaces(config)` -> `jj_workspaces()`: calls `self.source_path()` (no config).
- `git_worktrees(config)` -> `git_worktrees()`: same.

**Remove:**
- `find_matching()`, `discover_repo_ids()`, `discover_repos_in_dir()` -- no global discovery.
- `calculate_relative_path()` -- no longer needed. Note: this is currently a public function; removal is a breaking API change (acceptable since project is young).

**Tests:** Remove `test_find_matching_*` (4 tests). Rewrite `test_repo_identifier_from_repo_path` and `test_repo_identifier_path_builders`. Add tests for `workspace_slug` (uniqueness, determinism, format — 16 hex chars, no human-readable component). Add test for sentinel file collision detection. Add test for sentinel-verified lookup (matching sentinel). Add test for scan fallback — both cases: (a) slug dir exists but sentinel points to different path, (b) slug dir absent but another slug dir has matching sentinel. Add test for `ab new` rename-on-find (existing workspace under different slug gets renamed). Add test for `ENOENT` handling during concurrent rename (source already gone, destination exists with correct sentinel — should succeed). Add test for auto-heal during lookup (scan fallback renames slug dir, subsequent lookup uses fast path). Add test for `name()` returning an error on non-UTF-8 directory names (use `OsStr::from_bytes` on Linux to construct one).

### 2. `common/src/repo.rs` -- Simplify repo resolution

**Remove:** `locate_repo()` (line 44-67), `prompt_select_repo()` (line 27-41).

**`find_git_root()` generalization:** Refactor `find_git_root()` (line 9-24) into `find_git_root_from(path: &Path) -> Result<PathBuf>` that runs `gix::discover()` from the given path. Redefine `find_git_root()` as `find_git_root_from(&std::env::current_dir()?)`. This is used by both the CWD and explicit-path code paths in `resolve_repo_id()`.

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

**`new_workspace()`** (line 85-128): Change `repo_name: Option<&str>` to `repo_path: Option<&Path>`. Pass through to `resolve_repo_id()`. Before creating, call `resolve_workspace_dir()` to check for an existing workspace under any slug. If found under a different slug, atomically rename the slug dir to the current hash (this converges migrated workspaces to the current hash scheme). Then compute workspace path using the (possibly newly renamed) slug dir. Update `source_path()` calls to drop config where applicable. Still needs config for `workspace_dir`.

**`create_git_worktree()`** (line 177-224): Update `repo_id.source_path()` (drop config), keep config for workspace path. Write `.source-repo` sentinel file atomically (write temp file, then `fs::rename`) to the slug directory after creating the parent directory. Sentinel format: canonical path followed by `\n`.

**`create_jj_workspace()`** (line 131-174): Same pattern (including atomic sentinel file write with trailing `\n`).

**`remove_repo()`** (line 266-318): Use `resolve_workspace_dir()` to find the actual workspace directory for each type (git/jj), handling mismatched slugs from migration. Display `repo_id.source_path()` instead of `repo_id.relative_path().display()` (full canonical path in all user-facing output).

**Integration test:** Add an automated test for the full `new_workspace` → `resolve_workspace_dir` round-trip. Create a temp directory with a real git repo (`git init`), call `new_workspace` to create a workspace with the new content-addressable slug layout, then verify: (1) slug directory exists with a 16-hex-char name, (2) `.source-repo` sentinel file contains the correct canonical path (with trailing newline), (3) `resolve_workspace_dir` finds the workspace via the fast path (slug match). Add a second test for the rename-on-find path: create a workspace dir with a fake slug and valid `.source-repo`, then call `new_workspace` and verify the old slug dir is renamed to the current hash.

### 3. `common/src/display.rs` -- Update info command

**Fix worktree resolution:** The current code uses `gix::discover(&cwd)` followed by `workdir()`, which returns the *linked worktree* directory when run from inside a session workspace — not the source repo. Fix this by resolving to the main repository: use `gix::Repository::common_dir()` to find the shared `.git` directory, then derive the main worktree path from its parent:
```rust
let repo = gix::discover(&cwd)?;
let common = repo.common_dir();
// common_dir() returns the shared .git dir (e.g., /home/user/my-project/.git)
// Its parent is the main repo root.
let main_repo_root = common.parent()
    .ok_or_else(|| eyre::eyre!("common_dir has no parent: {}", common.display()))?;
```
This ensures `ab info` shows correct results regardless of whether the user is in the source repo or a session workspace. Add a test that runs `ab info` logic from inside a linked worktree path and verifies it resolves to the main repo.

Line 29: `RepoIdentifier::from_repo_path(config, &repo_path)` -> `RepoIdentifier::from_path(&repo_path)` (using the resolved main repo path).
Line 33: `repo_id.git_worktrees(config)` -> `repo_id.git_worktrees()`.
Line 62: `repo_id.jj_workspaces(config)` -> `repo_id.jj_workspaces()`.

Keep `config` in the `info()` function signature for forward-compatibility (e.g., displaying workspace paths in the future would need `config.workspace_dir`).

**Add repo identity header**: Add a line at the top of `info()` output showing the repo name and path, e.g.:
```
Repository: my-repo (/home/user/path/to/my-repo)
```
Use `repo_id.name()` and `repo_id.source_path()` to populate this.

### 4. `ab/src/main.rs` -- CLI changes

**`Commands::New`** (line 27-39): Change `repo_name: Option<String>` to `repo_path: Option<PathBuf>` (positional arg is now a filesystem path, not a search string). Call `new_workspace(&config, repo_path.as_deref(), session.as_deref(), workspace_type)`. When `gix::discover()` fails for a provided path, check if the argument looks like a bare name (no path separator, doesn't exist as a path). If so, add a hint to the error:
```
Error: could not find a git repository at 'my-project'
Hint: repo name search was removed. Use 'cd my-project && ab new' or 'ab new /path/to/my-project' instead.
```
This catches users with muscle memory for the old `ab new <name>` behavior.

**`Commands::Spawn`** (line 54-55): Change `--repo` from `Option<String>` to `Option<PathBuf>` (a filesystem path).

**`Commands::Spawn` local mode** (line 225-236): In local mode, `RepoIdentifier` is not needed — the workspace path *is* the source path. Replace the `locate_repo` call: if `--repo` is provided, discover the git root from that path via `find_git_root_from(path)`. Otherwise call `find_git_root()` (discovers from CWD). This is consistent with session mode — explicit paths always resolve to the repo root, not the literal path provided. The result is used as both `workspace_path` and `source_path` without constructing a `RepoIdentifier`.

**`Commands::Spawn` session mode** (line 237-253): Pass `repo.as_deref()` as `Option<&Path>` to `resolve_repo_id()` and `new_workspace()`. When `resolve_workspace_dir()` returns `None` (workspace not found), before erroring, scan sentinels for orphaned workspaces whose source path shares the same `file_name()` as the current repo. If any are found, include them in the error message as a hint:
```
Error: no workspace found for '/home/user/repos/my-project-v2'
Note: found orphaned workspace for '/home/user/repos/my-project' — same repo?
  Run 'ab dbg orphans a1b2c3d4e5f6a7b8' to inspect, or manually update
  .source-repo in ~/.agent-box/workspaces/git/a1b2c3d4e5f6a7b8/
```
This catches the common case of a repo being moved or renamed.

**`DbgCommands::Locate`** (line 118-121): Remove entirely.

**`DbgCommands::Migrate`** (new): Migrates old-layout workspace directories to the new content-addressable slug format. See Section 10 for details. Flags: `--dry-run` (preview only), `--clean-orphans` (remove old-layout dirs whose source repos no longer exist), `--base-repo-dir <path>` (override for the deprecated config field — allows migration without restoring the field in the config file; falls back to config value if not provided).

**`DbgCommands::Orphans`** (new): Scans all immediate subdirectories of `workspace_dir/{git,jj}/`. For each:
- If no `.source-repo` file exists → report as **old-layout directory** (pre-migration).
- If `.source-repo` exists but the path it contains no longer exists on disk → report as **orphaned workspace**.
- Otherwise → skip (healthy).
Output lists orphans and old-layout dirs with their slug names and (where available) original source paths. Read-only — no deletions. Accepts an optional positional `slug: String` argument — when provided, shows detailed info for that slug only: `.source-repo` contents, session directories, and whether the source repo still exists on disk.

**`DbgCommands::Remove`** (line 123-132): Change `repo: String` to `repo_path: Option<PathBuf>`. When provided, pass to `RepoIdentifier::from_path()`, use `resolve_workspace_dir()` to find the actual workspace directory (handles mismatched slugs), remove. When not provided, detect repo from CWD. Require interactive confirmation unless `--force` is passed. Keep `--dry-run` flag as-is.

**`DbgCommands::Clean`** (new): When called with no arguments, run the same scan as `DbgCommands::Orphans`, remove all orphaned and old-layout directories. Show the full list before prompting for interactive confirmation unless `--force` is passed. Support `--dry-run`. When called with a positional `slug: String` argument, look up `{workspace_dir}/{git,jj}/<slug>/` directly and remove it — useful for orphans where the source path no longer exists and `Remove` can't resolve it. Require `--force` or interactive confirmation.

**Imports** (line 7): Remove `locate_repo` from imports.

### 5. `common/src/config.rs` -- Config struct

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

### 6. `common/config.schema.json` -- Regenerate

The schema is auto-generated from the `Config` struct via `JsonSchema` derive (see `common/src/bin/generate_schema.rs`). After changing the struct in step 5, regenerate the schema rather than hand-editing:
```bash
cargo run --bin generate_schema > common/config.schema.json
```
This will automatically remove `base_repo_dir` from `properties` and `required`.

### 7. `ab/src/runtime/mod.rs` -- Test fixtures

Lines 1322, 1415, 1490: Change `base_repo_dir: PathBuf::from("/repos")` to `base_repo_dir: None`.

### 8. `portal/tests/host_integration.rs`

Line 23: Remove `base_repo_dir` from test config TOML. Line 41: Remove `fs::create_dir_all(home.join("repos"))`.

### 9. Documentation

- `docs/src/reference/agent-box/config.md` line 21: Remove `base_repo_dir` entry, add deprecation note.
- `docs/src/tutorials/agent-box/first-run.md` line 11: Remove `base_repo_dir` from example config. Change `ab new myrepo` to `cd myrepo && ab new`.
- `docs/src/explanation/architecture/agent-box-overview.md` line 7: "Detect repository root from working directory".
- `docs/src/explanation/architecture/agent-box-workflow.md` line 7: "Source repositories are detected from the current working directory."

### 10. Migration / breaking changes

**Workspace path layout change:** Workspace paths change from `{workspace_dir}/git/{relative_path}/{session}` to `{workspace_dir}/git/{hash16}/{session}`. Existing workspaces created under the old layout will not be found or cleaned up by the new code.

**Old-layout and orphan detection:** No warnings on the hot path (`ab new`, `ab spawn`). Old-layout directories and orphaned workspaces are surfaced exclusively via `ab dbg orphans`. Users can clean them up with `ab dbg clean` (all orphans) or `ab dbg clean <slug>` (a specific slug).

**`ab spawn` error when workspace doesn't exist:** If `ab spawn -s session --git` is called and `resolve_workspace_dir()` returns `None` (no workspace found for this repo), produce a clear error message including the full canonical repo path (via `source_path()`) and session name. Suggest running `ab new` first. Additionally, scan sentinels for orphaned workspaces with matching `file_name()` and include them as a hint if found — this catches the common case of a repo move/rename and points the user toward the orphaned workspace (see Section 4, `Commands::Spawn` session mode).

**Migration subcommand (`ab dbg migrate`):** A compiled Rust subcommand that migrates old-layout workspaces to the new content-addressable slug format. Using Rust (not a shell script) ensures the slug computation uses the actual `rapidhash` implementation, eliminating hash divergence. The subcommand:
1. Reads `base_repo_dir` from the `--base-repo-dir` CLI flag, falling back to the config file value. If neither is set, error with a message explaining that the old base path is needed for migration. Reads `workspace_dir` from the config file.
2. Walks the workspace tree under `workspace_dir/{git,jj}/` looking for git worktree markers (`.git` *file*, not directory) and jj workspace markers (`.jj/working_copy/`) to identify session directories. Each session's parent directory is the old-layout repo directory, and the relative path between `workspace_dir/{type}/` and that parent is the old `relative_path`. This correctly handles multi-component relative paths (e.g., `work/project`) that create nested directory structures under the old layout. Directories that already contain a `.source-repo` sentinel are skipped (already migrated).
3. For each discovered old-layout repo directory, reconstructs the source repo path as `base_repo_dir / relative_path`.
4. Verifies the source repo still exists on disk. If not, reports it as orphaned and skips (or deletes with `--clean-orphans`).
5. Computes the new slug via `workspace_slug()` (the same `rapidhash`-based function used by all other code paths).
6. Renames the directory to the new slug name. Handles `ENOENT` on rename gracefully (concurrent migration). If the target slug already exists with a `.source-repo` pointing to a different repo, this is a hash collision: report both conflicting paths, skip this repo, and continue migrating the rest. (Same collision error as `ab new` — see Section 1.)
7. Writes the `.source-repo` sentinel file (canonical path + trailing `\n`, written atomically).
8. Supports `--dry-run` to preview changes without acting.

Since the subcommand uses the same hash implementation as the rest of the codebase, migrated workspaces are immediately on the fast path — no scan fallback needed on first spawn.

The subcommand is removed from the codebase in a follow-up release once the migration window has passed.

**Action:** Add a "Breaking Changes" section to the CHANGELOG/README:

1. **Before upgrading (recommended):** Run `ab dbg migrate --dry-run` to preview migration, then `ab dbg migrate` to migrate. If `base_repo_dir` has already been removed from the config, pass it via `--base-repo-dir <path>`. Old-layout workspaces will be renamed to the new content-addressable slug format and sentinel files written.
2. **After upgrading (alternative):** If you have no active sessions worth preserving, simply delete `workspace_dir/git/` and `workspace_dir/jj/` directories and recreate workspaces with `ab new`.

Additional notes:
- `base_repo_dir` config field is deprecated and ignored. It can be removed from config files.
- `--repo` flag on `ab spawn` and positional `repo_name` on `ab new` now accept a filesystem path (not a search string). The fuzzy repo search (`ab new agent-box` matching repos under `base_repo_dir`) is removed with no replacement — users should `cd` into the repo first (the common case) or pass an explicit path. Shell completion and tools like `zoxide` make explicit paths ergonomic. This is an intentional simplification: CWD detection covers the primary workflow, and removing the search avoids the need for a configured scan directory.
- **Repo moves/renames:** If a repository is moved or renamed after workspaces are created, the canonical path changes and the old workspace slug becomes orphaned. The sentinel file makes this detectable (`.source-repo` will point to a non-existent path). When `ab spawn` fails to find a workspace, it scans for orphans with matching `file_name()` and includes them in the error as a hint. Users can discover all orphans with `ab dbg orphans` and clean them up with `ab dbg clean` (all) or `ab dbg clean <slug>` (specific slug).

## Dependency order

0. Add `rapidhash` to Cargo.toml (no deps)
1. `workspace_slug()` in path.rs (depends on 0)
2. `RepoIdentifier` redesign in path.rs (depends on 1)
3. Config struct change in config.rs + schema regeneration (no deps, parallel with 0-2)
4. repo.rs updates incl. `find_git_root_from()`, sentinel file logic, integration test (depends on 2)
5. display.rs updates (depends on 2)
6. main.rs CLI changes incl. `DbgCommands::Migrate`, `Orphans`, `Remove`, and `Clean` (depends on 3, 4)
7. Test fixtures (depends on all above)
8. Documentation (depends on all above)
9. Migration subcommand `ab dbg migrate` (depends on finalized slug format from 1 and CLI structure from 6, can be written in parallel with 4-8)

## Verification

1. `cargo check --workspace` -- compiles
2. `cargo clippy --workspace --all-targets -- -D warnings` -- no warnings
3. `cargo test --workspace` -- all tests pass (including new integration test for `new_workspace` → `resolve_workspace_dir` round-trip)
4. `cargo fmt --all -- --check` -- formatted
5. Manual test: `cd` into a repo, run `ab new -s test --git`, then `ab spawn -s test --git`
6. Manual test: `ab new /path/to/repo/src/ -s test --git` (explicit subdirectory path — should resolve to repo root)
7. Manual test: verify old config with `base_repo_dir` prints deprecation warning but works
8. Manual test: verify sentinel file is created and collision detection works
9. Manual test: `ab dbg orphans` lists old-layout dirs and orphans correctly
10. Manual test: `ab dbg orphans <slug>` shows detailed info for a specific slug
11. Manual test: `ab dbg clean` cleans up all orphaned workspaces (with confirmation prompt)
12. Manual test: `ab dbg clean <slug>` removes a specific slug directory (with confirmation)
13. Manual test: `ab dbg remove` with no args detects repo from CWD and prompts for confirmation
14. Manual test: `ab spawn -s nonexistent --git` produces clear error with suggestion
15. Manual test: sentinel-verified lookup — rename a slug directory manually, verify `ab spawn` finds it via scan fallback, auto-heals (renames to correct slug), and subsequent spawn uses fast path
16. Manual test: `ab dbg migrate --dry-run` on an old-layout workspace directory previews correct migration
17. Manual test: `ab dbg migrate` migrates old-layout workspaces to content-addressable slugs, writes sentinel files, and `ab spawn` finds them via fast path (no scan fallback needed)
18. Manual test: `ab dbg migrate --base-repo-dir /path/to/repos` works when `base_repo_dir` has been removed from config
19. Manual test: `ab dbg migrate` with two old-layout repos that hash-collide — verify first migrates, second reports collision and is skipped
20. Manual test: move a repo, run `ab spawn`, verify error message includes hint about orphaned workspace with matching name
21. Manual test: `cd` into a session workspace (linked worktree), run `ab info`, verify it shows the source repo's workspaces
22. Manual test: `ab dbg migrate` correctly handles old-layout workspaces with multi-component relative paths (e.g., `work/project`)
23. Manual test: `ab new my-project` (bare name, no path separator, not a valid path) produces helpful hint about removed repo search
