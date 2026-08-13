use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

pub const STEAM_APP_ID: u32 = 250_900;
pub const STATE_SCHEMA: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileIdentity {
    pub sha256: String,
    pub size: u64,
    pub modified_unix_ms: Option<u64>,
    pub steam_sha1: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileSyncState {
    pub slot: u8,
    pub local_relative_path: String,
    pub remote_name: String,
    pub base_sha256: String,
    pub base_steam_sha1: Option<String>,
    pub last_local: FileIdentity,
    pub last_remote: FileIdentity,
    pub last_successful_sync_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingRemotePull {
    pub slot: u8,
    pub remote_name: String,
    pub expected_steam_sha1: Option<String>,
    pub requested_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingLocalRestore {
    pub backup_id: String,
    pub slot: u8,
    pub expected_sha256: String,
    pub requested_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncState {
    pub schema: u32,
    pub app_id: u32,
    pub steam_id: Option<u64>,
    pub account_name: Option<String>,
    pub cloud_change_number: Option<u64>,
    pub files: BTreeMap<String, FileSyncState>,
    #[serde(default)]
    pub pending_remote_pulls: BTreeMap<u8, PendingRemotePull>,
    #[serde(default)]
    pub pending_local_restore: Option<PendingLocalRestore>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            schema: STATE_SCHEMA,
            app_id: STEAM_APP_ID,
            steam_id: None,
            account_name: None,
            cloud_change_number: None,
            files: BTreeMap::new(),
            pending_remote_pulls: BTreeMap::new(),
            pending_local_restore: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackupSource {
    LocalBeforePull,
    LocalBeforeRestore,
    LocalBeforeForce,
    LocalFirstSync,
    LocalConflict,
    SteamBeforePush,
    SteamBeforeForce,
    SteamFirstSync,
    SteamConflict,
    SteamInvalid,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupRecord {
    pub backup_id: String,
    pub operation_id: String,
    pub created_unix_ms: u64,
    pub source: BackupSource,
    pub slot: u8,
    pub filename: String,
    pub sha256: String,
    pub steam_sha1: Option<String>,
    pub size: u64,
    pub data_relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSave {
    pub slot: u8,
    pub path: PathBuf,
    pub relative_path: String,
    pub identity: FileIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSave {
    pub slot: u8,
    pub filename: String,
    pub path_prefix: String,
    pub identity: FileIdentity,
}
