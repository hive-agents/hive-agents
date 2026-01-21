# Pairing and Connection Protocols

This document captures the current MVP pairing flow (SSH-only, manual approval) and a
proposed safe OTP-based pairing flow that assumes an HTTPS endpoint. It also lists
integration points for future work.

## Current MVP protocol (SSH-only, manual approval)

Assumptions:
- No web control plane or pairing API.
- Exported profiles are non-secret and shareable.
- Device access is explicitly approved by the server owner via `authorized_keys`.

Actors:
- Server owner (admin) with shell access to hive-core.
- Hive client running on a device.

Steps:
1. Server owner installs the stack:
   - `hive-core install`
2. Server owner reads the SSH host key fingerprint:
   - `hive-core fingerprint`
3. Server owner exports a non-secret connection profile:
   - `hive-core connect --host <public-host>`
   - The JSON includes SSH host/port, tunnel layout, JuiceFS templates, and
     `SecretRef` placeholders for secrets.
4. Hive imports the profile and pins the SSH host key fingerprint.
5. Hive generates a per-device SSH keypair and shows a command:
   - `hive-core device add --name <device> --pubkey "<ssh-ed25519 ...>"`
6. Server owner runs the `device add` command, which appends a restricted key to
   `/home/hive/.ssh/authorized_keys` with forwarding-only options.
7. Hive prompts for secrets (Postgres password, S3 keys) and stores them in the
   OS keychain at the profile's `SecretRef` paths.
8. Hive opens SSH tunnels and mounts JuiceFS using the profile templates.

Security properties:
- A leaked profile does not grant access (no secrets; SSH key still required).
- Access is gated by manual key approval.
- SSH host key pinning protects against MITM on first connect.

## Proposed safe protocol (OTP with HTTPS endpoint)

This flow makes the "enter OTP" step meaningful by allowing Hive to register a
device key without manual `device add`. It requires a minimal HTTPS pairing
endpoint on hive-core.

Requirements:
- HTTPS endpoint on hive-core (self-signed or real cert).
- OTP is high-entropy (not a short numeric code) and single-use.
- OTP is never embedded in the JSON export; it is shown separately and entered
  by the user in Hive.
- Rate limiting, short TTL, and attempt limits for OTP validation.
- Hive verifies the HTTPS server identity (pinning or trusted CA).

Steps:
1. Server owner requests a pairing token:
   - `hive-core connect --host <public-host>` prints the profile JSON.
   - `hive-core pair-token` (or similar) prints a one-time token out of band.
2. Hive imports the profile and pins the SSH host key fingerprint.
3. Hive asks the user for the OTP token and sends:
   - `POST https://<host>/pair`
   - payload: { otp, device_name, device_pubkey }
4. hive-core validates the OTP (TTL, one-time, rate-limited) and writes the key
   to `authorized_keys` with strict forwarding-only options.
5. Hive connects over SSH and mounts as in the MVP flow.

Notes:
- If using a self-signed HTTPS cert, the profile should include a pinned
  certificate fingerprint or CA bundle. Without this, the pairing endpoint is
  vulnerable to MITM.
- A short numeric OTP is not sufficient if the endpoint is internet-reachable.

## Integration points and future work

Profile schema (hive-protocol):
- `Profile` and `LocalSettings` live in `hive-protocol`.
- `Profile` contains secret references (no secret values).
- `SecretRef` strings should map directly to OS keychain entries on the client.

hive-core CLI:
- `connect` builds a profile using values from `/opt/hive-core/.env`.
- `device add` appends to `/home/hive/.ssh/authorized_keys` with:
  `no-pty,no-agent-forwarding,no-X11-forwarding,permitopen="127.0.0.1:5432",permitopen="127.0.0.1:8333"`.
- `fingerprint` uses the host SSH key and must be shown to the client to pin.

Hive desktop core:
- Must enforce host key pinning on first connect.
- Must prompt for secrets if `SecretRef` values are missing.
- Uses `tunnels` + `dsn_template` + `bucket_url_template` to build runtime
  connection strings.

Possible future changes:
- Add a pairing endpoint with HTTPS and token validation (see OTP protocol).
- Add profile fields for HTTPS cert pinning or pairing endpoint URL.
- Support BYO S3 by setting `EndpointMode::Direct` and removing the S3 tunnel.
- Add device listing and removal commands backed by a registry file or API.
