use crate::{
    local::{identity_for_bytes, system_time_ms},
    model::FileIdentity,
};
use anyhow::{Context, Result, bail};
use std::{fs, path::Path, thread, time::Duration};

const REPENTANCE_MAGIC: &[u8; 16] = b"ISAACNGSAVE09R  ";
const MAX_SAVE_SIZE: usize = 32 * 1024 * 1024;
const SECTION_COUNT: u32 = 11;
const TRAILER_SIZE: usize = 8;
// Native Isaac may rewrite the three persistent files sequentially for about
// six seconds during cold startup. The UIKit startup gate is bounded at 25
// seconds, so allow up to 12 seconds for a complete, structurally valid
// observation before failing closed and continuing local play.
const STABLE_READ_ATTEMPTS: usize = 120;
const STABLE_READ_RETRY_DELAY: Duration = Duration::from_millis(100);

// Repentance uses this game-specific CRC lookup table. It shares the lower
// bits of the conventional CRC-32 table but is not interchangeable with zlib's
// CRC implementation. The checksum covers bytes 16..len-4 with seed
// 0xfedcba76 and is stored little-endian in the final four bytes.
const ISAAC_CRC_TABLE: [u32; 256] = [
    0x00000000, 0x09073096, 0x120E612C, 0x1B0951BA, 0xFF6DC419, 0xF66AF48F, 0xED63A535, 0xE46495A3,
    0xFEDB8832, 0xF7DCB8A4, 0xECD5E91E, 0xE5D2D988, 0x01B64C2B, 0x08B17CBD, 0x13B82D07, 0x1ABF1D91,
    0xFDB71064, 0xF4B020F2, 0xEFB97148, 0xE6BE41DE, 0x02DAD47D, 0x0BDDE4EB, 0x10D4B551, 0x19D385C7,
    0x036C9856, 0x0A6BA8C0, 0x1162F97A, 0x1865C9EC, 0xFC015C4F, 0xF5066CD9, 0xEE0F3D63, 0xE7080DF5,
    0xFB6E20C8, 0xF269105E, 0xE96041E4, 0xE0677172, 0x0403E4D1, 0x0D04D447, 0x160D85FD, 0x1F0AB56B,
    0x05B5A8FA, 0x0CB2986C, 0x17BBC9D6, 0x1EBCF940, 0xFAD86CE3, 0xF3DF5C75, 0xE8D60DCF, 0xE1D13D59,
    0x06D930AC, 0x0FDE003A, 0x14D75180, 0x1DD06116, 0xF9B4F4B5, 0xF0B3C423, 0xEBBA9599, 0xE2BDA50F,
    0xF802B89E, 0xF1058808, 0xEA0CD9B2, 0xE30BE924, 0x076F7C87, 0x0E684C11, 0x15611DAB, 0x1C662D3D,
    0xF6DC4190, 0xFFDB7106, 0xE4D220BC, 0xEDD5102A, 0x09B18589, 0x00B6B51F, 0x1BBFE4A5, 0x12B8D433,
    0x0807C9A2, 0x0100F934, 0x1A09A88E, 0x130E9818, 0xF76A0DBB, 0xFE6D3D2D, 0xE5646C97, 0xEC635C01,
    0x0B6B51F4, 0x026C6162, 0x196530D8, 0x1062004E, 0xF40695ED, 0xFD01A57B, 0xE608F4C1, 0xEF0FC457,
    0xF5B0D9C6, 0xFCB7E950, 0xE7BEB8EA, 0xEEB9887C, 0x0ADD1DDF, 0x03DA2D49, 0x18D37CF3, 0x11D44C65,
    0x0DB26158, 0x04B551CE, 0x1FBC0074, 0x16BB30E2, 0xF2DFA541, 0xFBD895D7, 0xE0D1C46D, 0xE9D6F4FB,
    0xF369E96A, 0xFA6ED9FC, 0xE1678846, 0xE860B8D0, 0x0C042D73, 0x05031DE5, 0x1E0A4C5F, 0x170D7CC9,
    0xF005713C, 0xF90241AA, 0xE20B1010, 0xEB0C2086, 0x0F68B525, 0x066F85B3, 0x1D66D409, 0x1461E49F,
    0x0EDEF90E, 0x07D9C998, 0x1CD09822, 0x15D7A8B4, 0xF1B33D17, 0xF8B40D81, 0xE3BD5C3B, 0xEABA6CAD,
    0xEDB88320, 0xE4BFB3B6, 0xFFB6E20C, 0xF6B1D29A, 0x12D54739, 0x1BD277AF, 0x00DB2615, 0x09DC1683,
    0x13630B12, 0x1A643B84, 0x016D6A3E, 0x086A5AA8, 0xEC0ECF0B, 0xE509FF9D, 0xFE00AE27, 0xF7079EB1,
    0x100F9344, 0x1908A3D2, 0x0201F268, 0x0B06C2FE, 0xEF62575D, 0xE66567CB, 0xFD6C3671, 0xF46B06E7,
    0xEED41B76, 0xE7D32BE0, 0xFCDA7A5A, 0xF5DD4ACC, 0x11B9DF6F, 0x18BEEFF9, 0x03B7BE43, 0x0AB08ED5,
    0x16D6A3E8, 0x1FD1937E, 0x04D8C2C4, 0x0DDFF252, 0xE9BB67F1, 0xE0BC5767, 0xFBB506DD, 0xF2B2364B,
    0xE80D2BDA, 0xE10A1B4C, 0xFA034AF6, 0xF3047A60, 0x1760EFC3, 0x1E67DF55, 0x056E8EEF, 0x0C69BE79,
    0xEB61B38C, 0xE266831A, 0xF96FD2A0, 0xF068E236, 0x140C7795, 0x1D0B4703, 0x060216B9, 0x0F05262F,
    0x15BA3BBE, 0x1CBD0B28, 0x07B45A92, 0x0EB36A04, 0xEAD7FFA7, 0xE3D0CF31, 0xF8D99E8B, 0xF1DEAE1D,
    0x1B64C2B0, 0x1263F226, 0x096AA39C, 0x006D930A, 0xE40906A9, 0xED0E363F, 0xF6076785, 0xFF005713,
    0xE5BF4A82, 0xECB87A14, 0xF7B12BAE, 0xFEB61B38, 0x1AD28E9B, 0x13D5BE0D, 0x08DCEFB7, 0x01DBDF21,
    0xE6D3D2D4, 0xEFD4E242, 0xF4DDB3F8, 0xFDDA836E, 0x19BE16CD, 0x10B9265B, 0x0BB077E1, 0x02B74777,
    0x18085AE6, 0x110F6A70, 0x0A063BCA, 0x03010B5C, 0xE7659EFF, 0xEE62AE69, 0xF56BFFD3, 0xFC6CCF45,
    0xE00AE278, 0xE90DD2EE, 0xF2048354, 0xFB03B3C2, 0x1F672661, 0x166016F7, 0x0D69474D, 0x046E77DB,
    0x1ED16A4A, 0x17D65ADC, 0x0CDF0B66, 0x05D83BF0, 0xE1BCAE53, 0xE8BB9EC5, 0xF3B2CF7F, 0xFAB5FFE9,
    0x1DBDF21C, 0x14BAC28A, 0x0FB39330, 0x06B4A3A6, 0xE2D03605, 0xEBD70693, 0xF0DE5729, 0xF9D967BF,
    0xE3667A2E, 0xEA614AB8, 0xF1681B02, 0xF86F2B94, 0x1C0BBE37, 0x150C8EA1, 0x0E05DF1B, 0x0702EF8D,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveEncoding {
    SteamCanonical,
    IosRawLz4,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalSave {
    pub bytes: Vec<u8>,
    pub encoding: SaveEncoding,
}

/// Returns the normal Steam/Windows representation of a Repentance save.
///
/// Native iOS Isaac writes the complete normal save stream as a raw LZ4 block
/// (without an LZ4 frame header). Windows Isaac expects the decoded stream to
/// begin directly with `ISAACNGSAVE09R  `. Steam downloads are already in that
/// decoded representation, and native iOS Isaac accepts either representation.
pub fn canonicalize_save(input: &[u8]) -> Result<CanonicalSave> {
    if input.starts_with(REPENTANCE_MAGIC) {
        validate_canonical(input)?;
        return Ok(CanonicalSave {
            bytes: input.to_vec(),
            encoding: SaveEncoding::SteamCanonical,
        });
    }

    let decoded = decode_raw_lz4(input)?;
    validate_canonical(&decoded)
        .context("iOS LZ4 stream did not decode to a valid Repentance save")?;
    Ok(CanonicalSave {
        bytes: decoded,
        encoding: SaveEncoding::IosRawLz4,
    })
}

pub fn canonical_identity_for_path(path: &Path) -> Result<FileIdentity> {
    let mut last_error = None;
    for attempt in 0..STABLE_READ_ATTEMPTS {
        match canonical_identity_for_path_once(path) {
            Ok(identity) => return Ok(identity),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < STABLE_READ_ATTEMPTS {
            thread::sleep(STABLE_READ_RETRY_DELAY);
        }
    }
    Err(last_error.context("stable Isaac save read failed")?)
}

pub(crate) fn canonical_identity_for_path_once(path: &Path) -> Result<FileIdentity> {
    let before = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if before.len() > MAX_SAVE_SIZE as u64 {
        bail!("Isaac save exceeds the safety limit");
    }
    let before_modified = before.modified().ok().and_then(system_time_ms);
    let raw = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let after = fs::metadata(path).with_context(|| format!("restat {}", path.display()))?;
    let after_modified = after.modified().ok().and_then(system_time_ms);
    if before.len() != after.len()
        || raw.len() as u64 != after.len()
        || before_modified != after_modified
    {
        bail!("Isaac save changed while it was being read");
    }
    let canonical = canonicalize_save(&raw)?;
    let mut identity = identity_for_bytes(&canonical.bytes);
    identity.modified_unix_ms = after_modified;
    Ok(identity)
}

fn decode_raw_lz4(input: &[u8]) -> Result<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_SAVE_SIZE {
        bail!("invalid iOS LZ4 save size");
    }

    let mut output = Vec::with_capacity(input.len().saturating_mul(2).min(MAX_SAVE_SIZE));
    let mut cursor = 0usize;
    while cursor < input.len() {
        let token = input[cursor];
        cursor += 1;

        let literal_length = extended_length(input, &mut cursor, usize::from(token >> 4))?;
        let literal_end = cursor
            .checked_add(literal_length)
            .context("LZ4 literal length overflow")?;
        if literal_end > input.len()
            || output
                .len()
                .checked_add(literal_length)
                .is_none_or(|size| size > MAX_SAVE_SIZE)
        {
            bail!("LZ4 literal is outside the bounded save stream");
        }
        output.extend_from_slice(&input[cursor..literal_end]);
        cursor = literal_end;

        // A final literal-only sequence ends exactly at the input boundary.
        if cursor == input.len() {
            break;
        }
        let offset_end = cursor.checked_add(2).context("LZ4 offset overflow")?;
        if offset_end > input.len() {
            bail!("truncated LZ4 match offset");
        }
        let offset = usize::from(u16::from_le_bytes([input[cursor], input[cursor + 1]]));
        cursor = offset_end;
        if offset == 0 || offset > output.len() {
            bail!("invalid LZ4 match offset");
        }

        let base_match = usize::from(token & 0x0f);
        let match_length = extended_length(input, &mut cursor, base_match)?
            .checked_add(4)
            .context("LZ4 match length overflow")?;
        if output
            .len()
            .checked_add(match_length)
            .is_none_or(|size| size > MAX_SAVE_SIZE)
        {
            bail!("LZ4 match exceeds the bounded save stream");
        }
        for _ in 0..match_length {
            let value = output[output.len() - offset];
            output.push(value);
        }
    }
    Ok(output)
}

fn extended_length(input: &[u8], cursor: &mut usize, base: usize) -> Result<usize> {
    if base != 15 {
        return Ok(base);
    }
    let mut length = base;
    loop {
        let extension = *input.get(*cursor).context("truncated LZ4 length")?;
        *cursor += 1;
        length = length
            .checked_add(usize::from(extension))
            .context("LZ4 length overflow")?;
        if extension != u8::MAX {
            return Ok(length);
        }
    }
}

fn validate_canonical(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_SAVE_SIZE || !bytes.starts_with(REPENTANCE_MAGIC) {
        bail!("invalid Repentance save header or size");
    }
    // Header (16) plus the save checksum (4).
    let mut cursor = 20usize;
    for expected_type in 1..=SECTION_COUNT {
        let section_type = read_u32(bytes, &mut cursor)?;
        let block_size = read_u32(bytes, &mut cursor)? as usize;
        let count = read_u32(bytes, &mut cursor)? as usize;
        if section_type != expected_type {
            bail!("unexpected Repentance save section {section_type}");
        }

        if section_type == 11 {
            if count != 4 {
                bail!("unexpected Repentance bestiary section count");
            }
            let mut bestiary_payload_size = 0usize;
            for expected_subtype in [4_u32, 2, 3, 1] {
                let subtype = read_u32(bytes, &mut cursor)?;
                let encoded_count = read_u32(bytes, &mut cursor)? as usize;
                if subtype != expected_subtype || !encoded_count.is_multiple_of(4) {
                    bail!("invalid Repentance bestiary subsection");
                }
                bestiary_payload_size = bestiary_payload_size
                    .checked_add(encoded_count)
                    .context("Repentance bestiary size overflow")?;
                advance(bytes, &mut cursor, encoded_count.checked_mul(2))?;
            }
            if bestiary_payload_size != block_size {
                bail!("Repentance bestiary section size mismatch");
            }
            continue;
        }

        let body_size = match section_type {
            1 => Some(block_size),
            2 | 3 | 8 | 9 => count.checked_mul(4),
            4..=7 | 10 => Some(count),
            _ => None,
        }
        .context("Repentance section size overflow")?;
        advance(bytes, &mut cursor, Some(body_size))?;
    }
    if bytes.len().checked_sub(cursor) != Some(TRAILER_SIZE) {
        bail!("unexpected data after Repentance save sections");
    }
    let checksum_offset = bytes.len() - 4;
    let stored = u32::from_le_bytes(
        bytes[checksum_offset..]
            .try_into()
            .expect("four-byte checksum slice"),
    );
    let computed = isaac_crc32(&bytes[16..checksum_offset], 0xfedc_ba76);
    if stored != computed {
        bail!("Repentance save checksum mismatch");
    }
    Ok(())
}

fn isaac_crc32(bytes: &[u8], start: u32) -> u32 {
    let mut crc = !start;
    for byte in bytes {
        let index = usize::from(*byte ^ crc as u8);
        crc = (crc >> 8) ^ ISAAC_CRC_TABLE[index];
    }
    !crc
}

#[cfg(test)]
pub(crate) fn write_valid_checksum_for_tests(bytes: &mut [u8]) {
    let checksum_offset = bytes.len() - 4;
    let checksum = isaac_crc32(&bytes[16..checksum_offset], 0xfedc_ba76);
    bytes[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
}

fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32> {
    let end = cursor.checked_add(4).context("save offset overflow")?;
    let raw: [u8; 4] = bytes
        .get(*cursor..end)
        .context("truncated Repentance save")?
        .try_into()
        .expect("four-byte slice");
    *cursor = end;
    Ok(u32::from_le_bytes(raw))
}

fn advance(bytes: &[u8], cursor: &mut usize, amount: Option<usize>) -> Result<()> {
    let end = cursor
        .checked_add(amount.context("save section size overflow")?)
        .context("save offset overflow")?;
    if end > bytes.len() {
        bail!("truncated Repentance save section");
    }
    *cursor = end;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_fixture() -> Vec<u8> {
        let mut bytes = REPENTANCE_MAGIC.to_vec();
        bytes.extend_from_slice(&0x1234_5678_u32.to_le_bytes());
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
        bytes.extend_from_slice(&[0_u8; TRAILER_SIZE]);
        write_valid_checksum_for_tests(&mut bytes);
        bytes
    }

    fn literal_only_lz4(bytes: &[u8]) -> Vec<u8> {
        let mut encoded = vec![0xf0];
        let mut remaining = bytes.len() - 15;
        while remaining >= 255 {
            encoded.push(255);
            remaining -= 255;
        }
        encoded.push(remaining as u8);
        encoded.extend_from_slice(bytes);
        encoded
    }

    #[test]
    fn accepts_canonical_steam_representation() {
        let fixture = canonical_fixture();
        let result = canonicalize_save(&fixture).unwrap();
        assert_eq!(result.encoding, SaveEncoding::SteamCanonical);
        assert_eq!(result.bytes, fixture);
    }

    #[test]
    fn decodes_ios_raw_lz4_to_canonical_representation() {
        let fixture = canonical_fixture();
        let result = canonicalize_save(&literal_only_lz4(&fixture)).unwrap();
        assert_eq!(result.encoding, SaveEncoding::IosRawLz4);
        assert_eq!(result.bytes, fixture);
    }

    #[test]
    fn rejects_invalid_or_unbounded_lz4() {
        assert!(canonicalize_save(&[0x00, 0x00, 0x00]).is_err());
        assert!(canonicalize_save(&[0xf0]).is_err());
    }

    #[test]
    fn rejects_structurally_valid_save_with_bad_checksum() {
        let mut fixture = canonical_fixture();
        fixture[20] ^= 1;
        assert!(canonicalize_save(&fixture).is_err());
    }

    #[test]
    #[ignore = "requires a private user-supplied save fixture"]
    fn validates_private_external_fixture() {
        let path = std::env::var("ISAAC_SAVE_FIXTURE")
            .expect("set ISAAC_SAVE_FIXTURE to a private .dat path outside the repository");
        let input = fs::read(path).unwrap();
        let canonical = canonicalize_save(&input).unwrap();
        assert!(canonical.bytes.starts_with(REPENTANCE_MAGIC));
    }
}
