use crate::isaac_format::canonicalize_save;
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;

const SECTION_TABLE_OFFSET: usize = 0x14;
const SECTION_HEADER_SIZE: usize = 12;
const ACHIEVEMENT_ENTRY_SIZE: usize = 1;
const MAX_REPENTANCE_ACHIEVEMENT_ID: usize = 637;

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset.checked_add(4).context("save offset overflow")?;
    let raw = bytes
        .get(offset..end)
        .context("truncated Isaac section header")?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

/// Parse achievement unlocks from a real Repentance persistentgamedata save.
///
/// The canonical Repentance stream starts its section table at 0x14. Each
/// section begins with three little-endian u32 values, followed by its entries.
/// Achievements are section 0 and use one-byte entries. Achievement IDs are
/// one-based in Isaac, so byte `section_start + N` represents achievement N;
/// byte 0 is intentionally skipped.
pub fn unlocked_achievement_ids(raw: &[u8]) -> Result<BTreeSet<u32>> {
    let canonical = canonicalize_save(raw)?;
    unlocked_achievement_ids_from_canonical(&canonical.bytes)
}

pub fn unlocked_achievement_ids_from_canonical(bytes: &[u8]) -> Result<BTreeSet<u32>> {
    let header_end = SECTION_TABLE_OFFSET
        .checked_add(SECTION_HEADER_SIZE)
        .context("achievement section header overflow")?;
    if bytes.len() < header_end {
        bail!("Isaac save is too small for the achievement section");
    }

    let section_type = read_u32_le(bytes, SECTION_TABLE_OFFSET)?;
    if section_type != 1 {
        bail!("Isaac save does not begin with the achievement section");
    }
    let block_size = read_u32_le(bytes, SECTION_TABLE_OFFSET + 4)? as usize;
    let entry_count = read_u32_le(bytes, SECTION_TABLE_OFFSET + 8)? as usize;
    let expected_block_size = entry_count
        .checked_mul(ACHIEVEMENT_ENTRY_SIZE)
        .context("achievement section size overflow")?;
    if block_size != expected_block_size {
        bail!("Isaac achievement section size/count mismatch");
    }
    let section_start = header_end;
    let section_end = section_start
        .checked_add(block_size)
        .context("achievement section end overflow")?;
    if section_end > bytes.len() {
        bail!("achievement section extends beyond the Isaac save");
    }

    // Repentance has 637 Steam achievements. Repentance+ can expose a few
    // additional save entries, but AppID 250900's live Steam schema is the
    // final authority, so do not invent IDs beyond the Repentance set here.
    let max_id = entry_count
        .saturating_sub(1)
        .min(MAX_REPENTANCE_ACHIEVEMENT_ID);
    let mut unlocked = BTreeSet::new();
    for id in 1..=max_id {
        if bytes[section_start + id] == 1 {
            unlocked.insert(id as u32);
        }
    }
    Ok(unlocked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_based_achievement_bytes() {
        let mut bytes = vec![0u8; SECTION_TABLE_OFFSET + SECTION_HEADER_SIZE + 8];
        bytes[SECTION_TABLE_OFFSET..SECTION_TABLE_OFFSET + 4].copy_from_slice(&1u32.to_le_bytes());
        bytes[SECTION_TABLE_OFFSET + 4..SECTION_TABLE_OFFSET + 8]
            .copy_from_slice(&8u32.to_le_bytes());
        bytes[SECTION_TABLE_OFFSET + 8..SECTION_TABLE_OFFSET + 12]
            .copy_from_slice(&8u32.to_le_bytes());
        let start = SECTION_TABLE_OFFSET + SECTION_HEADER_SIZE;
        bytes[start + 1] = 1;
        bytes[start + 3] = 1;
        let ids = unlocked_achievement_ids_from_canonical(&bytes).unwrap();
        assert_eq!(ids.into_iter().collect::<Vec<_>>(), vec![1, 3]);
    }

    #[test]
    #[ignore = "requires a private user-supplied save fixture"]
    fn parses_private_external_fixture() {
        let path = std::env::var("ISAAC_SAVE_FIXTURE")
            .expect("set ISAAC_SAVE_FIXTURE to a private .dat path outside the repository");
        let raw = std::fs::read(path).unwrap();
        let ids = unlocked_achievement_ids(&raw).unwrap();
        assert!(!ids.is_empty());
        eprintln!(
            "private save contains {} achievement unlocks; highest ID {:?}",
            ids.len(),
            ids.last()
        );
    }
}
