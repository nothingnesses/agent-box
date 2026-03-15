# Tutorial: Connect Portal to Agent-box

## Outcome

You run an Agent-box container where tools can use Portal through wrapper binaries.

## Prerequisites

- Agent-box setup working (`ab spawn` succeeds)
- Wrappers installed in container image or mounted into container PATH

## Option A: use a user-managed global Portal

1. Enable portal in config:

    ```toml
    [portal]
    enabled = true
    global = true
    socket_path = "/run/user/1000/agent-portal/portal.sock"

    [portal.policy.defaults]
    clipboard_read_image = "allow"
    gh_exec = "ask_for_writes"
    ```

2. Start portal host on the machine running containers:

    ```bash
    agent-portal-host
    ```

3. Spawn an Agent-box session:

    ```bash
    ab spawn -r myrepo -s portal-session
    ```

    Agent-box mounts the configured socket and sets `AGENT_PORTAL_SOCKET` in the container.

## Option B: let `ab` manage Portal per container

1. Enable managed mode in config:

    ```toml
    [portal]
    enabled = true
    global = false

    [portal.policy.defaults]
    clipboard_read_image = "allow"
    gh_exec = "ask_for_writes"
    ```

2. Spawn an Agent-box session:

    ```bash
    ab spawn -r myrepo -s portal-session
    ```

    In this mode, `ab` starts a dedicated in-process Portal host, mounts its per-container socket, and shuts it down when the container exits.

## Validate wrapper-backed flow

In the container:

```bash
wl-paste --list-types
```

If wrappers are in PATH and policy allows, this returns an image MIME type when present.

## What you learned

- How Agent-box and Portal integrate
- How wrappers keep calling conventions tool-compatible
- The difference between global and per-container Portal operation
