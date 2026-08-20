# Changelog

## 1.2.0 - 2026-08-20

- Renamed the project and release artifacts to IsaacSteamSynciOS to reflect
  both Steam Cloud and achievement synchronization.

- Added one-way Steam achievement synchronization from native Isaac
  `persistentgamedata` saves when the user taps **Sync Now**.
- Added only save-derived unlocks missing from Steam; existing Steam
  achievements and stat bits are preserved and never cleared.
- Replaced the obsolete UserStats write with the current
  `CMsgClientStoreUserStats2`/EMsg 5466 request, current CRC, and both SteamID
  fields.
- Serialized achievement and Cloud operations through the same authenticated
  Steam session to prevent competing CM logins.
- Added post-write read-back verification, bounded request timeouts, strict
  save-section validation, and additive-safety regression tests.
- Removed all runtime dependence on Game Center and added a build audit that
  rejects GameKit/Game Center linkage.

## 1.1.4 - 2026-08-13

- Added a read-only native menu/game detector for the supported Isaac build.
- Made the Steam Cloud button menu-only and automatically dismissed an open
  settings panel when gameplay begins.
- Resolve Isaac's native player-list vector so the detector cannot remain
  stuck on an inactive main-menu player allocation.

## 1.1.3 - 2026-08-13

Initial public release as IsaacSteamCloudSynciOS.

- Added direct Steam QR/device and username/password authentication with Steam
  Guard approval and Keychain refresh-token persistence.
- Added verified AppID 250900 Cloud enumeration, download, upload, commit, and
  re-enumeration.
- Added canonical iOS raw-LZ4 to Windows save conversion and validation.
- Added last-known-common three-way synchronization, first-sync protection,
  explicit conflict resolution, and manual publication with Sync Now.
- Added local and remote backups, retention, restoration, atomic replacement,
  and interruption-safe recovery.
- Added remote-only prelaunch pulls, offline fallback, lifecycle handling, and
  Steam rich presence.
- Added one portable ARM64 dylib for rootless ElleKit injection and embedded
  non-jailbroken installation.
- Added UIKit account, sync, backup, log, conflict, and invisible-button UI.
- Added IPA patching, signing support, dependency auditing, and comprehensive
  Rust tests.
