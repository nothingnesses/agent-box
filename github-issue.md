Title: Remove `base_repo_dir` requirement

---

## Problem

Session/worktree mode requires all repositories to be under a single `base_repo_dir`. This is limiting because repositories are often spread across multiple locations and the requirement adds upfront configuration before the tool can be used.

Workarounds don't hold up:

- `--local` mode bypasses the check, but loses worktree sandboxing.
- Setting `base_repo_dir` to a common ancestor like `~` slows discovery by scanning the entire home directory.
- Symlinking repos into `base_repo_dir` doesn't work, agent-box resolves symlinks to their real paths, which then fall outside `base_repo_dir`.

## Proposal

Remove `base_repo_dir` and have `ab new` detect the git/jj root from the current directory:

```bash
cd ~/anywhere/my-repo
ab new -s my-session --git
ab spawn -s my-session --git
```

Workspace paths would be derived from the repo's directory name with a hash of the full path to avoid collisions:

```
{workspace_dir}/git/my-repo-a1b2c3/{session}
```

This removes a required config field, eliminates the symlink/path resolution issues, and lets the tool work from any repo without upfront configuration.

### Name collisions

Two repos with the same directory name (e.g., `~/work/utils` and `~/personal/utils`) would produce the same workspace path without the hash. Including a short hash of the full repo path in the workspace directory name (e.g., `utils-a1b2c3`) avoids this.
