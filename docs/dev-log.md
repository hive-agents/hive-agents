# Dev Log

## 2026-01-21 - E2E dev connection (CLI)

Goal: get `hive-desktop-cli` connected to a dev `hive-core` install with SSH tunnels + JuiceFS mount.

Steps:
- Install JuiceFS on both server and client:
  `curl -sSL https://d.juicefs.com/install | sh`
- Server install (with updated Seaweed S3 config):
  `cargo run -p hive-core-cli install --root ~/hive-core --force`
- Export pairing envelope:
  `target/debug/hive-core connect --host <public-host> --out ~/hive-core-envelope.json`
- Connect (pairs + connects; auto-pin host key for dev):
  `cargo run -p hive-desktop-cli -- connect --envelope ~/hive-core-envelope.json --mountpoint ~/hive-desktop-cli-mount --accept-host-key`
- Verify:
  - Status should show `Mounted`.
  - Files created in `~/hive-desktop-cli-mount` are persisted; reconnecting shows them again.

Notes:
- `ssh-keyscan` can show a proxy key (SSHPiper) vs local `sshd` key; we now default to scanning the public host so the client sees the same key.
- For localhost dev, use `--host localhost` so the pairing URL uses `http://localhost:8081/pair` (no TLS required).
