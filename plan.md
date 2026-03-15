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

Add `rapidhash = "4.4"` to workspace dependencies in the root `Cargo.toml` and to `common/Cargo.toml`.

### 1. `common/src/path.rs` -- RepoIdentifier redesign

**RepoIdentifier struct** (line 35-39): Change `relative_path: PathBuf` to `repo_path: PathBuf` (absolute canonical path).

**Add `workspace_slug()` function**: Computes `{dirname}-{hash8}` from an absolute repo path. Use `rapidhash` (stable between major crate versions, fast, good distribution). First 8 hex chars of the hash for collision resistance (~4B combinations).

Sanitize `dirname` to `[a-zA-Z0-9._-]` (replace other characters with `_`) to avoid problematic filesystem paths.

**Note on rapidhash stability:** The rapidhash algorithm has a fixed specification, so output should be stable across crate versions. However, if it ever changes, existing workspaces would silently become orphaned. The sentinel file (below) mitigates this by making mismatches detectable.

**Sentinel file (`.source-repo`)**: When creating a slug directory (e.g., `{workspace_dir}/git/myrepo-a1b2c3d4/`), write a `.source-repo` file containing the canonical path of the source repo. Before creating a workspace, check if the slug directory already exists and whether `.source-repo` points to the same repo. If it points to a different repo, error with a collision message. This also enables detecting old-layout directories (they won't have `.source-repo`).

**Methods to change:**
- `from_repo_path(config, full_path)` -> `from_path(full_path)`: canonicalize and store. No config needed. This is the single canonicalization point — callers must not pre-canonicalize.
- `source_path(&self, config)` -> `source_path(&self)`: returns `&self.repo_path`. No config needed.
- `relative_path()` -> `name()`: returns `repo_path.file_name()` as `&str`. If `file_name()` returns `None` (e.g., path is `/`), fall back to the full path string. This avoids a panic on degenerate inputs.
- `git_workspace_path`: use `workspace_slug()` instead of `self.relative_path` in path construction. Still needs config for `workspace_dir`.
- `jj_workspace_path`: same.
- `workspace_path`: delegates to `git_workspace_path`/`jj_workspace_path` — no signature change needed beyond what those two require.
- `jj_workspaces(config)` -> `jj_workspaces()`: calls `self.source_path()` (no config).
- `git_worktrees(config)` -> `git_worktrees()`: same.

**Remove:**
- `find_matching()`, `discover_repo_ids()`, `discover_repos_in_dir()` -- no global discovery.
- `calculate_relative_path()` -- no longer needed. Note: this is currently a public function; removal is a breaking API change (acceptable since project is young).

**Tests:** Remove `test_find_matching_*` (4 tests). Rewrite `test_repo_identifier_from_repo_path` and `test_repo_identifier_path_builders`. Add tests for `workspace_slug` (uniqueness, determinism, format, dirname sanitization). Add test for sentinel file collision detection.

### 2. `common/src/repo.rs` -- Simplify repo resolution

**Remove:** `locate_repo()` (line 44-67), `prompt_select_repo()` (line 27-41).

**`resolve_repo_id()`** (line 69-82): Change `repo_name: Option<&str>` to `repo_path: Option<&Path>`. If a path is provided, canonicalize it and use it directly. If `None`, detect from CWD via `find_git_root()`. No config needed in either case. Note: `find_git_root()` uses `gix::discover()` which works for colocated jj repos (they have `.git`). Non-colocated jj repos are not supported elsewhere in the codebase (`create_jj_workspace` requires `.jj` alongside `.git`), so this is fine.
```rust
pub fn resolve_repo_id(repo_path: Option<&Path>) -> Result<RepoIdentifier> {
    let root = match repo_path {
        Some(path) => path.to_path_buf(),
        None => find_git_root()?,
    };
    // from_path handles canonicalization — don't pre-canonicalize here.
    RepoIdentifier::from_path(&root)
}
```

**Cleanup:** Remove the stale `println!("debug: {repo_id:?}")` at line 80.

**`new_workspace()`** (line 85-128): Change `repo_name: Option<&str>` to `repo_path: Option<&Path>`. Pass through to `resolve_repo_id()`. Update `source_path()` and workspace path calls to drop config where applicable. Still needs config for `workspace_dir`.

**`create_git_worktree()`** (line 177-224): Update `repo_id.source_path()` (drop config), keep config for workspace path. Write `.source-repo` sentinel file to the slug directory after creating the parent directory.

**`create_jj_workspace()`** (line 131-174): Same pattern (including sentinel file).

**`remove_repo()`** (line 266-318): Use `workspace_slug()` instead of `repo_id.relative_path()` for building paths (lines 274, 281). Display `repo_id.name()` instead of `repo_id.relative_path().display()`.

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

**`Commands::Spawn` local mode** (line 225-236): In local mode, `RepoIdentifier` is not needed — the workspace path *is* the source path. Replace the `locate_repo` call: if `--repo` is provided, canonicalize and use it directly as a `PathBuf`. Otherwise call `find_git_root()`. The result is used as both `workspace_path` and `source_path` without constructing a `RepoIdentifier`.

**`Commands::Spawn` session mode** (line 237-253): Pass `repo.as_deref()` as `Option<&Path>` to `resolve_repo_id()` and `new_workspace()`.

**`DbgCommands::Locate`** (line 118-121): Remove entirely.

**`DbgCommands::Remove`** (line 123-132): Change `repo: String` to `repo_path: Option<PathBuf>`. If provided, pass to `RepoIdentifier::from_path()` (which handles canonicalization). If `None`, detect from CWD. Then compute the slug and remove `{workspace_dir}/{git,jj}/{slug}/` as before. Keep `--dry-run` and `--force` flags as-is. Note: this means removing workspaces for a repo you're not currently in requires the full path — an acceptable trade-off for a debug command.

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

**Runtime detection of old-layout directories:** On workspace creation, scan `workspace_dir/{git,jj}/` for immediate subdirectories that lack a `.source-repo` sentinel file. If any are found, print a warning:
```
Warning: Found workspace directories in old layout under {workspace_dir}. These are no longer managed.
Remove them manually or run: rm -rf {workspace_dir}/git/ {workspace_dir}/jj/
```
This warning will repeat on every `ab new` until the old directories are cleaned up. This is intentional — stale workspaces consume disk and the user should address them.

**Action:** Add a "Breaking Changes" section to the CHANGELOG/README:

1. **Before upgrading (if still on old version):** Run `ab dbg remove <repo>` for each repo to clean up old-layout workspaces while the old code can still find them.
2. **After upgrading (primary path):** Manually delete `workspace_dir/git/` and `workspace_dir/jj/` directories to remove orphaned old-layout workspaces.

Additional notes:
- `base_repo_dir` config field is deprecated and ignored. It can be removed from config files.
- `--repo` flag on `ab spawn` and positional `repo_name` on `ab new` now accept a filesystem path (not a search string). The fuzzy repo search (`ab new agent-box` matching repos under `base_repo_dir`) is removed with no replacement — users should `cd` into the repo first (the common case) or pass an explicit path. Shell completion and tools like `zoxide` make explicit paths ergonomic. This is an intentional simplification: CWD detection covers the primary workflow, and removing the search avoids the need for a configured scan directory.
- **Repo moves/renames:** If a repository is moved or renamed after workspaces are created, the canonical path changes and the old workspace slug becomes orphaned. The sentinel file makes this detectable (`.source-repo` will point to a non-existent path), but no automatic cleanup is performed. Users should `ab dbg remove /old/path` before moving, or manually delete the orphaned slug directory after.

## Dependency order

0. Add `rapidhash` to Cargo.toml (no deps)
1. `workspace_slug()` in path.rs (depends on 0)
2. `RepoIdentifier` redesign in path.rs (depends on 1)
3. Config struct change in config.rs + schema regeneration (no deps, parallel with 0-2)
4. repo.rs updates + sentinel file logic (depends on 2)
5. display.rs updates (depends on 2)
6. main.rs CLI changes (depends on 3, 4)
7. Test fixtures (depends on all above)
8. Documentation (depends on all above)

## Verification

1. `cargo check --workspace` -- compiles
2. `cargo clippy --workspace --all-targets -- -D warnings` -- no warnings
3. `cargo test --workspace` -- all tests pass
4. `cargo fmt --all -- --check` -- formatted
5. Manual test: `cd` into a repo, run `ab new -s test --git`, then `ab spawn -s test --git`
6. Manual test: `ab new /path/to/repo -s test --git` (explicit path form)
7. Manual test: verify old config with `base_repo_dir` prints deprecation warning but works
8. Manual test: verify sentinel file is created and collision detection works
