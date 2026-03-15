# Plan: Remove `base_repo_dir` from agent-box

## Context

[**GitHub issue draft**](./github-issue.md)

**Problem:** Session/worktree mode in agent-box requires all repositories to be under a single `base_repo_dir` config field. This is limiting because repositories are often spread across multiple locations, and the requirement adds upfront configuration before the tool can be used. Workarounds don't hold up:

- `--local` mode bypasses the check, but loses worktree sandboxing.
- Setting `base_repo_dir` to a common ancestor like `~` slows discovery by scanning the entire home directory.
- Symlinking repos into `base_repo_dir` doesn't work because agent-box resolves symlinks to their real paths, which then fall outside `base_repo_dir`.

**Goal:** Remove `base_repo_dir` entirely. `ab new` will detect the git/jj root from the current directory. Workspace paths will use the repo directory name plus a short hash of the full path for uniqueness (e.g., `{workspace_dir}/git/my-repo-a1b2c3d4/{session}`).

## Changes

### 0. `Cargo.toml` -- Add rapidhash dependency

Add `rapidhash = "4.4"` to workspace dependencies in the root `Cargo.toml` and to `common/Cargo.toml`. (Version 4.4 has been verified to exist on crates.io.)

### 1. `common/src/path.rs` -- RepoIdentifier redesign

**RepoIdentifier struct** (line 35-39): Change `relative_path: PathBuf` to `repo_path: PathBuf` (absolute canonical path).

**Add `workspace_slug()` function**: Computes `{dirname}-{hash8}` from an absolute repo path. Use `rapidhash` (stable between major crate versions, fast, good distribution). First 8 hex chars of the hash for collision resistance (~4B combinations).

Sanitize `dirname` to `[a-zA-Z0-9._-]` (replace other characters with `_`) to avoid problematic filesystem paths. Note: leading dots are preserved, so repos named `.foo` will produce hidden slug directories (e.g., `.foo-a1b2c3d4`). This is acceptable — users who name repos with leading dots are already comfortable with hidden directories, and the slug is an internal implementation detail inside `workspace_dir` that users don't browse manually. `ab dbg orphans` will still find them.

**Note on rapidhash stability:** The rapidhash algorithm has a fixed specification, so output should be stable across crate versions.

**Sentinel file (`.source-repo`) — single source of truth**: When creating a slug directory (e.g., `{workspace_dir}/git/myrepo-a1b2c3d4/`), write a `.source-repo` file containing the canonical path of the source repo. Write atomically (write to a temp file in the same directory, then `fs::rename`) to avoid partial reads from concurrent `ab new` calls.

The sentinel file is the **authoritative** mapping from workspace directory to source repo. The hash slug is a fast-path optimization for directory naming and lookup, not the source of truth. This means:

- **On creation (`ab new`):** compute slug → create dir → write sentinel. If slug dir already exists, read `.source-repo` to check for collision. If it points to a different repo, error with a message naming both conflicting repo paths:
  ```
  Error: workspace slug "my-repo-a1b2c3d4" is already mapped to /home/user/other/my-repo
  (requested: /home/user/repos/my-repo). This is a hash collision (~1 in 4 billion).
  Please report this at <issue tracker URL>.
  ```
- **On lookup (`ab spawn`):** compute slug → find dir → **read `.source-repo`** → verify it matches the current repo path. If it matches, proceed. If it doesn't match (e.g., hash algorithm changed across crate versions), fall back to scanning all slug dirs under `workspace_dir/{git,jj}/`, reading each `.source-repo`, and finding the one that matches. This makes hash algorithm changes self-healing rather than silently orphaning workspaces. When the scan fallback is triggered, log a warning: `Workspace slug mismatch for /path/to/repo — consider running 'ab new' to update. Using fallback lookup.` This makes the degraded state visible so users can proactively fix it.
- **On orphan detection (`ab dbg orphans`):** scan all slug dirs, read sentinels, report any where the source path no longer exists on disk or where no sentinel exists (old-layout).

The cost of sentinel-verified lookup is one `fs::read_to_string` per spawn (a few microseconds). The scan fallback is only triggered if the hash-computed slug doesn't match, which should effectively never happen unless the hash algorithm changes.

**Methods to change:**
- `from_repo_path(config, full_path)` -> `from_path(full_path)`: canonicalize and store. No config needed. This is the single canonicalization point — callers must not pre-canonicalize.
- `source_path(&self, config)` -> `source_path(&self)`: returns `&self.repo_path`. No config needed.
- `relative_path()` -> `name()`: returns `repo_path.file_name()` as `&str` via `to_str()`. If `file_name()` returns `None` (e.g., path is `/`), return an error — `/` is never a valid repo root, and `gix::discover()` would fail before reaching this point anyway. If `to_str()` returns `None` (non-UTF-8 directory name), return an error with a descriptive message including the lossy representation of the path — non-UTF-8 repo directory names are pathological on systems where this tool runs.
- `git_workspace_path`: use `workspace_slug()` instead of `self.relative_path` in path construction. Still needs config for `workspace_dir`. Verify via sentinel (see below).
- `jj_workspace_path`: same.
- `workspace_path`: delegates to `git_workspace_path`/`jj_workspace_path` — no signature change needed beyond what those two require.

**Add `resolve_workspace_dir()` helper**: Given a `workspace_dir`, workspace type (`git`/`jj`), and `repo_path`:
1. Compute slug via `workspace_slug()`.
2. Check if `{workspace_dir}/{type}/{slug}/.source-repo` exists and contains `self.repo_path`.
3. If yes → return `{workspace_dir}/{type}/{slug}/`.
4. If the slug dir exists but sentinel doesn't match → scan all dirs under `{workspace_dir}/{type}/`, read each `.source-repo`, return the one matching `self.repo_path`.
5. If no match found anywhere → return the hash-computed path (for creation by `ab new`) or error (for lookup by `ab spawn`).

`git_workspace_path` and `jj_workspace_path` delegate to this helper.
- `jj_workspaces(config)` -> `jj_workspaces()`: calls `self.source_path()` (no config).
- `git_worktrees(config)` -> `git_worktrees()`: same.

**Remove:**
- `find_matching()`, `discover_repo_ids()`, `discover_repos_in_dir()` -- no global discovery.
- `calculate_relative_path()` -- no longer needed. Note: this is currently a public function; removal is a breaking API change (acceptable since project is young).

**Tests:** Remove `test_find_matching_*` (4 tests). Rewrite `test_repo_identifier_from_repo_path` and `test_repo_identifier_path_builders`. Add tests for `workspace_slug` (uniqueness, determinism, format, dirname sanitization). Add test for sentinel file collision detection. Add test for sentinel-verified lookup (matching sentinel). Add test for scan fallback (slug dir exists but sentinel points to different path, correct dir found by scan). Add test for `name()` returning an error on non-UTF-8 directory names (use `OsStr::from_bytes` on Linux to construct one).

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

**`new_workspace()`** (line 85-128): Change `repo_name: Option<&str>` to `repo_path: Option<&Path>`. Pass through to `resolve_repo_id()`. Update `source_path()` and workspace path calls to drop config where applicable. Still needs config for `workspace_dir`.

**`create_git_worktree()`** (line 177-224): Update `repo_id.source_path()` (drop config), keep config for workspace path. Write `.source-repo` sentinel file atomically (write temp file, then `fs::rename`) to the slug directory after creating the parent directory.

**`create_jj_workspace()`** (line 131-174): Same pattern (including atomic sentinel file write).

**`remove_repo()`** (line 266-318): Use `workspace_slug()` instead of `repo_id.relative_path()` for building paths (lines 274, 281). Display `repo_id.name()` instead of `repo_id.relative_path().display()`.

**Integration test:** Add an automated test for the full `new_workspace` → `resolve_workspace_dir` round-trip. Create a temp directory with a real git repo (`git init`), call `new_workspace` to create a workspace with the new slug layout, then verify: (1) slug directory exists with the expected name, (2) `.source-repo` sentinel file contains the correct canonical path, (3) `resolve_workspace_dir` finds the workspace via the fast path (slug match). This covers the critical interaction between slug computation and sentinel verification.

### 3. `common/src/display.rs` -- Update info command

Line 29: `RepoIdentifier::from_repo_path(config, &repo_path)` -> `RepoIdentifier::from_path(&repo_path)`.
Line 33: `repo_id.git_worktrees(config)` -> `repo_id.git_worktrees()`.
Line 62: `repo_id.jj_workspaces(config)` -> `repo_id.jj_workspaces()`.

Keep `config` in the `info()` function signature for forward-compatibility (e.g., displaying workspace paths in the future would need `config.workspace_dir`).

**Add repo identity header**: Add a line at the top of `info()` output showing the repo name and path, e.g.:
```
Repository: my-repo (/home/user/path/to/my-repo)
```
Use `repo_id.name()` and `repo_id.source_path()` to populate this.

### 4. `ab/src/main.rs` -- CLI changes

**`Commands::New`** (line 27-39): Change `repo_name: Option<String>` to `repo_path: Option<PathBuf>` (positional arg is now a filesystem path, not a search string). Call `new_workspace(&config, repo_path.as_deref(), session.as_deref(), workspace_type)`.

**`Commands::Spawn`** (line 54-55): Change `--repo` from `Option<String>` to `Option<PathBuf>` (a filesystem path).

**`Commands::Spawn` local mode** (line 225-236): In local mode, `RepoIdentifier` is not needed — the workspace path *is* the source path. Replace the `locate_repo` call: if `--repo` is provided, discover the git root from that path via `find_git_root_from(path)`. Otherwise call `find_git_root()` (discovers from CWD). This is consistent with session mode — explicit paths always resolve to the repo root, not the literal path provided. The result is used as both `workspace_path` and `source_path` without constructing a `RepoIdentifier`.

**`Commands::Spawn` session mode** (line 237-253): Pass `repo.as_deref()` as `Option<&Path>` to `resolve_repo_id()` and `new_workspace()`.

**`DbgCommands::Locate`** (line 118-121): Remove entirely.

**`DbgCommands::Orphans`** (new): Scans all immediate subdirectories of `workspace_dir/{git,jj}/`. For each:
- If no `.source-repo` file exists → report as **old-layout directory** (pre-migration).
- If `.source-repo` exists but the path it contains no longer exists on disk → report as **orphaned workspace**.
- Otherwise → skip (healthy).
Output lists orphans and old-layout dirs with their slug names and (where available) original source paths. Read-only — no deletions.

**`DbgCommands::Remove`** (line 123-132): Change `repo: String` to `repo_path: Option<PathBuf>`. Add `--slug <name>` and `--orphans` flags (mutually exclusive with each other and with `repo_path`):
- `repo_path` provided: pass to `RepoIdentifier::from_path()`, compute slug, remove.
- `--slug <name>`: look up `{workspace_dir}/{git,jj}/<name>/` directly, remove. Useful for orphans where the source path no longer exists.
- `--orphans`: run the same scan as `DbgCommands::Orphans`, remove all orphaned and old-layout directories.
- No args and no flags: detect repo from CWD.
In all cases, require interactive confirmation unless `--force` is passed. Keep `--dry-run` flag as-is.

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
    eprintln!("Warning: 'base_repo_dir' is deprecated and ignored. Repos are now detected from the current directory.");
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

**Workspace path layout change:** Workspace paths change from `{workspace_dir}/git/{relative_path}/{session}` to `{workspace_dir}/git/{name}-{hash8}/{session}`. Existing workspaces created under the old layout will not be found or cleaned up by the new code.

**Old-layout and orphan detection:** No warnings on the hot path (`ab new`, `ab spawn`). Old-layout directories and orphaned workspaces are surfaced exclusively via `ab dbg orphans`. Users can clean them up with `ab dbg remove --orphans`.

**`ab spawn` error when workspace doesn't exist:** If `ab spawn -s session --git` is called and the computed slug directory (`{workspace_dir}/git/{slug}/{session}`) doesn't exist, produce a clear error message including the repo name, session name, and expected path. Suggest running `ab new` first.

**One-shot migration script (`scripts/migrate-workspaces.sh`):** Ship a standalone shell script (not compiled into the binary) that migrates old-layout workspaces to the new slug layout. The script:
1. Reads `base_repo_dir` and `workspace_dir` from the config file (`~/.agent-box.toml`).
2. Scans `workspace_dir/{git,jj}/` for old-layout directories (those without a `.source-repo` sentinel).
3. For each old-layout directory, reconstructs the source repo path as `base_repo_dir / relative_path` (the old layout uses the relative path directly as the directory name).
4. Verifies the source repo still exists on disk. If not, reports it as orphaned and skips (or deletes with `--clean-orphans`).
5. Computes the new slug via the same `{dirname}-{hash8}` algorithm (reimplemented in shell using a portable hash — e.g., `printf '%s' "$path" | sha256sum | cut -c1-8` as a stand-in, with a note that this won't match rapidhash and the sentinel file makes that fine).
6. Renames the directory to the new slug name.
7. Writes the `.source-repo` sentinel file.
8. Supports `--dry-run` to preview changes without acting.

Since the script uses sha256 (not rapidhash) for the hash, the slug won't match what the new Rust code computes. This is fine — on first `ab spawn`, the sentinel-verified lookup will find a slug mismatch, trigger the scan fallback, find the correct directory via `.source-repo`, and log the warning. The workspace works immediately; the user can optionally run `ab new` to realign the slug. This is the intended self-healing behavior.

The script is removed from the repository in a follow-up release once the migration window has passed.

**Action:** Add a "Breaking Changes" section to the CHANGELOG/README:

1. **Before upgrading (recommended):** Run `scripts/migrate-workspaces.sh --dry-run` to preview migration, then `scripts/migrate-workspaces.sh` to migrate. Old-layout workspaces will be renamed and sentinel files written. First spawn after migration may trigger the scan fallback (with a logged warning) since the script's hash differs from rapidhash — this is harmless and self-corrects.
2. **After upgrading (alternative):** If you have no active sessions worth preserving, simply delete `workspace_dir/git/` and `workspace_dir/jj/` directories and recreate workspaces with `ab new`.

Additional notes:
- `base_repo_dir` config field is deprecated and ignored. It can be removed from config files.
- `--repo` flag on `ab spawn` and positional `repo_name` on `ab new` now accept a filesystem path (not a search string). The fuzzy repo search (`ab new agent-box` matching repos under `base_repo_dir`) is removed with no replacement — users should `cd` into the repo first (the common case) or pass an explicit path. Shell completion and tools like `zoxide` make explicit paths ergonomic. This is an intentional simplification: CWD detection covers the primary workflow, and removing the search avoids the need for a configured scan directory.
- **Repo moves/renames:** If a repository is moved or renamed after workspaces are created, the canonical path changes and the old workspace slug becomes orphaned. The sentinel file makes this detectable (`.source-repo` will point to a non-existent path). Users can discover orphans with `ab dbg orphans` and clean them up with `ab dbg remove --orphans` or `ab dbg remove --slug <name>`.

## Dependency order

0. Add `rapidhash` to Cargo.toml (no deps)
1. `workspace_slug()` in path.rs (depends on 0)
2. `RepoIdentifier` redesign in path.rs (depends on 1)
3. Config struct change in config.rs + schema regeneration (no deps, parallel with 0-2)
4. repo.rs updates incl. `find_git_root_from()`, sentinel file logic, integration test (depends on 2)
5. display.rs updates (depends on 2)
6. main.rs CLI changes incl. `DbgCommands::Orphans` and `Remove` updates (depends on 3, 4)
7. Test fixtures (depends on all above)
8. Documentation (depends on all above)
9. Migration script `scripts/migrate-workspaces.sh` (depends on finalized slug format from 1, can be written in parallel with 4-8)

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
10. Manual test: `ab dbg remove --orphans` cleans up orphaned workspaces
11. Manual test: `ab dbg remove --slug <name>` removes a specific slug directory
12. Manual test: `ab dbg remove` with no args prompts for confirmation
13. Manual test: `ab spawn -s nonexistent --git` produces clear error with suggestion
14. Manual test: sentinel-verified lookup — rename a slug directory manually, verify `ab spawn` finds it via scan fallback and logs a warning
15. Manual test: `scripts/migrate-workspaces.sh --dry-run` on an old-layout workspace directory previews correct migration
16. Manual test: `scripts/migrate-workspaces.sh` migrates old-layout workspaces, writes sentinel files, and `ab spawn` finds them via scan fallback
