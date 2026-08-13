use crate::{
    isaac_format::{canonical_identity_for_path, canonical_identity_for_path_once},
    model::{FileIdentity, LocalSave},
};
use anyhow::{Context, Result, bail};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_DISCOVERY_DEPTH: usize = 6;
const MAX_SAVE_SIZE: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy)]
struct DiscoveryMode {
    retry_until_valid: bool,
    skip_invalid_candidates: bool,
}

pub fn slot_for_filename(name: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    for slot in 1..=3 {
        let repentance = format!("rep_persistentgamedata{slot}.dat");
        let rebirth = format!("persistentgamedata{slot}.dat");
        if lower == repentance || lower == rebirth {
            return Some(slot);
        }
    }
    None
}

pub fn discover_saves(home: &Path) -> Result<Vec<LocalSave>> {
    discover_saves_internal(home, None, true, false)
}

pub fn discover_saves_immediate(home: &Path) -> Result<Vec<LocalSave>> {
    // A force-close can leave one of Isaac's empty slots in its temporary
    // truncated representation until the next launch repairs it. Preserve all
    // valid slots and defer only the invalid candidate; never hash or upload it.
    discover_saves_internal(home, None, false, true)
}

pub fn discover_saves_for_slots(
    home: &Path,
    allowed_slots: Option<&BTreeSet<u8>>,
) -> Result<Vec<LocalSave>> {
    discover_saves_internal(home, allowed_slots, true, false)
}

/// Locate the existing live file for a queued restore without parsing it.
///
/// Isaac can leave an empty slot temporarily truncated after a forced exit. A
/// restore must be able to replace that exact file at the prelaunch barrier;
/// requiring it to parse first would permanently block recovery. This locator
/// still stays inside the application sandbox, ignores our backup tree, rejects
/// symlinks, and refuses ambiguous destinations.
pub fn locate_save_path_for_restore(
    home: &Path,
    slot: u8,
    preferred_filename: &str,
) -> Result<PathBuf> {
    if slot_for_filename(preferred_filename) != Some(slot) {
        bail!("backup filename does not match its save slot");
    }
    let roots = [home.join("Documents"), home.join("Library")];
    let mut candidates = Vec::new();
    for root in roots {
        if root.is_dir() {
            walk_restore_candidates(&root, slot, 0, &mut candidates)?;
        }
    }
    candidates.sort_by(|left, right| {
        restore_path_rank(left, preferred_filename)
            .cmp(&restore_path_rank(right, preferred_filename))
            .then_with(|| left.cmp(right))
    });
    let destination = candidates
        .first()
        .cloned()
        .context("live save slot not found for queued restore")?;
    let best_rank = restore_path_rank(&destination, preferred_filename);
    if candidates
        .iter()
        .skip(1)
        .any(|candidate| restore_path_rank(candidate, preferred_filename) == best_rank)
    {
        bail!("multiple equally preferred live save files found for queued restore");
    }
    Ok(destination)
}

fn walk_restore_candidates(
    dir: &Path,
    slot: u8,
    depth: usize,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    if depth > MAX_DISCOVERY_DEPTH {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(ty) = entry.file_type() else { continue };
        let path = entry.path();
        if ty.is_symlink() {
            continue;
        }
        if ty.is_dir() {
            if entry.file_name() != "IsaacCloudSync" {
                walk_restore_candidates(&path, slot, depth + 1, out)?;
            }
            continue;
        }
        if !ty.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if slot_for_filename(name) == Some(slot) {
            out.push(path);
        }
    }
    Ok(())
}

fn restore_path_rank(path: &Path, preferred_filename: &str) -> u8 {
    let exact_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(preferred_filename));
    let in_repentance = path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|value| value.eq_ignore_ascii_case("Repentance"))
    });
    match (exact_name, in_repentance) {
        (true, true) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (false, false) => 3,
    }
}

fn discover_saves_internal(
    home: &Path,
    allowed_slots: Option<&BTreeSet<u8>>,
    retry_until_valid: bool,
    skip_invalid_candidates: bool,
) -> Result<Vec<LocalSave>> {
    let roots = [home.join("Documents"), home.join("Library")];
    // Decide the active DLC generation from filenames before parsing bytes.
    // Older Rebirth files may have a different valid magic/structure; parsing
    // them as Repentance before filtering would incorrectly block the active
    // Repentance save for the same slot.
    let repentance_slots = collect_repentance_slots(&roots, allowed_slots)?;
    let mode = DiscoveryMode {
        retry_until_valid,
        skip_invalid_candidates,
    };
    let mut candidates = Vec::new();
    for root in roots {
        if root.is_dir() {
            walk(
                &root,
                home,
                allowed_slots,
                &repentance_slots,
                mode,
                0,
                &mut candidates,
            )?;
        }
    }
    // Isaac keeps older DLC generations next to the active Repentance data.
    // Prefer Repentance independently per slot, while retaining a legacy
    // fallback for installations that do not have that DLC generation.
    candidates
        .retain(|save| !repentance_slots.contains(&save.slot) || is_repentance_save(&save.path));
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(candidates)
}

fn collect_repentance_slots(
    roots: &[PathBuf],
    allowed_slots: Option<&BTreeSet<u8>>,
) -> Result<BTreeSet<u8>> {
    fn visit(
        dir: &Path,
        allowed_slots: Option<&BTreeSet<u8>>,
        depth: usize,
        slots: &mut BTreeSet<u8>,
    ) -> Result<()> {
        if depth > MAX_DISCOVERY_DEPTH {
            return Ok(());
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(()),
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let Ok(ty) = entry.file_type() else { continue };
            if ty.is_symlink() {
                continue;
            }
            let path = entry.path();
            if ty.is_dir() {
                if entry.file_name() != "IsaacCloudSync" {
                    visit(&path, allowed_slots, depth + 1, slots)?;
                }
                continue;
            }
            if !ty.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some(slot) = slot_for_filename(name) else {
                continue;
            };
            if allowed_slots.is_some_and(|allowed| !allowed.contains(&slot)) {
                continue;
            }
            if name
                .to_ascii_lowercase()
                .starts_with("rep_persistentgamedata")
            {
                slots.insert(slot);
            }
        }
        Ok(())
    }

    let mut slots = BTreeSet::new();
    for root in roots {
        if root.is_dir() {
            visit(root, allowed_slots, 0, &mut slots)?;
        }
    }
    Ok(slots)
}

fn is_repentance_save(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| {
            name.to_ascii_lowercase()
                .starts_with("rep_persistentgamedata")
        })
}

fn walk(
    dir: &Path,
    home: &Path,
    allowed_slots: Option<&BTreeSet<u8>>,
    repentance_slots: &BTreeSet<u8>,
    mode: DiscoveryMode,
    depth: usize,
    out: &mut Vec<LocalSave>,
) -> Result<()> {
    if depth > MAX_DISCOVERY_DEPTH {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(ty) = entry.file_type() else { continue };
        let path = entry.path();
        if ty.is_symlink() {
            continue;
        }
        if ty.is_dir() {
            // Never inspect our own backups as possible live saves.
            if entry.file_name() != "IsaacCloudSync" {
                walk(
                    &path,
                    home,
                    allowed_slots,
                    repentance_slots,
                    mode,
                    depth + 1,
                    out,
                )?;
            }
            continue;
        }
        if !ty.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some(slot) = slot_for_filename(name) else {
            continue;
        };
        if allowed_slots.is_some_and(|slots| !slots.contains(&slot)) {
            continue;
        }
        if repentance_slots.contains(&slot)
            && !name
                .to_ascii_lowercase()
                .starts_with("rep_persistentgamedata")
        {
            continue;
        }
        let identity = if mode.retry_until_valid {
            canonical_identity_for_path(&path)
        } else {
            canonical_identity_for_path_once(&path)
        };
        let identity = match identity {
            Ok(identity) => identity,
            Err(_) if mode.skip_invalid_candidates => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("validate local save slot {slot}"));
            }
        };
        if identity.size > MAX_SAVE_SIZE {
            bail!("candidate save is unexpectedly large: {}", path.display());
        }
        let relative_path = path
            .strip_prefix(home)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        out.push(LocalSave {
            slot,
            path,
            relative_path,
            identity,
        });
    }
    Ok(())
}

pub fn identity_for_path(path: &Path) -> Result<FileIdentity> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let metadata = file.metadata()?;
    let mut reader = BufReader::new(file);
    let mut sha256 = Sha256::new();
    let mut sha1 = Sha1::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        sha256.update(&buffer[..count]);
        sha1.update(&buffer[..count]);
    }
    Ok(FileIdentity {
        sha256: hex::encode(sha256.finalize()),
        size: metadata.len(),
        modified_unix_ms: metadata.modified().ok().and_then(system_time_ms),
        steam_sha1: Some(hex::encode(sha1.finalize())),
    })
}

pub fn identity_for_bytes(bytes: &[u8]) -> FileIdentity {
    FileIdentity {
        sha256: hex::encode(Sha256::digest(bytes)),
        size: bytes.len() as u64,
        modified_unix_ms: None,
        steam_sha1: Some(hex::encode(Sha1::digest(bytes))),
    }
}

pub async fn wait_for_stable_snapshot(path: &Path, staging: &Path) -> Result<FileIdentity> {
    let first = identity_for_path(path)?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let second = identity_for_path(path)?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let third = identity_for_path(path)?;
    if !same_observation(&first, &second) || !same_observation(&second, &third) {
        bail!("save is still being written");
    }
    fs::copy(path, staging).with_context(|| {
        format!(
            "stage stable save {} -> {}",
            path.display(),
            staging.display()
        )
    })?;
    let staged = identity_for_path(staging)?;
    let after_copy = identity_for_path(path)?;
    if staged.sha256 != third.sha256
        || staged.size != third.size
        || !same_observation(&third, &after_copy)
    {
        bail!("save changed while staging");
    }
    Ok(third)
}

fn same_observation(left: &FileIdentity, right: &FileIdentity) -> bool {
    left.sha256 == right.sha256
        && left.size == right.size
        && left.modified_unix_ms == right.modified_unix_ms
}

pub(crate) fn system_time_ms(value: SystemTime) -> Option<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

pub fn unix_ms() -> u64 {
    system_time_ms(SystemTime::now()).unwrap_or(0)
}

pub fn unique_temp_path(directory: &Path, prefix: &str) -> PathBuf {
    directory.join(format!(".{prefix}.{}.tmp", uuid::Uuid::new_v4()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_save(marker: u32) -> Vec<u8> {
        let mut bytes = b"ISAACNGSAVE09R  ".to_vec();
        bytes.extend_from_slice(&marker.to_le_bytes());
        for section in 1_u32..=10 {
            bytes.extend_from_slice(&section.to_le_bytes());
            bytes.extend_from_slice(&0_u32.to_le_bytes());
            bytes.extend_from_slice(&0_u32.to_le_bytes());
        }
        bytes.extend_from_slice(&11_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&4_u32.to_le_bytes());
        for subtype in [4_u32, 2, 3, 1] {
            bytes.extend_from_slice(&subtype.to_le_bytes());
            bytes.extend_from_slice(&0_u32.to_le_bytes());
        }
        bytes.extend_from_slice(&[0_u8; 8]);
        crate::isaac_format::write_valid_checksum_for_tests(&mut bytes);
        bytes
    }

    #[test]
    fn recognizes_only_supported_persistent_slot_names() {
        assert_eq!(slot_for_filename("rep_persistentgamedata1.dat"), Some(1));
        assert_eq!(slot_for_filename("REP_PERSISTENTGAMEDATA3.DAT"), Some(3));
        assert_eq!(slot_for_filename("persistentgamedata2.dat"), Some(2));
        assert_eq!(slot_for_filename("rep_gamestate1.dat"), None);
        assert_eq!(slot_for_filename("rep_persistentgamedata4.dat"), None);
    }

    #[test]
    fn discovery_is_sandbox_relative_and_ignores_our_backups() {
        let home = std::env::temp_dir().join(format!("isaaccloud-local-{}", uuid::Uuid::new_v4()));
        let documents = home.join("Documents");
        let backup = home.join("Library/Application Support/IsaacCloudSync/backups/slot1");
        fs::create_dir_all(&documents).unwrap();
        fs::create_dir_all(&backup).unwrap();
        let live = test_save(1);
        fs::write(documents.join("rep_persistentgamedata1.dat"), &live).unwrap();
        fs::write(backup.join("rep_persistentgamedata1.dat"), b"backup").unwrap();

        let saves = discover_saves(&home).unwrap();
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].slot, 1);
        assert_eq!(
            saves[0].relative_path,
            "Documents/rep_persistentgamedata1.dat"
        );
        let expected = identity_for_bytes(&live);
        assert_eq!(saves[0].identity.sha256, expected.sha256);
        assert_eq!(saves[0].identity.steam_sha1, expected.steam_sha1);
        assert_eq!(saves[0].identity.size, expected.size);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn repentance_files_override_legacy_files_per_slot() {
        let home = std::env::temp_dir().join(format!("isaaccloud-dlc-{}", uuid::Uuid::new_v4()));
        let documents = home.join("Documents");
        let repentance = documents.join("Repentance");
        fs::create_dir_all(&repentance).unwrap();
        fs::write(
            documents.join("persistentgamedata1.dat"),
            b"valid-for-an-older-DLC-but-not-Repentance",
        )
        .unwrap();
        fs::write(documents.join("persistentgamedata2.dat"), test_save(2)).unwrap();
        fs::write(repentance.join("rep_persistentgamedata1.dat"), test_save(3)).unwrap();

        let saves = discover_saves(&home).unwrap();
        assert_eq!(saves.len(), 2);
        let slot1 = saves.iter().find(|save| save.slot == 1).unwrap();
        let slot2 = saves.iter().find(|save| save.slot == 2).unwrap();
        assert_eq!(
            slot1.relative_path,
            "Documents/Repentance/rep_persistentgamedata1.dat"
        );
        assert_eq!(slot2.relative_path, "Documents/persistentgamedata2.dat");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn queued_restore_can_locate_a_temporarily_invalid_live_slot() {
        let home =
            std::env::temp_dir().join(format!("isaaccloud-restore-{}", uuid::Uuid::new_v4()));
        let repentance = home.join("Documents/Repentance");
        let backup = home.join("Library/Application Support/IsaacCloudSync/backups/slot3");
        fs::create_dir_all(&repentance).unwrap();
        fs::create_dir_all(&backup).unwrap();
        let live = repentance.join("rep_persistentgamedata3.dat");
        fs::write(&live, b"temporarily truncated").unwrap();
        fs::write(
            backup.join("rep_persistentgamedata3.dat"),
            b"must not be selected",
        )
        .unwrap();

        let found = locate_save_path_for_restore(&home, 3, "rep_persistentgamedata3.dat").unwrap();
        assert_eq!(found, live);
        let _ = fs::remove_dir_all(home);
    }
}
