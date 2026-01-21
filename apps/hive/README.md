# Hive Desktop UI (apps/hive)

This document describes the current UI implementation, its data contracts, and how it should integrate with the rest of the codebase.

## Purpose and scope

- Desktop-only UI for managing a single Hive connection session.
- Two primary states:
  - Onboarding (no profiles imported): user pastes profile JSON and connects.
  - Connected workspace (profiles exist): local settings + connection status.
- Single active session: profile selection and settings edits are locked while connected.
- Theme follows system light/dark mode via `prefers-color-scheme`.

## UI flow

### Onboarding (no profiles)

- Title: "Connect Hive".
- Actions: paste JSON, `Need help?`, `Connect`.
- Need help modal copy: `hive-core connect --host <public-host>`.
- Copy button uses icon-only UI and flips to a green check after success.
- The pasted JSON may be a pairing envelope (profile + OTP + pair URL).

### After import

- If the pasted JSON is a pairing envelope, the backend performs the OTP
  handshake and stores secrets automatically (no manual step).
- If the pasted JSON is a plain profile (no OTP), the modal shows the manual
  authorization command:
  - `hive-core device add --name "<device-name>" --pubkey "<device-public-key>"`

### Connected workspace

- Profile selector + connect/disconnect button in top right.
- Local settings panel (mountpoint, cache dir, cache size).
- Connection panel shows a red/green status pill; clicking opens a status modal.
- Logs are accessed via the floating button in the lower-right.

## Commands and data contracts

Tauri commands are invoked via `apps/hive/src/lib/api.ts`. All args use snake_case.

- `profiles_list() -> ProfileSummary[]`
  - `ProfileSummary { profile_id, display_name, created_at }`
- `profile_import({ json }) -> ProfileImportResult`
  - `ProfileImportResult { profile_id, needs_authorization, device_public_key }`
- `profile_export({ profile_id }) -> json` (not currently used by UI)
- `get_local_settings({ profile_id }) -> LocalSettings`
- `set_local_settings({ profile_id, patch }) -> void`
- `connect({ profile_id }) -> session_id`
- `disconnect({ session_id }) -> void`
- `status({ session_id }) -> Status`
- `logs_tail({ session_id, n }) -> LogLine[]`

### Status expectations

`Status` should align with `hive-desktop-core` (`get-hive/crates/hive-desktop-core/src/supervisor.rs`):

- `state`: string (snake_case; UI treats `mounted` as connected)
- `mountpoint`: string or null
- `last_error`: string or null
- `local_ports`: map of tunnel name -> port
  - UI expects `postgres` and `s3` keys when tunneled.

### Logs expectations

`LogLine` should align with `hive-desktop-core`:

- `timestamp`: ISO string
- `level`: `info | warn | error`
- `source`: `supervisor | tunnel | mount`
- `message`: string

## Integration notes

- The UI depends on `@tauri-apps/api` v2 and Tauri v2 plugins:
  - `@tauri-apps/plugin-dialog` for directory picker
  - `@tauri-apps/plugin-clipboard-manager` for clipboard
- The backend must register these plugins in `src-tauri` for the desktop app.
- The app uses system light/dark mode; no in-app theme toggle exists.
- Fonts are loaded from Google Fonts in `apps/hive/src/styles.css`:
  - Fraunces (display), Outfit (body), JetBrains Mono (mono)
  - If offline packaging is required, replace with bundled fonts.

## Assets and background

- Bee animation image: `apps/hive/src/assets/bee2.png`
- Bee trails are SVG paths in `App.tsx` and are disabled for reduced motion.
- The hex hero diagram was removed per UI simplification.

## Default behavior and fallbacks

- `get_local_settings` errors fall back to defaults and show an error toast.
- Directory picker is no-op in web preview (`isTauri()` guard).
- Copy success does not show a toast; it flips the icon to a green check.

## Future integration hooks

- Add UI for re-running pairing if the OTP expires before import finishes.
- Consider a `get_local_settings` default response for new profiles.
- Tauri app icon lives at `src-tauri/icons/icon.png` (landing-page logo).
