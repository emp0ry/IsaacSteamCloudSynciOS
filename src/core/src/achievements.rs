use crate::{keychain, model::STEAM_APP_ID, steam::cm};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Mutex, OnceLock},
};
use steam_cm_protocol::{achievements, friends::ProtocolAchievement};

#[derive(Debug, Clone, Serialize)]
pub struct AchievementView {
    pub api_name: String,
    pub display_name: Option<String>,
    pub achieved: bool,
    pub unlock_time: u64,
    pub staged: bool,
}

static STAGED: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
static CACHE: OnceLock<Mutex<HashMap<String, ProtocolAchievement>>> = OnceLock::new();

fn staged() -> &'static Mutex<BTreeSet<String>> {
    STAGED.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn cache() -> &'static Mutex<HashMap<String, ProtocolAchievement>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn stage_gamekit_id(achievement_id: u32) -> bool {
    if achievement_id == 0 || achievement_id > 10_000 {
        return false;
    }
    staged()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(achievement_id.to_string());
    true
}

pub fn has_staged() -> bool {
    !staged()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_empty()
}

pub fn achievements_json() -> String {
    let staged = staged()
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
            staged: staged.contains(&achievement.apiname),
        })
        .collect();
    values.sort_by(|left, right| {
        numeric_api_name(&left.api_name)
            .cmp(&numeric_api_name(&right.api_name))
            .then_with(|| left.api_name.cmp(&right.api_name))
    });
    serde_json::to_string(&values).unwrap_or_else(|_| "[]".to_owned())
}

pub async fn sync_staged() -> Result<usize> {
    let token = keychain::load_refresh_token()?
        .context("Connect Steam in IsaacSteamCloudSync before syncing achievements")?;
    let session = cm::connect_with_refresh_token(&token)
        .await
        .context("connect Steam CM for achievement sync")?;
    let state = session.connection.state_snapshot().await;
    let current = achievements::get_player_achievements(&session.connection, &state, STEAM_APP_ID)
        .await
        .context("request Isaac Steam achievement schema")?;
    replace_cache(current);

    let names = {
        let known = cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let staged = staged()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        staged
            .iter()
            .filter(|name| known.get(*name).is_some_and(|item| !item.achieved))
            .cloned()
            .collect::<Vec<_>>()
    };

    if names.is_empty() {
        clear_already_unlocked();
        return Ok(0);
    }

    let verified = achievements::store_achievement_unlocks(
        &session.connection,
        &state,
        STEAM_APP_ID,
        &names,
    )
    .await
    .context("store and verify Isaac achievements")?;
    replace_cache(verified);

    let verified_count = {
        let known = cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        names
            .iter()
            .filter(|name| known.get(*name).is_some_and(|item| item.achieved))
            .count()
    };
    if verified_count != names.len() {
        bail!(
            "Steam verified only {verified_count} of {} staged achievements",
            names.len()
        );
    }
    clear_already_unlocked();
    Ok(verified_count)
}

fn replace_cache(values: Vec<ProtocolAchievement>) {
    let mut target = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    target.clear();
    target.extend(values.into_iter().map(|item| (item.apiname.clone(), item)));
}

fn clear_already_unlocked() {
    let known = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut staged = staged()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    staged.retain(|name| !known.get(name).is_some_and(|item| item.achieved));
}

fn numeric_api_name(value: &str) -> u64 {
    value.parse().unwrap_or(u64::MAX)
}
