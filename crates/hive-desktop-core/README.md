# hive-desktop-core (MVP)

This crate owns the desktop-side supervisor for connecting to hive-core via SSH,
running JuiceFS, and monitoring/tearing down both processes.

## Responsibilities

- Spawn and monitor an SSH tunnel process (forwarding Postgres + S3).
- Run preflight checks (Postgres + S3 reachability) before mounting.
- Spawn and monitor the `juicefs mount` process.
- Provide a simple handle API for connect/disconnect/status/logs.
- Maintain an in-memory log buffer for UI consumption.

## Key modules

- `supervisor.rs`: State machine and orchestration.
- `tunnel.rs`: SSH tunneling (port allocation, process health, temp key file).
- `mount.rs`: JuiceFS mount/unmount and mount detection.
- `health.rs`: Postgres + S3 preflight checks.
- `filesystem.rs`: Platform-specific mount detection.

## Integration points

### SecretStore

Consumers must provide a `SecretStore` implementation:

- `resolve_text` is used for all secrets, including the SSH private key.
- `resolve_path` is currently unused by the supervisor.

The supervisor materializes the SSH key to a temp file (strict perms) and
passes it to `ssh -i`. The temp file is deleted when the tunnel session drops.
There is a TODO in `tunnel.rs` to replace this with agent/keychain-backed
identity handling in a later hardening pass.

### Profile + LocalSettings

- `Profile` supplies SSH config, tunnel definitions, and JuiceFS templates.
- `LocalSettings` supplies mountpoint + cache settings.

Template placeholders are resolved inside the supervisor:

- `{local_postgres_port}`
- `{local_s3_port}`

If the placeholders are present but the ports are missing, connect fails.

### Supervisor API (Tauri / UI)

`SupervisorHandle::spawn(...)` runs the supervisor loop on a tokio task.

Common usage flow:

1. `connect()` (allowed only when state is `Idle`).
2. Poll `status()` for state, mountpoint, and local ports.
3. Read `logs_tail(n)` for recent logs.
4. `disconnect()` or `force_disconnect()` on user action.

Error handling:

- If connect fails, the supervisor transitions to `Error` and the last error is
  stored in `Status.last_error`.
- `connect()` returns an error and also triggers `handle_failure`, which attempts
  a cleanup/disconnect with force.

## Tunnel behavior

- Dynamic port allocation uses `bind("127.0.0.1:0")` (small race accepted in MVP).
- Tunnel health is true when `ssh` is running AND local ports accept TCP connect.
- On tunnel drop while mounted, the supervisor goes `Degraded` and attempts
  exponential backoff reconnect.

Tunnel naming:

- Expected tunnel names: `postgres` and `s3` (configurable in `SupervisorConfig`).
- The supervisor looks up tunnel ports by name, not by port number.

## Preflight behavior

- Postgres: `SELECT 1` via `tokio-postgres`.
- S3: HTTP(S) GET using `reqwest`; any 2xx/3xx/4xx is treated as reachable.
- 5xx returns a preflight failure (assumed endpoint failure).

## Mount behavior

- `juicefs mount` runs in the foreground and is kept alive.
- `juicefs umount` is used for disconnect, with optional `--force`.
- Mount detection:
  - Linux: `/proc/self/mountinfo` parsing.
  - macOS/Windows: currently unimplemented and returns `UnsupportedPlatform`.

## Config defaults

`SupervisorConfig::default()` uses:

- `ssh_path = "ssh"`
- `juicefs_path = "juicefs"`
- `known_hosts_path = "known_hosts"` (callers should override with a per-app path)
- 5s health interval, 10s tunnel timeout, 15s mount timeout

## Notes for future work

- Replace temp SSH key files with ssh-agent / keychain integration.
- Add macOS + Windows mount detection (or use platform APIs).
- Improve Windows temp key file ACLs (currently read-only only).
- Use a per-profile `known_hosts` file and expose host key pinning in the UI.
