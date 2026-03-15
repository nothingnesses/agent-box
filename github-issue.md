Title: Remove `base_repo_dir` requirement

---

## Problem

Session/worktree mode requires all repositories to be under a single `base_repo_dir`. This is limiting because repositories are often spread across multiple locations and the requirement adds upfront configuration before the tool can be used. Making `base_repo_dir` accept an array of directories was considered, but rejected because different base directories can contain repos with the same directory name, leading to slug collisions and ambiguous workspace lookups. It would also increase lookup times proportionally to the number of directories added.

Workarounds don't hold up:

- `--local` mode bypasses the check, but loses worktree sandboxing.
- Setting `base_repo_dir` to a common ancestor like `~` slows discovery by scanning the entire home directory.
- Symlinking repos into `base_repo_dir` doesn't work, agent-box resolves symlinks to their real paths, which then fall outside `base_repo_dir`.

## Proposal

Remove `base_repo_dir` entirely. `ab new` detects the git/jj root from the current directory (or an explicit path argument). Workspace paths use a content-addressable hash slug, the full 16 hex characters of a `rapidhash` u64 of the canonical repo path:

```bash
cd ~/anywhere/my-repo
ab new -s my-session --git
ab spawn -s my-session --git
```

```
# Workspace layout (before → after)
{workspace_dir}/git/{relative_path}/{session}     # old: requires base_repo_dir
{workspace_dir}/git/a1b2c3d4e5f6a7b8/{session}   # new: content-addressable slug
```

A `.source-repo` sentinel file inside each slug directory maps it back to the source repo's canonical path. This is the authoritative mapping, the hash is just a fast-path optimization for directory naming.

### Why pure hex slugs (not name+hash)

A hybrid like `my-repo-a1b2c3d4` was considered but rejected: repo directory names can contain non-UTF-8 bytes, characters invalid on other platforms, or sequences that complicate parsing (e.g., embedded hyphens conflicting with the name-hash separator). Pure hex avoids all sanitization edge cases. The sentinel file and `ab info`/`ab dbg list` provide human readability where needed.

### Collision handling

16 hex chars = 64 bits gives a birthday-bound collision probability of ~50% at ~4 billion repos. On creation, if the computed slug already exists and its sentinel points to a different repo, `ab new` errors with a message naming both conflicting paths and suggests removing the conflicting workspace.

### Sentinel file (`.source-repo`)

- **Format:** Canonical repo path followed by a trailing `\n`. Written atomically (temp file + rename).
- **On creation (`ab new`):** Compute slug → check sentinel → create or reuse. Before creating a new directory, scan existing slug dirs to prevent duplicate workspaces (e.g., after migration or hash algorithm change).
- **On lookup (`ab spawn`):** Compute slug → verify sentinel. If mismatched or absent, fall back to scanning all slug dirs. Found path is used directly (no automatic rename) to avoid breaking active sessions. Logs a suggestion to run `ab dbg migrate`.

### Linked worktree resolution

Running `ab new` or `ab spawn` from inside a session workspace (which is a git linked worktree) now correctly resolves to the source repo root via `repo.kind()` + `common_dir().parent()`, instead of treating the worktree itself as a repo. Local mode (`--local`) preserves the current behavior of using the worktree directory as-is.

### CLI changes

- `ab new` positional arg changes from a search string to a filesystem path. CWD detection is the primary workflow.
- `ab spawn --repo` changes from `String` to `PathBuf`.
- Fuzzy repo search (matching repos under `base_repo_dir`) is removed, users `cd` into the repo first or pass an explicit path.

### New subcommands

- **`ab dbg list`**: Shows all workspaces with status (`healthy`/`orphaned`/`old-layout`), source path, slug, type, and session count. `--orphans` filters to broken workspaces. Accepts optional slug for detailed info.
- **`ab dbg migrate`**: Migrates old-layout workspaces to new slug format. Reads `base_repo_dir` from `--base-repo-dir` flag or config. Checks for active sessions (via `*.lock` files), supports `--dry-run` and `--force`. Idempotent and safe to re-run.
- **`ab dbg remap <old-path> <new-path>`**: Updates a workspace's sentinel after a repo move/rename. Renames slug directory if needed. Supports `--dry-run`.
- **`ab dbg remove`**: Enhanced with `--orphans` flag to clean up orphaned/old-layout workspaces. Supports `--dry-run` and `--force`.

### Migration path

1. **Recommended:** Back up `workspace_dir`, run `ab dbg migrate --dry-run` to preview, then `ab dbg migrate`.
2. **Alternative:** Delete `workspace_dir/git/` and `workspace_dir/jj/` and recreate with `ab new`.

`base_repo_dir` becomes `Option<PathBuf>` with a deprecation warning. It's only needed for migration; after migrating, it can be removed from the config.

### Repo move detection

When `ab spawn` can't find a workspace, it scans sentinels for orphans with a matching directory name and suggests `ab dbg remap` in the error message.

---

A [detailed implementation plan](https://github.com/nothingnesses/agent-box/blob/bd399bfedef901bbfe27d92331c55113cd4a5518/plan.md) covers all of the above, sentinel file semantics, migration safety, linked worktree handling, error messages, and dependency order, if ever it's decided that the proposal should be pursued.
