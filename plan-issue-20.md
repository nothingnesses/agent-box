# Plan: Add `repo_discovery_dirs` (Issue #20)

## Context

This plan covers Phase 2 of the initiative to enable spawning containers from anywhere on the filesystem. The work is split across four issues, each submitted as a separate PR:

- **#16** (Phase 1): Default `base_repo_dir` to `/`, fix linked worktree resolution, add `ab dbg list`, add `ab new` hint for bare names.
- **#20** (Phase 2): Add `repo_discovery_dirs` config and `--repo-discovery-dir` CLI flag for repo name lookup.
- **#17** (Phase 3): Handle moved/renamed repos and repair existing workspaces.
- **#21** (Phase 4): Add command to change `base_workspace_dir`.

Phase 2 depends on Phase 1 (issue #16), specifically:

- `base_repo_dir_explicit` (Phase 1 step 2): a `#[serde(skip)]` bool on `Config` that tracks whether the user explicitly set `base_repo_dir` or it defaulted to `/`. Phase 2's config validation (step 1) and discovery logic (step 4) both branch on this field.
- The two-priority `discover_repo_ids` rewrite (Phase 1 step 3): Phase 2 step 4 extends this with a new Priority 1 for `repo_discovery_dirs`, shifting the existing priorities down.

---

## Phase 2: Add `repo_discovery_dirs` (Issue #20)

This phase adds multi-directory repo discovery for the `-r` flag, replacing the single `base_repo_dir` scanning approach.

Depends on Phase 1 (uses `base_repo_dir_explicit` from Phase 1 step 2).

### 1. Add `repo_discovery_dirs` config field

**File:** [config.rs](common/src/config.rs)

- Add `#[serde(default)] pub repo_discovery_dirs: Vec<PathBuf>` to `Config` struct
- In `load_config()` (after line 823), expand each path in `repo_discovery_dirs` (tilde expansion, canonicalization)
- In `load_config()`, validate: if `repo_discovery_dirs` is non-empty and `base_repo_dir_explicit` is true (and `base_repo_dir` is not `/`), return an error. These two features are incompatible because `strip_prefix(base_repo_dir)` will fail for repos in discovery dirs that are not under `base_repo_dir`. Users should use one or the other:
  ```
  Error: `repo_discovery_dirs` cannot be used with an explicit `base_repo_dir`.
  Either remove `base_repo_dir` to use the default,
  or remove `repo_discovery_dirs` and place all repos under `base_repo_dir`.
  ```
  Note: this blanket rejection is stricter than necessary (it would be valid if all discovery dirs happen to be under `base_repo_dir`), but keeps the logic simple. If users request more flexibility, a future refinement could validate each discovery dir with `discovery_dir.starts_with(&base_repo_dir)` at config load time instead.

### 2. Add `--repo-discovery-dir` CLI flag

**File:** [main.rs](ab/src/main.rs)

Add a repeatable `--repo-discovery-dir <dir>` flag to the CLI. Values from CLI and config are merged (not replaced). This flag should be available on commands that accept `-r`/`--repo`.

### 3. Update `discover_repos_in_dir` to accept separate scan/strip dirs

**File:** [path.rs](common/src/path.rs) lines 105-160

Change signature from `discover_repos_in_dir(base_dir, is_repo)` to `discover_repos_in_dir(scan_dir, strip_base, is_repo)`. The walker scans `scan_dir` but `strip_prefix` uses `strip_base` to compute relative paths. When `scan_dir == strip_base` (current behavior for explicit `base_repo_dir`), nothing changes. When using `repo_discovery_dirs`, `scan_dir` is the discovery dir and `strip_base` is `config.base_repo_dir` (which is `/` by default).

Key changes inside the function:
- `WalkDir::new(scan_dir)` (line 117)
- `filter_entry` comparisons use `scan_dir` (lines 123, 134)
- `strip_prefix(strip_base)` (line 150) operates on the canonicalized path

After joining `scan_dir` with the walkdir-relative entry, canonicalize the discovered repo path before calling `strip_prefix`. This is required by the cross-cutting canonicalization section (see plan-issue-16.md).

### 4. Update `discover_repo_ids` to support discovery dirs

**File:** [path.rs](common/src/path.rs) lines 162-168

Add a new priority to the discovery logic from Phase 1 step 3:

```
Priority 1: If repo_discovery_dirs is non-empty (config + CLI merged), scan each dir
             -> discover_repos_in_dir(discovery_dir, config.base_repo_dir, is_repo) for each
Priority 2: If base_repo_dir was explicitly set AND is not "/", scan it (backward compat)
             -> discover_repos_in_dir(config.base_repo_dir, config.base_repo_dir, is_repo)
Priority 3: Neither configured, return error with guidance
```

Note: Priority 1 and Priority 2 are mutually exclusive because step 1's validation rejects `repo_discovery_dirs` combined with an explicit non-`/` `base_repo_dir`. If that validation is ever relaxed (see step 1's note about future refinement), this priority logic must be revisited to handle the combined case (scanning discovery dirs with a non-`/` strip base).

### 5. Regenerate JSON schema

After config struct changes, regenerate:
```bash
cargo run --bin generate_schema > common/config.schema.json
```

### 6. Tests

#### Fix existing tests

- Add `repo_discovery_dirs: vec![]` to `make_test_config()` and all inline `Config` structs in path.rs tests (lines 380, 422, 464) and runtime/mod.rs tests (lines ~1328, ~1422, ~1498, ~1597, ~1670). Since `repo_discovery_dirs` uses `#[serde(default)]` (defaults to `vec![]`), tests that construct `Config` via deserialization get correct defaults automatically. For tests that build `Config` structs manually, add the field explicitly.

#### New tests for repo discovery

**File:** [path.rs](common/src/path.rs)

- `test_discover_with_repo_discovery_dirs`: create temp dirs with mock repos, set `repo_discovery_dirs`, verify repos are discovered with correct full-path relative paths (stripped against `base_repo_dir`)
- `test_discover_with_multiple_discovery_dirs`: set multiple discovery dirs, verify repos from all dirs are found

#### New tests for config

**File:** [config.rs](common/src/config.rs)

- `test_config_with_repo_discovery_dirs`: TOML with `repo_discovery_dirs = ["~/repos", "~/work"]`, verify paths are expanded
- `test_config_rejects_discovery_dirs_with_explicit_base`: TOML with both `base_repo_dir = "/home/user/repos"` and `repo_discovery_dirs = ["~/work"]`, verify `load_config()` returns an error

### 7. Update docs

- [config.md](docs/src/reference/agent-box/config.md): add `repo_discovery_dirs` documentation
- Document lookup behavior: how config and CLI values are merged, how ambiguous names produce a prompt

### Phase 2 verification

1. `cargo build` compiles
2. `cargo test` passes
3. Manual test: set `repo_discovery_dirs`, verify `-r` flag discovers repos from those dirs
4. Manual test: pass `--repo-discovery-dir` on CLI, verify it merges with config
5. Manual test: use `-r agent-box` with repos in multiple discovery dirs, verify prompt appears for disambiguation

---

## Cross-cutting concern: Canonicalize repo paths

This phase must adhere to the canonicalization requirements described in plan-issue-16.md (and in plan-v2.md's cross-cutting concerns section). Specifically, `discover_repos_in_dir` must canonicalize discovered repo paths before calling `strip_prefix`, and canonicalization failures during discovery should warn and skip rather than abort. See plan-issue-16.md for the full specification of canonicalization behavior, error handling, and the `debug_assert!` contract on `from_repo_path`.
