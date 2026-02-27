# agent-portal

Portal crate containing host service and debug CLI for container-to-host mediated access.

## Binaries

- `agent-portal-host`: host daemon listening on Unix socket
- `agent-portal-cli`: debug/testing client

## Current Methods

- `ping`
- `whoami`
- `clipboard.read_image`

## Development

From repo root:

```bash
cargo run -p agent-portal --bin agent-portal-host -- --help
cargo run -p agent-portal --bin agent-portal-cli -- --help
cargo test -p agent-portal
```

## Integration tests

- `portal/tests/host_integration.rs`
