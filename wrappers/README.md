# agent-wrappers

Compatibility wrapper binaries that make portal-backed host access transparent to tools/agents.

## Binaries

- `wl-paste`: wrapper compatible with image clipboard flow
- `agent-portal-client`: small helper CLI for scripts/wrappers

## Goal

Tools keep calling familiar commands while wrappers proxy requests to the portal socket.

## Development

From repo root:

```bash
cargo run -p agent-wrappers --bin wl-paste -- --help
cargo run -p agent-wrappers --bin agent-portal-client -- --help
cargo test -p agent-wrappers
```
