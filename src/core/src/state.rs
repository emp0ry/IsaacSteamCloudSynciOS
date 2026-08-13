use crate::{atomic, model::SyncState};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join("state/sync-state.json"),
        }
    }

    pub fn load(&self) -> Result<SyncState> {
        if !self.path.exists() {
            return Ok(SyncState::default());
        }
        let data = fs::read(&self.path)?;
        let mut state: SyncState = serde_json::from_slice(&data)
            .with_context(|| format!("decode {}", self.path.display()))?;
        if state.schema == 2 {
            // Schema 2 hashed native iOS raw-LZ4 bytes. Those hashes cannot be
            // compared to the canonical Steam/Windows identities introduced
            // by schema 3, and the old BASE bytes may no longer exist locally.
            // Clearing only per-file BASE records forces an explicit first-sync
            // choice while retaining the approved Steam account and queued
            // user actions. Never manufacture a replacement BASE.
            state.schema = crate::model::STATE_SCHEMA;
            state.files.clear();
            state.cloud_change_number = None;
        } else if state.schema != crate::model::STATE_SCHEMA {
            bail!("unsupported sync state schema {}", state.schema);
        }
        if state.app_id != crate::model::STEAM_APP_ID {
            bail!("sync state belongs to unexpected AppID {}", state.app_id);
        }
        Ok(state)
    }

    pub fn save(&self, state: &SyncState) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(state)?;
        atomic::write_bytes(&self.path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileIdentity, FileSyncState, STEAM_APP_ID};
    use std::collections::BTreeMap;
    use uuid::Uuid;

    #[test]
    fn schema_two_raw_lz4_base_is_cleared_without_losing_account() {
        let root = std::env::temp_dir().join(format!("isaaccloud-state-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("state")).unwrap();
        let identity = FileIdentity {
            sha256: "raw-lz4-hash".to_owned(),
            size: 123,
            modified_unix_ms: None,
            steam_sha1: Some("steam-sha1".to_owned()),
        };
        let mut files = BTreeMap::new();
        files.insert(
            "rep_persistentgamedata1.dat".to_owned(),
            FileSyncState {
                slot: 1,
                local_relative_path: "Documents/Repentance/rep_persistentgamedata1.dat".to_owned(),
                remote_name: "rep_persistentgamedata1.dat".to_owned(),
                base_sha256: identity.sha256.clone(),
                base_steam_sha1: identity.steam_sha1.clone(),
                last_local: identity.clone(),
                last_remote: identity,
                last_successful_sync_unix_ms: 1,
            },
        );
        let old = SyncState {
            schema: 2,
            app_id: STEAM_APP_ID,
            steam_id: Some(76561190000000000),
            account_name: Some("private-account".to_owned()),
            cloud_change_number: Some(42),
            files,
            pending_remote_pulls: BTreeMap::new(),
            pending_local_restore: None,
        };
        fs::write(
            root.join("state/sync-state.json"),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();

        let migrated = StateStore::new(&root).load().unwrap();
        assert_eq!(migrated.schema, crate::model::STATE_SCHEMA);
        assert_eq!(migrated.steam_id, old.steam_id);
        assert_eq!(migrated.account_name, old.account_name);
        assert!(migrated.files.is_empty());
        assert_eq!(migrated.cloud_change_number, None);
        let _ = fs::remove_dir_all(root);
    }
}
