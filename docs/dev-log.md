# Dev Log

## 2026-01-21 - E2E dev connection (CLI)

Goal: get `hive-desktop-cli` connected to a dev `hive-core` install with SSH tunnels + JuiceFS mount.

Steps:
- Install JuiceFS on both server and client:
  `curl -sSL https://d.juicefs.com/install | sh`
- Server install (with updated Seaweed S3 config):
  `cargo run -p hive-core-cli install --root ~/hive-core --force`
- Export profile (now uses `ssh-keyscan` by default):
  `target/debug/hive-core connect --host <public-host> --out ~/hive-core-profile.json`
- Generate an SSH keypair on the client (unencrypted) and add it on the server for the same user in the profile:
  `ssh-keygen -t ed25519 -f ~/.ssh/hive_ed25519 -N "" -C "hive"`
  `target/debug/hive-core device add --name hive-desktop --pubkey-file ~/.ssh/hive_ed25519.pub --authorized-keys /home/<user>/.ssh/authorized_keys`
- Prepare secrets JSON (private key + server env secrets):
  - Start with:
    `cargo run -p hive-desktop-cli secrets-template --profile ~/hive-core-profile.json > ~/hive-secrets.json`
  - Inject private key via `jq` (JSON-escaped):
    `ref=$(jq -r '.ssh.identity_key_ref' ~/hive-core-profile.json)`
    `jq --rawfile key ~/.ssh/hive_ed25519 --arg ref "$ref" '.[$ref]=$key' ~/hive-secrets.json > /tmp/hive-secrets.json && mv /tmp/hive-secrets.json ~/hive-secrets.json`
  - Fill `meta_password`, `s3_access_key`, `s3_secret_key` from `/opt/hive-core/.env`.
- Connect (auto-pin host key for dev):
  `cargo run -p hive-desktop-cli -- connect --profile ~/hive-core-profile.json --secrets ~/hive-secrets.json --mountpoint ~/hive-desktop-cli-mount --accept-host-key`
- Verify:
  - Status should show `Mounted`.
  - Files created in `~/hive-desktop-cli-mount` are persisted; reconnecting shows them again.

Notes:
- `ssh-keyscan` can show a proxy key (SSHPiper) vs local `sshd` key; we now default to scanning the public host so the client sees the same key.
- If connect fails with `Load key ... error in libcrypto`, the secrets file likely contains a public key or a passphrase-protected private key.
