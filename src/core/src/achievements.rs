use crate::{
    local::discover_saves, model::STEAM_APP_ID, save_achievements::unlocked_achievement_ids,
    steam::cm::SteamSession,
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::Path,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use steam_cm_protocol::{
    achievements,
    emsg::EMsg,
    friends::ProtocolAchievement,
    kv::{self, KVValue},
    protobuf::{
        CMsgClientGetUserStats, CMsgClientGetUserStatsResponse, CMsgClientStoreUserStats2,
        CMsgClientStoreUserStatsResponse, CMsgProtoBufHeader,
        c_msg_client_get_user_stats_response::Stats as CurrentStat,
        c_msg_client_store_user_stats2::Stats as StoreStat,
    },
};

const ERESULT_OK: i32 = 1;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const VERIFY_ATTEMPTS: usize = 4;
const VERIFY_DELAY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Serialize)]
pub struct AchievementView {
    pub api_name: String,
    pub display_name: Option<String>,
    pub achieved: bool,
    pub unlock_time: u64,
    pub present_in_local_save: bool,
}

#[derive(Debug, Clone)]
struct AchievementBit {
    stat_id: u32,
    bit: u32,
    api_name: String,
}

static SAVE_UNLOCKS: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
static CACHE: OnceLock<Mutex<HashMap<String, ProtocolAchievement>>> = OnceLock::new();

fn save_unlocks() -> &'static Mutex<BTreeSet<String>> {
    SAVE_UNLOCKS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn cache() -> &'static Mutex<HashMap<String, ProtocolAchievement>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn achievements_json() -> String {
    let local = save_unlocks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut values: Vec<AchievementView> = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .map(|achievement| AchievementView {
            api_name: achievement.apiname.clone(),
            display_name: achievement.name.clone(),
            achieved: achievement.achieved,
            unlock_time: achievement.unlocktime,
            present_in_local_save: local.contains(&achievement.apiname),
        })
        .collect();
    values.sort_by(|left, right| {
        numeric_api_name(&left.api_name)
            .cmp(&numeric_api_name(&right.api_name))
            .then_with(|| left.api_name.cmp(&right.api_name))
    });
    serde_json::to_string(&values).unwrap_or_else(|_| "[]".to_owned())
}

/// Read all real local persistentgamedata saves and add only Steam achievements
/// that are unlocked in at least one save but still locked on Steam.
///
/// This operation is deliberately one-way/additive:
/// - local unlocked + Steam locked => unlock on Steam
/// - local unlocked + Steam unlocked => no-op
/// - local locked + Steam unlocked => NEVER clear Steam
pub async fn sync_from_local_saves(home: &Path, session: &SteamSession) -> Result<usize> {
    let local_ids = collect_local_unlocks(home)?;
    {
        let mut target = save_unlocks()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        target.clear();
        target.extend(local_ids.iter().map(u32::to_string));
    }

    if local_ids.is_empty() {
        return Ok(0);
    }

    let state = session.connection.state_snapshot().await;

    // Steam's live AppID 250900 schema/state is the authority for which API
    // names exist and which bits are already unlocked. Hidden achievements are
    // still part of this schema/state and need no special treatment.
    let current = get_achievements_bounded(&session.connection, &state, STEAM_APP_ID)
        .await
        .context("request Isaac Steam achievement schema")?;
    let previously_achieved = current
        .iter()
        .filter(|achievement| achievement.achieved)
        .map(|achievement| achievement.apiname.clone())
        .collect::<BTreeSet<_>>();
    replace_cache(current);

    let names_to_add = {
        let known = cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        local_ids
            .iter()
            .map(u32::to_string)
            .filter(|name| known.get(name).is_some_and(|item| !item.achieved))
            .collect::<Vec<_>>()
    };

    // Critical safety property: never send locked/false values and never submit
    // a reset. Only bitfields containing missing true unlocks are written.
    if names_to_add.is_empty() {
        return Ok(0);
    }

    let verified = store_missing_unlocks(&session.connection, &state, STEAM_APP_ID, &names_to_add)
        .await
        .context("store and verify missing Isaac Steam achievements")?;
    ensure_no_existing_unlocks_lost(&previously_achieved, &verified)?;
    replace_cache(verified);

    let verified_count = {
        let known = cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        names_to_add
            .iter()
            .filter(|name| known.get(*name).is_some_and(|item| item.achieved))
            .count()
    };
    if verified_count != names_to_add.len() {
        bail!(
            "Steam verified only {verified_count} of {} save-derived achievement unlocks",
            names_to_add.len()
        );
    }
    Ok(verified_count)
}

async fn store_missing_unlocks(
    connection: &steam_cm_protocol::connection::Connection,
    state: &steam_cm_protocol::connection::ConnectionState,
    appid: u32,
    api_names: &[String],
) -> Result<Vec<ProtocolAchievement>> {
    if api_names.is_empty() {
        return get_achievements_bounded(connection, state, appid)
            .await
            .context("refresh achievements");
    }

    let steam_id = state
        .steamid
        .context("Steam session has no SteamID for achievement store")?;
    let get_request = CMsgClientGetUserStats {
        game_id: Some(appid as u64),
        steam_id_for_user: Some(steam_id),
        crc_stats: Some(0),
        schema_local_version: None,
    };
    let header = CMsgProtoBufHeader {
        steamid: state.steamid,
        client_sessionid: state.client_session_id,
        routing_appid: Some(appid),
        ..Default::default()
    };
    let get_packet = tokio::time::timeout(
        REQUEST_TIMEOUT,
        connection.request(EMsg::ClientGetUserStats, header.clone(), &get_request),
    )
    .await
    .context("Steam user-stats refresh timed out")??;
    let current = get_packet
        .decode_body::<CMsgClientGetUserStatsResponse>()
        .context("decode current Steam user stats")?;
    if current.eresult != Some(ERESULT_OK) {
        bail!(
            "Steam user-stats refresh failed with eresult {:?}",
            current.eresult
        );
    }

    let schema = current
        .schema
        .as_deref()
        .context("Steam achievement schema missing before store")?;
    let defs = parse_achievement_bits(schema)?;
    let stats_to_store = build_additive_stats(&defs, &current.stats, api_names)?;

    if stats_to_store.is_empty() {
        return get_achievements_bounded(connection, state, appid)
            .await
            .context("verify no-op achievement store");
    }

    let store_request = CMsgClientStoreUserStats2 {
        game_id: Some(appid as u64),
        settor_steam_id: Some(steam_id),
        settee_steam_id: Some(steam_id),
        crc_stats: Some(
            current
                .crc_stats
                .context("Steam user-stats response omitted its CRC; refusing unsafe write")?,
        ),
        explicit_reset: Some(false),
        stats: stats_to_store,
    };
    let packet = tokio::time::timeout(
        REQUEST_TIMEOUT,
        connection.request(EMsg::ClientStoreUserStats2, header, &store_request),
    )
    .await
    .context("Steam achievement store timed out")??;
    if packet.emsg != EMsg::ClientStoreUserStatsResponse.raw() {
        bail!(
            "Steam returned unexpected EMsg {} for achievement store",
            packet.emsg
        );
    }
    let response = packet
        .decode_body::<CMsgClientStoreUserStatsResponse>()
        .context("decode Steam user-stats store response")?;
    if response.eresult != Some(ERESULT_OK) {
        bail!(
            "Steam achievement store failed with eresult {:?}",
            response.eresult
        );
    }
    if response.stats_out_of_date == Some(true) {
        bail!("Steam rejected achievement store because stats are out of date");
    }
    if !response.stats_failed_validation.is_empty() {
        bail!(
            "Steam rejected {} achievement stat group(s) during validation",
            response.stats_failed_validation.len()
        );
    }

    let mut verified = Vec::new();
    for attempt in 0..VERIFY_ATTEMPTS {
        verified = get_achievements_bounded(connection, state, appid)
            .await
            .context("verify Steam achievements after additive store")?;
        if api_names.iter().all(|name| {
            verified
                .iter()
                .any(|achievement| achievement.apiname == *name && achievement.achieved)
        }) {
            break;
        }
        if attempt + 1 < VERIFY_ATTEMPTS {
            tokio::time::sleep(VERIFY_DELAY).await;
        }
    }
    Ok(verified)
}

async fn get_achievements_bounded(
    connection: &steam_cm_protocol::connection::Connection,
    state: &steam_cm_protocol::connection::ConnectionState,
    appid: u32,
) -> Result<Vec<ProtocolAchievement>> {
    tokio::time::timeout(
        REQUEST_TIMEOUT,
        achievements::get_player_achievements(connection, state, appid),
    )
    .await
    .context("Steam achievement request timed out")?
    .map_err(Into::into)
}

/// Build complete values only for stat groups that gain at least one missing bit.
/// Every source value must come from Steam's immediately preceding response. This
/// avoids defaulting an absent group to zero and accidentally clearing existing bits.
fn build_additive_stats(
    definitions: &[AchievementBit],
    current_stats: &[CurrentStat],
    api_names: &[String],
) -> Result<Vec<StoreStat>> {
    let by_name: HashMap<&str, &AchievementBit> = definitions
        .iter()
        .map(|definition| (definition.api_name.as_str(), definition))
        .collect();
    let mut values: HashMap<u32, u32> = current_stats
        .iter()
        .filter_map(|stat| Some((stat.stat_id?, stat.stat_value?)))
        .collect();
    let mut changed = BTreeSet::new();

    for api_name in api_names {
        let definition = by_name
            .get(api_name.as_str())
            .with_context(|| format!("Steam schema no longer contains achievement {api_name}"))?;
        if definition.bit >= 32 {
            bail!(
                "Steam achievement {} uses unsupported bit {}",
                definition.api_name,
                definition.bit
            );
        }
        let value = values.get_mut(&definition.stat_id).with_context(|| {
            format!(
                "Steam omitted current value for achievement stat group {}; refusing unsafe write",
                definition.stat_id
            )
        })?;
        let updated = *value | (1u32 << definition.bit);
        if updated != *value {
            *value = updated;
            changed.insert(definition.stat_id);
        }
    }

    Ok(changed
        .into_iter()
        .map(|stat_id| StoreStat {
            stat_id: Some(stat_id),
            stat_value: values.get(&stat_id).copied(),
        })
        .collect())
}

fn parse_achievement_bits(schema: &[u8]) -> Result<Vec<AchievementBit>> {
    let root = kv::parse_binary_kv(schema).context("parse Steam achievement schema")?;
    let stats = find_stats_node(&root).context("Steam achievement schema has no stats node")?;
    let entries = stats
        .as_nested()
        .context("Steam achievement stats node is not nested")?;
    let mut result = Vec::new();

    for (stat_key, stat_value) in entries {
        let Ok(stat_id) = stat_key.parse::<u32>() else {
            continue;
        };
        let Some(bits) = stat_value.get("bits").and_then(KVValue::as_nested) else {
            continue;
        };
        for (bit_key, bit_value) in bits {
            let Ok(bit) = bit_key.parse::<u32>() else {
                continue;
            };
            let Some(api_name) = bit_value.get("name").and_then(KVValue::as_str) else {
                continue;
            };
            if !api_name.is_empty() {
                result.push(AchievementBit {
                    stat_id,
                    bit,
                    api_name: api_name.to_owned(),
                });
            }
        }
    }
    Ok(result)
}

fn find_stats_node(root: &KVValue) -> Option<&KVValue> {
    if let Some(stats) = root.get("stats") {
        return Some(stats);
    }
    root.as_nested()?
        .iter()
        .find_map(|(_, value)| value.get("stats"))
}

fn collect_local_unlocks(home: &Path) -> Result<BTreeSet<u32>> {
    let saves = discover_saves(home)?;
    if saves.is_empty() {
        bail!("no valid Isaac persistentgamedata save files were found");
    }

    let mut unlocked = BTreeSet::new();
    for save in saves {
        let raw = fs::read(&save.path)
            .with_context(|| format!("read {} for achievement sync", save.path.display()))?;
        unlocked.extend(
            unlocked_achievement_ids(&raw)
                .with_context(|| format!("parse achievements from {}", save.path.display()))?,
        );
    }
    Ok(unlocked)
}

fn replace_cache(values: Vec<ProtocolAchievement>) {
    let mut target = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    target.clear();
    target.extend(values.into_iter().map(|item| (item.apiname.clone(), item)));
}

fn numeric_api_name(value: &str) -> u64 {
    value.parse().unwrap_or(u64::MAX)
}

fn ensure_no_existing_unlocks_lost(
    previously_achieved: &BTreeSet<String>,
    verified: &[ProtocolAchievement],
) -> Result<()> {
    let achieved_after = verified
        .iter()
        .filter(|achievement| achievement.achieved)
        .map(|achievement| achievement.apiname.clone())
        .collect::<BTreeSet<_>>();
    let lost_existing = previously_achieved.difference(&achieved_after).count();
    if lost_existing != 0 {
        bail!(
            "Steam read-back failed to preserve {lost_existing} previously unlocked achievement(s)"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(stat_id: u32, bit: u32, api_name: &str) -> AchievementBit {
        AchievementBit {
            stat_id,
            bit,
            api_name: api_name.to_owned(),
        }
    }

    #[test]
    fn additive_plan_preserves_every_existing_bit() {
        let definitions = vec![
            definition(10, 2, "1"),
            definition(10, 4, "2"),
            definition(20, 1, "3"),
        ];
        let current = vec![
            CurrentStat {
                stat_id: Some(10),
                stat_value: Some(0b1010_0001),
            },
            CurrentStat {
                stat_id: Some(20),
                stat_value: Some(0b0100_0010),
            },
        ];

        let planned = build_additive_stats(
            &definitions,
            &current,
            &["1".to_owned(), "2".to_owned(), "3".to_owned()],
        )
        .unwrap();

        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].stat_id, Some(10));
        assert_eq!(planned[0].stat_value, Some(0b1011_0101));
    }

    #[test]
    fn additive_plan_refuses_an_unknown_current_group() {
        let error =
            build_additive_stats(&[definition(10, 2, "1")], &[], &["1".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("refusing unsafe write"));
    }

    #[test]
    fn store_protocol_uses_current_message_and_never_resets() {
        let request = CMsgClientStoreUserStats2 {
            game_id: Some(STEAM_APP_ID as u64),
            settor_steam_id: Some(123),
            settee_steam_id: Some(123),
            crc_stats: Some(456),
            explicit_reset: Some(false),
            stats: vec![StoreStat {
                stat_id: Some(10),
                stat_value: Some(0b100),
            }],
        };

        assert_eq!(EMsg::ClientStoreUserStats2.raw(), 5466);
        assert_eq!(request.explicit_reset, Some(false));
        assert_eq!(request.settor_steam_id, request.settee_steam_id);
        assert_eq!(request.crc_stats, Some(456));
    }

    #[test]
    fn verification_rejects_any_lost_steam_unlock() {
        let prior = BTreeSet::from(["1".to_owned(), "2".to_owned()]);
        let verified = vec![ProtocolAchievement {
            apiname: "1".to_owned(),
            achieved: true,
            unlocktime: 1,
            name: None,
            description: None,
        }];

        let error = ensure_no_existing_unlocks_lost(&prior, &verified).unwrap_err();
        assert!(error.to_string().contains("failed to preserve 1"));
    }
}
