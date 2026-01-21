# get-hive (MVP)

This repo hosts the MVP implementation of Hive and hive-core:

- `crates/hive-core-cli`: server installer and profile exporter.
- `crates/hive-desktop-core`: client supervisor (SSH tunnel + JuiceFS mount).
- `crates/hive-desktop-cli`: dev/test harness for end-to-end runs.
- `apps/hive`: desktop UI with Tauri backend (pairing + connect).

## Dependencies

### Server (hive-core)

- Linux host with public SSH access.
- Docker + `docker compose` plugin.
- OpenSSH server (`sshd` + `ssh-keygen`).
- JuiceFS binary:

```bash
curl -sSL https://d.juicefs.com/install | sh
```

### Client (Hive)

- `ssh`, `ssh-keygen`, `ssh-keyscan`.
- JuiceFS binary (same install command as above).
- FUSE:
  - Linux: `fuse3`
  - macOS: macFUSE
  - Windows: WinFsp

### Desktop app (Tauri build deps)

Tauri CLI v2 (required for the v2 config + ACL):

```bash
cargo install tauri-cli@2 --locked
```

Or use the Node CLI:

```bash
npm --prefix apps/hive install
npm --prefix apps/hive install -D @tauri-apps/cli
```

Linux (Debian/Ubuntu):

```bash
sudo apt install -y build-essential pkg-config libgtk-3-dev libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev libgdk-pixbuf-2.0-dev libpango1.0-dev libcairo2-dev libatk1.0-dev
```

If your distro doesn't ship `*-4.1` packages, install the closest
`webkit2gtk`/`javascriptcoregtk` dev packages available.

macOS: install Xcode Command Line Tools (`xcode-select --install`).
Windows: WebView2 runtime (preinstalled on most Windows 10/11 machines).

## Client setup (Linux)

- Install FUSE: `sudo apt install fuse3` (or your distro equivalent).
- Install JuiceFS:
  `curl -sSL https://d.juicefs.com/install | sh`
- Ensure `ssh` is available (`openssh-client` on Debian/Ubuntu).

## Client setup (macOS)

- Install macFUSE (required for JuiceFS mounts).
- Install JuiceFS:
  `curl -sSL https://d.juicefs.com/install | sh`
- `ssh` is built in; approve macFUSE system extension if prompted.

## Client setup (Windows)

- Install WinFsp (required for JuiceFS mounts).
- Install OpenSSH client (Windows Optional Features).
- Put `juicefs.exe` on PATH.

## Server setup (Linux)

Create a restricted `hive` user and lock down SSH forwarding:

```bash
sudo useradd -m -s /usr/sbin/nologin hive
sudo mkdir -p /home/hive/.ssh
sudo chmod 700 /home/hive/.ssh
```

Add to `/etc/ssh/sshd_config` (then restart `sshd`):

```text
Match User hive
  AllowTcpForwarding yes
  PermitOpen 127.0.0.1:5432 127.0.0.1:8333
  PermitTTY no
  X11Forwarding no
  AllowAgentForwarding no
```

Install hive-core services:

```bash
cargo run -p hive-core-cli install --root /opt/hive-core
```

Export a profile for clients:

```bash
target/debug/hive-core connect --host <public-host> --out profile.json
```

The output is a JSON envelope containing the profile plus a short-lived OTP and
pairing URL for the client handshake.

## Pairing handshake (recommended)

1. Run `hive-core connect --host <public-host>` on the server.
2. Paste the JSON into the Hive desktop app (Tauri).
3. The app calls the pairing endpoint, stores secrets, and connects.

Pairing expects HTTPS. Proxy `https://<host>/pair` to `http://127.0.0.1:8081`
with Caddy or a similar reverse proxy.

## End-to-end test (CLI, manual fallback)

Generate a device key on the client:

```bash
cargo run -p hive-desktop-cli keygen --out ~/.ssh/hive_ed25519 --comment "hive-laptop"
```

Add the device key on the server:

```bash
target/debug/hive-core device add --name hive-laptop --pubkey-file ~/.ssh/hive_ed25519.pub
```

Generate a secrets template (client):

```bash
cargo run -p hive-desktop-cli secrets-template --profile profile.json > secrets.json
```

Fill `secrets.json` with:

- `ssh_ed25519`: the **private key contents**.
- `meta_password`, `s3_access_key`, `s3_secret_key`: values from `/opt/hive-core/.env`.

Example:

```json
{
  "keychain:get-hive:profile/<id>/ssh_ed25519": "-----BEGIN OPENSSH PRIVATE KEY-----\n...\n",
  "keychain:get-hive:profile/<id>/meta_password": "...",
  "keychain:get-hive:profile/<id>/s3_access_key": "...",
  "keychain:get-hive:profile/<id>/s3_secret_key": "..."
}
```

Run the client supervisor:

```bash
cargo run -p hive-desktop-cli connect \
  --profile profile.json \
  --secrets secrets.json \
  --mountpoint ~/Hive \
```

Use Ctrl+C to disconnect.

## Troubleshooting

- If the client fails with host key errors, rerun with `--accept-host-key` to
  auto-pin the key (fingerprint mismatch only warns), or add it to `known_hosts`
  manually.
- If the pinned fingerprint doesn't match the public SSH endpoint (SSH proxies,
  nonstandard host key paths), pass `--fingerprint` to override the scan result.
- If pairing fails, check `docker compose logs pairing` and ensure HTTPS is
  proxying to `127.0.0.1:8081`.
- If `hive-core install` fails on Postgres, check:
  `docker compose logs postgres` in the install root.
- If S3 writes fail, check:
  `docker compose logs seaweedfs`, then rerun `hive-core install --force`.
- `hive-core status` should show postgres/s3 ports as open before you format.

## Notes

- SeaweedFS S3 credentials are written to `state/seaweedfs/s3.json`.
  If you change `/opt/hive-core/.env` or hit S3 access errors, rerun `hive-core install --force`.
- macOS/Windows mount detection is best-effort (no OS-specific mount table check yet).
