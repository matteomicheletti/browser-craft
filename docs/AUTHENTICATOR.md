# Authenticator (M4)

Two-factor codes (TOTP and HOTP) in the browser, in `src/authenticator/`.
This note covers the design decisions, the file format, what is tested and
what is not, and how the integration step wires the panel in.

## Where the seeds live: a separate vault

The password manager keeps logins in a KeePass (`.kdbx`) file, and the
`keepass` crate can read KeePassXC's `otp` field (its `totp` feature,
through `totp-lite`). We still keep the authenticator in **its own vault**:

- **Two factors stay two factors.** If the TOTP seeds sit next to the
  passwords, whoever opens the password vault (a stolen key file, a shared
  master password, a KeePassXC sync folder) has both factors. A second file
  with its own master password keeps them apart.
- The password vault can be protected by a key file only; the authenticator
  must not inherit that choice silently.
- The KeePass `otp` field has no standard for tags, ordering, HOTP counter
  updates or per-account settings, and the `keepass` crate's TOTP support is
  SHA-1/256/512 TOTP only. Writing our own small format is less code than
  fitting those into KeePass custom fields, and it is fully tested here.
- Users who want everything in KeePassXC still can: export the plain
  `otpauth://` list and paste the links into KeePassXC's TOTP setup.

## Vault file format (version 1)

`authenticator.bcotp` in the per-user config folder
(`~/.config/BrowseCraft/` on Linux), written with
`storage::write_private` (owner-only folder, atomic replace) and created
with `storage::create_new` (never overwrites).

```text
offset size field
 0      8   magic     "BCOTPVLT" (vault) or "BCOTPBAK" (encrypted backup)
 8      2   version   u16 LE, currently 1
10      1   kdf       1 = Argon2id v1.3
11      4   memory    u32 LE, KiB
15      4   passes    u32 LE
19      4   lanes     u32 LE
23     16   salt      random, new for every password
39     24   nonce     random, new for every write
63      …   XChaCha20-Poly1305 ciphertext of the JSON contents + 16-byte tag
```

- The 63-byte header is the AEAD's associated data: changing any byte
  (including weakening the KDF parameters) makes decryption fail.
- Key: Argon2id (`rust-argon2`), 32 bytes, **64 MiB, 3 passes, 4 lanes**
  (RFC 9106's second recommended setting). About 50 ms in a release build on
  a desktop CPU, seconds in a debug build, so the panel always derives on a
  worker thread. Files are read with the parameters they carry, bounded to
  at most 1 GiB / 64 passes / 16 lanes so a crafted file cannot exhaust
  memory.
- Randomness: `getrandom`. A wrong password and a modified file give the
  same error (an AEAD cannot tell them apart).
- Versions: version 0 is refused as damaged, a newer version is refused with
  "made by a newer BrowseCraft"; new versions must keep reading version 1.
- Contents (JSON): `{"accounts": [...], "settings": {"auto_lock_secs": 300,
  "hide_codes": false}}`; each account has `id`, `issuer`, `label`, `secret`
  (base32), `algorithm` (`Sha1`/`Sha256`/`Sha512`), `digits`, `kind`
  (`{"type":"totp","period":30}` or `{"type":"hotp","counter":n}`), `tags`,
  `note`.
- In memory: the derived key, seeds and the serialized JSON are in
  `Zeroizing` buffers and wiped on drop; locking drops the vault. `Debug`
  of `Secret`, `VaultKey` and `AuthVault` never prints key material.
  Limits: text typed in egui fields is wiped only from our own buffers
  (egui's own copies, e.g. undo history, are not), and `serde_json` may
  leave short-lived copies while parsing. Codes copied to the clipboard are
  handled by core (see below).
- Edits are applied to a copy, written, and kept only if the write
  succeeded, so memory and disk never disagree.
- One process at a time: there is no detection of a second BrowseCraft
  instance writing the same vault (the last save wins).

## Supported formats

| Format | In | Out | Tested with |
|---|---|---|---|
| `otpauth://totp` / `hotp` (Key Uri Format) | yes | yes (plain list) | spec examples, round trips, bad URIs |
| `otpauth-migration://offline?data=…` (Google Authenticator export) | yes | no | a sample link + payloads built from the documented protobuf |
| Manual setup key (base32, spaces/lowercase/dashes) | yes | — | RFC 4648 vectors |
| QR code image (PNG, JPEG, GIF, WebP), decoded with `rqrr` | yes | no | PNGs generated with the `qrcode` crate in tests |
| BrowseCraft encrypted backup (`BCOTPBAK`, same container as the vault) | yes | yes | round trip, wrong password |
| BrowseCraft vault file itself (with its password) | yes | — | through detection |
| Aegis JSON, plain (db version 2 and 3) | yes | no | sample data built per Aegis `docs/vault.md` |
| Aegis JSON, encrypted (password slot: scrypt + AES-256-GCM) | yes | no | sample built per the spec (scrypt N=1024 in the test) |

Not supported: clipboard images (arboard is built without `image-data`),
Steam/Yandex/mOTP entries (skipped and counted), MD5 migration entries
(skipped), Aegis biometric-only slots, exporting to Aegis or Google
formats, account icons (a coloured letter avatar is drawn instead). No
import was tested against files produced by the real Aegis or Google apps:
the tests use data constructed from their published formats (the Google
sample is a migration link encoding the Key Uri Format example account).

Exports are written with `storage::create_new` (owner-only, never replace
an existing file). The plain export needs an explicit "I understand this
file is not encrypted" checkbox and warns to delete the file after use.

## Panel and integration

```rust
let mut auth = authenticator::AuthenticatorPanel::new(); // default vault path
// every frame (also while the drawer is closed), for auto-lock and jobs:
for ev in auth.tick() { handle(ev) }
// inside the drawer:
for ev in auth.ui(ui, &colors) { handle(ev) }
```

- `AuthEvent::Copy(String)`: put the code on the clipboard. The panel does
  not touch the clipboard itself so core keeps one clipboard owner (on X11
  the owner must stay alive) and can clear it later as the password manager
  does.
- `AuthEvent::Unlocked` / `AuthEvent::Locked`: toolbar button state.
- `is_unlocked()`, `vault_exists()`, `lock()` (e.g. on a "lock all" command
  or screen lock), `touch()` (reset the idle timer).
- The panel requests repaints itself (250 ms while unlocked for the
  countdown, 100 ms while a background job runs).
- Dropped files: while the QR image or Import view is open, a file dropped
  on the window fills the path field (`RawInput::dropped_files`).
- Never expose the panel or vault to page scripts, plugins or the automation
  API: there is no IPC prefix and no automation method for it, by design.

Views: locked (unlock / create), list (search, tag chips, live codes with a
countdown ring that turns red in the last 5 s, copy, next HOTP code, edit,
move up/down, delete with confirmation, sort A–Z), add (setup key, link,
QR image, with an import preview that flags accounts already present),
import, export, security settings (auto-lock: never / 1 / 5 / 15 / 60 min,
default 5; hide codes until hovered; change master password; lock now).

## Tests

`cargo test authenticator` (40 tests):

- RFC 4226 appendix D (HOTP) and RFC 6238 appendix B (TOTP SHA-1/256/512, 8
  digits) vectors; base32 RFC 4648 vectors and tolerance.
- `otpauth://` parsing rules (issuer prefix vs parameter, encoded colon,
  `+` in the label, all parameters), rejection of bad links, round trips.
- Google migration: real sample, constructed HOTP/SHA-256/8-digit payload,
  MD5 skipped, truncated data.
- Vault: create/reopen, owner-only mode, no plaintext in the file, wrong
  password, bit flips in salt/nonce/ciphertext/tag and KDF parameters,
  truncation, version 0 / newer version / unknown KDF / huge memory,
  failed save keeps memory unchanged, password change, ordering, HOTP
  counter persistence, duplicate skipping, redacted `Debug`.
- Import/export: URI list round trip, backup round trip, Aegis plain (db v2
  and v3) and encrypted, scrypt bound, QR PNG decode (otpauth and
  migration), non-OTP QR, blank image, no-overwrite export.
- Panel: every view rendered headless in dark and light, unlock/backup/
  import/password-change jobs, copy and HOTP events, delete confirmation,
  auto-lock, form validation.

A visual check in the running browser waits for the integration step.
