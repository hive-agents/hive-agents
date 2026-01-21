# Pairing and Connection Protocols

This document captures the current OTP-based pairing flow (HTTPS) and the
manual SSH-only fallback. It also lists
integration points for future work.

## Manual fallback protocol (SSH-only, manual approval)

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
2. Server owner exports a non-secret connection profile:
   - `hive-core connect --host <public-host>`
   - The JSON includes SSH host/port, tunnel layout, JuiceFS templates, and
     `SecretRef` placeholders for secrets.
3. Hive imports the profile and uses TOFU for the SSH host key (accepts first
   key, fails if it changes later).
4. Hive generates a per-device SSH keypair and shows a command:
   - `hive-core device add --name <device> --pubkey "<ssh-ed25519 ...>"`
5. Server owner runs the `device add` command, which appends a restricted key to
   `/home/hive/.ssh/authorized_keys` with forwarding-only options.
6. Hive prompts for secrets (Postgres password, S3 keys) and stores them in the
   OS keychain at the profile's `SecretRef` paths.
7. Hive opens SSH tunnels and mounts JuiceFS using the profile templates.

Security properties:
- A leaked profile does not grant access (no secrets; SSH key still required).
- Access is gated by manual key approval.
- SSH host key pinning (TOFU) protects against MITM after the first connect.

## Current MVP protocol (OTP with HTTPS endpoint)

Status: implemented.

This flow makes the "enter OTP" step meaningful by allowing Hive to register a
device key without manual `device add`. It requires a minimal HTTPS pairing
endpoint on hive-core.

Requirements:
- HTTPS endpoint on hive-core (self-signed or real cert).
- OTP is high-entropy (not a short numeric code) and single-use.
- OTP is bundled alongside the non-secret connection info and expires quickly
  (target: 5 minutes).
- OTP is intended for machine exchange, not manual entry.
- Rate limiting, short TTL, and attempt limits for OTP validation.
- Hive verifies the HTTPS server identity (pinning or trusted CA).
- The pairing service is started by `hive-core install` and listens on
  `127.0.0.1:8081` by default (typically proxied by Caddy).

Pairing envelope (JSON output from `hive-core connect`):
```json
{
  "profile": { "...": "non-secret profile JSON" },
  "otp": "opaque-high-entropy-token",
  "otp_expires_at": "2026-01-21T00:00:00Z",
  "pair_url": "https://<public-host>/pair"
}
```

Pairing API (HTTPS, JSON):

`POST /pair`

Request body:
```json
{
  "otp": "opaque-high-entropy-token",
  "device_name": "hive-desktop",
  "device_pubkey": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA..."
}
```

Response body (success, HTTP 200):
```json
{
  "profile_id": "b1c6c2a7-1a29-4df4-bd38-4a4fe2cc8a0a",
  "secrets": {
    "keychain:get-hive:profile/b1c6c2a7-1a29-4df4-bd38-4a4fe2cc8a0a/meta_password": "<postgres-password>",
    "keychain:get-hive:profile/b1c6c2a7-1a29-4df4-bd38-4a4fe2cc8a0a/s3_access_key": "<s3-access-key>",
    "keychain:get-hive:profile/b1c6c2a7-1a29-4df4-bd38-4a4fe2cc8a0a/s3_secret_key": "<s3-secret-key>"
  }
}
```

Response body (error, non-200):
```json
{
  "error": "otp_invalid",
  "message": "OTP is invalid, expired, or already used"
}
```

Expected error codes:
- `400 Bad Request`: malformed JSON, missing fields, invalid `device_pubkey` format.
- `401 Unauthorized`: OTP invalid, expired, or already used.
- `409 Conflict`: `device_name` already exists in `authorized_keys`.
- `429 Too Many Requests`: rate-limited (per-IP or per-OTP).
- `500 Internal Server Error`: unexpected server error.

Validation rules:
- `otp` must be a high-entropy token (>=128 bits of entropy) and single-use.
- `otp_expires_at` is UTC ISO 8601; server rejects after expiry.
- `device_name` must not contain whitespace.
- `device_pubkey` must be a single-line SSH public key (`ssh-` or `ecdsa-`).

Minimal JSON schema (Draft 2020-12 style; descriptive only):

Pairing envelope (`hive-core connect` output):
```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["profile", "otp", "otp_expires_at", "pair_url"],
  "properties": {
    "profile": { "type": "object" },
    "otp": { "type": "string", "minLength": 32 },
    "otp_expires_at": { "type": "string", "format": "date-time" },
    "pair_url": { "type": "string", "format": "uri" }
  },
  "additionalProperties": false
}
```

Pair request (`POST /pair`):
```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["otp", "device_name", "device_pubkey"],
  "properties": {
    "otp": { "type": "string", "minLength": 32 },
    "device_name": { "type": "string", "minLength": 1 },
    "device_pubkey": { "type": "string", "minLength": 16 }
  },
  "additionalProperties": false
}
```

Pair response (success, HTTP 200):
```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["profile_id", "secrets"],
  "properties": {
    "profile_id": { "type": "string", "format": "uuid" },
    "secrets": {
      "type": "object",
      "additionalProperties": { "type": "string" }
    }
  },
  "additionalProperties": false
}
```

Pair response (error, non-200):
```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["error", "message"],
  "properties": {
    "error": { "type": "string" },
    "message": { "type": "string" }
  },
  "additionalProperties": false
}
```

Steps:
1. Server owner requests a pairing envelope:
   - `hive-core connect --host <public-host>` prints the envelope JSON.
2. Hive imports the profile, generates a device keypair, and pins the SSH host key.
3. Hive sends the OTP and device pubkey:
   - `POST https://<host>/pair`
   - payload: { otp, device_name, device_pubkey }
4. hive-core validates the OTP (TTL, one-time, rate-limited), writes the key
   to `authorized_keys` with strict forwarding-only options, and returns the
   required secrets over HTTPS (keyed by `SecretRef`).
5. Hive stores the secrets locally and connects over SSH as in the MVP flow.

Notes:
- HTTPS termination can be handled by Caddy (or equivalent) serving the pairing
  endpoint on the same host. The pairing service listens on `127.0.0.1:8081`
  by default.
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
- `connect` should emit the pairing envelope (profile + OTP + expiry + pair_url).
- `device add` appends to `/home/hive/.ssh/authorized_keys` with:
  `no-pty,no-agent-forwarding,no-X11-forwarding,permitopen="127.0.0.1:5432",permitopen="127.0.0.1:8333"`.
- `fingerprint` uses the host SSH key and must be shown to the client to pin.

Hive desktop core:
- Must use TOFU host key pinning (accept new, fail on change).
- Must prompt for secrets if `SecretRef` values are missing.
- Uses `tunnels` + `dsn_template` + `bucket_url_template` to build runtime
  connection strings.

Possible future changes:
- Add rate limiting and OTP cleanup (currently TODO in the pairing service).
- Add profile fields for HTTPS cert pinning or pairing endpoint URL.
- Support BYO S3 by setting `EndpointMode::Direct` and removing the S3 tunnel.
- Add device listing and removal commands backed by a registry file or API.
