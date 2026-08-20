# IsaacSteamSynciOS

Native Steam Cloud and achievement synchronization for **The Binding of Isaac: Repentance on iOS**.

IsaacSteamSynciOS connects directly to Steam from inside Isaac. It can pull the
Steam version before play, publish a verified iPhone save, and add native-save
achievements that are still locked on Steam when the user taps **Sync Now**.
It never clears an existing Steam achievement. No Game Center, desktop
companion, Steam desktop client, server, JIT, or permanent background daemon is
required.

The same ARM64 synchronization core supports both installation modes:

| Device | Release file |
| --- | --- |
| Jailbroken iPhone or iPad | `IsaacSteamSynciOS-rootless.deb` |
| Non-jailbroken iPhone or iPad | `IsaacSteamSynciOS.dylib` |

The standalone dylib links only Apple system libraries. ElleKit is used only by
the optional rootless package to load it into Isaac.

## Features

- Direct Steam authentication with QR/Steam Mobile or login and password
- Steam Guard, email-code, refresh-token reconnect, timeout, and offline states
- Session credentials stored in the iOS Keychain
- AppID `250900` Cloud file enumeration, download, upload, commit, and
  post-upload verification
- Correct iOS raw-LZ4 to Windows/Steam save normalization
- SHA-256 local identity and Steam SHA-1 verification
- Three-way synchronization using a last-known-common `BASE`
- Automatic remote-only pull before Isaac opens progression
- Explicit **Sync Now** publication for finished iPhone sessions
- Additive Steam achievement sync from native Isaac persistent saves during
  **Sync Now**; existing Steam unlocks are never cleared
- First-sync and true-conflict choices that never guess a winner
- Versioned local and remote backups with retention and restore
- Same-directory atomic local replacement with final hash verification
- Native UIKit account, sync, conflict, backup, log, and invisible-button UI
- Menu-only Steam Sync settings button and panel that hide automatically during a
  run and return after Isaac reaches a menu
- Steam rich presence showing AppID `250900` while Isaac is active
- Offline play without blocking game startup

## How synchronization behaves

At startup the core compares the canonical iPhone save, the Steam object, and
the last verified common hash:

| iPhone | Steam | Result |
| --- | --- | --- |
| Same as BASE | Same as BASE | Nothing changed |
| Changed | Same as BASE | Show iPhone progress waiting for **Sync Now** |
| Same as BASE | Changed | Pull Steam before slot selection |
| Both changed to the same hash | Same content | Update BASE |
| Both changed differently | Conflict; ask the user |

There is no winner on the first sync. The user must choose **Use iPhone Save**,
**Use Steam Save**, or **Cancel**. Both available versions are backed up before
any destructive choice.

Automatic launch and foreground checks never upload. This prevents an old or
partially written local file from silently replacing newer Steam progress.
After finishing an iPhone run, open the cloud menu and tap **Sync Now**. The
core waits for a stable save, converts it to the canonical Windows format,
backs up Steam, uploads, re-enumerates Cloud, and advances BASE only after the
remote SHA-1 matches.

## Achievement synchronization

Achievement synchronization reads the achievement section directly from the
three native `rep_persistentgamedata*.dat` files. It does not read or submit
Game Center achievements.

After a successful manual **Sync Now**, the core reads Steam's live AppID
`250900` achievement schema and current user-stat values. For every Isaac
achievement ID present in an iPhone save but still locked on Steam, it sets only
the corresponding missing bit. Existing Steam stat-group values are preserved,
`explicit_reset` is always false, and the result is read back from Steam for
verification. If Steam omits the current value or CRC required for a safe
additive update, the write is refused.

The direction is intentionally one-way:

| iPhone save | Steam | Result |
| --- | --- | --- |
| Unlocked | Locked | Add the missing unlock to Steam |
| Unlocked | Unlocked | Nothing changes |
| Locked | Unlocked | Keep the Steam unlock; never clear it |
| Locked | Locked | Nothing changes |

## Compatibility

- Steam AppID: `250900`
- iOS bundle identifier: `com.Nicalis.Isaac-iOS`
- Architecture: ARM64
- Minimum deployment target: iOS 15.0
- Tested native Isaac release: App Store version 1.4 with Repentance

Save discovery begins at `NSHomeDirectory()` and never hard-codes an application
container UUID. Repentance files map directly to the Steam names:

```text
Documents/Repentance/rep_persistentgamedata1.dat
Documents/Repentance/rep_persistentgamedata2.dat
Documents/Repentance/rep_persistentgamedata3.dat
```

The core deliberately excludes `rep_gamestate*.dat` and `rep_rerunstate*.dat`.
It synchronizes persistent progression, not a run currently in progress.

## Installation

### Jailbroken devices

Install `IsaacSteamSynciOS-rootless.deb` with a package manager or `dpkg`,
then restart Isaac. The package targets rootless ElleKit installations.

### Non-jailbroken devices

Place `IsaacSteamSynciOS.dylib` in the app's `Frameworks` directory, add
the following Mach-O load command to the main executable, then sign the complete
application bundle:

```text
@executable_path/Frameworks/IsaacSteamSynciOS.dylib
```

The included patcher automates the bundle and Mach-O changes:

```sh
./tools/patch-ipa.sh Isaac.ipa Isaac-SteamSync.ipa
```

The patcher expects a decrypted ARM64 Isaac IPA and produces an unsigned output
unless `SIGNING_IDENTITY` is supplied. It does not download or include Isaac.

```sh
SIGNING_IDENTITY='Apple Development: Example' \
ENTITLEMENTS="$PWD/tools/IsaacSteamSynciOS.entitlements" \
  ./tools/patch-ipa.sh Isaac.ipa Isaac-SteamSync.ipa
```

No JIT, private entitlement, arbitrary executable memory, daemon, or jailbreak
filesystem path is required by the embedded dylib.

## Steam authentication and security

Authentication uses Valve's current Steam Connection Manager WebSocket
protocol. QR sessions are approved in Steam Mobile. Credential sessions encrypt
the password with Valve's current RSA public key before transmission and handle
the confirmation method requested by Steam.

Only the renewable refresh token is persisted. It is stored as a
Security.framework Generic Password item with
`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`. Passwords and Steam Guard
codes remain in memory only and are cleared after submission. Disconnecting
deletes the Keychain item.

Tokens, passwords, QR URLs, saves, IPA files, certificates, and provisioning
profiles are excluded from the repository and never written to logs. Network
traffic goes directly between Isaac and Valve infrastructure.

## Backups and atomic replacement

Runtime data remains inside Isaac's own sandbox. The internal support path is
kept as `IsaacCloudSync` for compatibility with existing installations:

```text
Library/Application Support/IsaacCloudSync/
├── state/sync-state.json
├── backups/slotN/
├── logs/isaaccloud.ndjson
└── tmp/
```

At least 12 backups per slot and source are retained. A manifest records the
operation ID, source, timestamp, filename, size, SHA-256, and Steam SHA-1 when
available.

Downloads never overwrite a live save directly. The core verifies the download,
backs up the existing file, writes a unique same-directory temporary file,
flushes and `fsync`s it, renames atomically, `fsync`s the directory, and verifies
the final canonical SHA-256. Any failure before rename leaves the previous save
untouched.

## Build

Requirements: macOS, Xcode, current Rust with the `aarch64-apple-ios` target,
Python 3, and `dpkg-deb`.

```sh
rustup target add aarch64-apple-ios
make test
make release
```

Release artifacts are written to `dist/`:

```text
IsaacSteamSynciOS.dylib
IsaacSteamSynciOS-rootless.deb
SHA256SUMS
```

Useful additional checks:

```sh
make audit
cargo audit --file src/core/Cargo.lock

# Optional read-only Valve CM connection test; performs no Steam login
cd src/core
cargo test --locked live_cm_directory_websocket_and_client_hello -- --ignored
```

## Verification

The release has been exercised on an iPhone14,4 running iOS 17.3.1:

- rootless ElleKit injection and standalone embedded Mach-O loading;
- QR/Steam Mobile, Steam Guard, credential login, and Keychain reconnect;
- AppID 250900 enumeration, download, upload, commit, and Steam SHA-1 check;
- save-only additive achievement synchronization, including a live update that
  added one missing Steam unlock and an idempotent repeat with zero additions;
- preservation verification confirming zero Steam achievements were cleared;
- canonical Windows save upload after decoding the native iOS raw-LZ4 form;
- first-sync backup and choice, remote-only prelaunch pull, manual Sync Now,
  atomic replacement, backup restore, and offline fallback;
- Steam presence while Isaac is foregrounded;
- lifecycle operation without a launch daemon or jailbreak-only core symbols.

The deterministic unchanged, local-only, remote-only, converged, first-sync,
conflict, save parsing, additive stat-bit update, and achievement-preservation
branches are covered by Rust tests. Public binaries are also checked for
jailbreak-only and Game Center dynamic dependencies.

## Signing and DLC limitations

Re-signing an App Store application can change its application identifier,
Keychain access group, data-container selection, receipt validation, and access
to legitimately purchased DLC. These results depend on the signing workflow.

This project does not modify StoreKit, receipts, purchases, DLC ownership, or
game resources. Back up the application's data before replacing an existing
installation. If a re-signed build cannot validate purchased content, this
project does not bypass that limitation.

## Known limitations

- iOS does not guarantee permanent background execution. A bounded background
  task can finish a manual sync already in progress; otherwise retry on the
  next foreground.
- A pull or restore chosen after gameplay starts is queued for the next launch
  so progression is never replaced underneath the running game.
- Normal Apple development/distribution signing and receipt-dependent DLC
  behavior vary by signer and provisioning profile.
- Steam is an undocumented evolving client protocol. A future Valve or Isaac
  update may require a compatible project update.

## Credits and legal

The Steam transport uses a narrowly patched vendored copy of the MIT-licensed
`steam-cm-protocol` crate. Protocol definitions originate from Valve's public
Steam protocol surface as maintained by SteamDatabase. See
[Third-party notices](THIRD_PARTY_NOTICES.md).

This is an unofficial project and is not affiliated with Valve, Nicalis,
Edmund McMillen, or Apple. It contains no Isaac application, DLC, saves, Steam
credentials, Apple certificates, or DRM bypass. Source code is released under
the [MIT License](LICENSE).
