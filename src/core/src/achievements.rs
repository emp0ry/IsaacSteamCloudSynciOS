use crate::{
    keychain,
    local::discover_saves,
    model::STEAM_APP_ID,
    save_achievements::unlocked_achievement_ids,
    steam::cm,
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
        CMsgClientGamesPlayed, CMsgClientGetUserStats, CMsgClientGetUserStatsResponse,
        CMsgClientStoreUserStats, CMsgClientStoreUserStatsResponse, CMsgProtoBufHeader,
        c_msg_client_games_played::GamePlayed,
        c_msg_client_store_user_stats::StatsToStore,
    },
};

const ERESULT_OK: i32 = 1;
const STORE_PRESENCE_DELAY: Duration = Duration::from_millis(500);

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
pub async fn sync_from_local_saves(home: &Path) -> Result<usize> {
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

    let token = keychain::load_refresh_token()?
        .context("Connect Steam in IsaacSteamCloudSync before syncing achievements")?;
    let session = cm::connect_with_refresh_token(&token)
        .await
        .context("connect Steam CM for save achievement sync")?;
    let state = session.connection.state_snapshot().await;

    // Steam's live AppID 250900 schema/state is the authority for which API
    // names exist and which bits are already unlocked. Hidden achievements are
    // still part of this schema/state and need no special treatment.
    let current = achievements::get_player_achievements(&session.connection, &state, STEAM_APP_ID)
        .await
        .context("request Isaac Steam achievement schema")?;
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

    let verified = store_missing_unlocks(
        &session.connection,
        &state,
        STEAM_APP_ID,
        &names_to_add,
    )
    .await
    .context("store and verify missing Isaac Steam achievements")?;
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
        return achievements::get_player_achievements(connection, state, appid)
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
    let get_packet = connection
        .request(EMsg::ClientGetUserStats, header.clone(), &get_request)
        .await
        .context("refresh raw Steam user stats before achievement store")?;
    let current = get_packet
        .decode_body::<CMsgClientGetUserStatsResponse>()
        .context("decode current Steam user stats")?;
    if current.eresult != Some(ERESULT_OK) {
        bail!("Steam user-stats refresh failed with eresult {:?}", current.eresult);
    }

    let schema = current
        .schema
        .as_deref()
        .context("Steam achievement schema missing before store")?;
    let defs = parse_achievement_bits(schema)?;
    let by_name: HashMap<&str, &AchievementBit> = defs
        .iter()
        .map(|definition| (definition.api_name.as_str(), definition))
        .collect();

    let mut stat_values: HashMap<u32, u32> = current
        .stats
        .iter()
        .filter_map(|stat| Some((stat.stat_id?, stat.stat_value.unwrap_or(0))))
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
        let value = stat_values.entry(definition.stat_id).or_insert(0);
        *value |= 1u32 << definition.bit;
        changed.insert(definition.stat_id);
    }

    let stats_to_store = changed
        .into_iter()
        .map(|stat_id| StatsToStore {
            stat_id: Some(stat_id),
            stat_value: stat_values.get(&stat_id).copied(),
        })
        .collect::<Vec<_>>();

    if stats_to_store.is_empty() {
        return achievements::get_player_achievements(connection, state, appid)
            .await
            .context("verify no-op achievement store");
    }

    // Steam expects user-private stats writes while the app is marked as played.
    connection
        .send_message(
            EMsg::ClientGamesPlayed,
            &session_header(state),
            &CMsgClientGamesPlayed {
                games_played: vec![GamePlayed {
                    game_id: Some(appid as u64),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await
        .context("set Steam game presence for achievement store")?;
    tokio::time::sleep(STORE_PRESENCE_DELAY).await;

    let store_request = CMsgClientStoreUserStats {
        game_id: Some(appid as u64),
        explicit_reset: Some(false),
        stats_to_store,
    };
    let store_result = async {
        let packet = connection
            .request(EMsg::ClientStoreUserStats, header, &store_request)
            .await
            .context("store additive Steam achievement bits")?;
        let response = packet
            .decode_body::<CMsgClientStoreUserStatsResponse>()
            .context("decode Steam user-stats store response")?;
        if response.eresult != Some(ERESULT_OK) {
            bail!("Steam achievement store failed with eresult {:?}", response.eresult);
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
        Ok::<(), anyhow::Error>(())
    }
    .await;

    let _ = connection
        .send_message(
            EMsg::ClientGamesPlayed,
            &session_header(state),
            &CMsgClientGamesPlayed {
                games_played: vec![],
                ..Default::default()
            },
        )
        .await;
    store_result?;

    achievements::get_player_achievements(connection, state, appid)
        .await
        .context("verify Steam achievements after additive store")
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

fn session_header(
    state: &steam_cm_protocol::connection::ConnectionState,
) -> CMsgProtoBufHeader {
    CMsgProtoBufHeader {
        steamid: state.steamid,
        client_sessionid: state.client_session_id,
        ..Default::default()
    }
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
