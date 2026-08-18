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
};
use steam_cm_protocol::{achievements, friends::ProtocolAchievement};

#[derive(Debug, Clone, Serialize)]
pub struct AchievementView {
    pub api_name: String,
    pub display_name: Option<String>,
    pub achieved: bool,
    pub unlock_time: u64,
    pub present_in_local_save: bool,
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
    // names exist and which bits are already unlocked.
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

    // Critical safety property: we never send locked/false values and never
    // submit the full Steam achievement state. Only missing true unlocks are sent.
    if names_to_add.is_empty() {
        return Ok(0);
    }

    let verified = achievements::store_achievement_unlocks(
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
