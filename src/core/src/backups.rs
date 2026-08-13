use crate::{
    atomic,
    local::{identity_for_bytes, identity_for_path, unix_ms},
    model::{BackupRecord, BackupSource},
};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const RETAIN_PER_SLOT_AND_SOURCE: usize = 12;

pub struct BackupManager {
    root: PathBuf,
}

impl BackupManager {
    pub fn new(support_root: &Path) -> Self {
        Self {
            root: support_root.join("backups"),
        }
    }

    pub fn backup_file(
        &self,
        path: &Path,
        slot: u8,
        source: BackupSource,
        operation_id: &str,
    ) -> Result<BackupRecord> {
        let bytes =
            fs::read(path).with_context(|| format!("read backup source {}", path.display()))?;
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .context("backup filename")?;
        let actual = identity_for_path(path)?;
        let from_bytes = identity_for_bytes(&bytes);
        if actual.sha256 != from_bytes.sha256 {
            bail!("source changed while creating backup");
        }
        self.backup_bytes(&bytes, name, slot, source, operation_id, actual.steam_sha1)
    }

    pub fn backup_bytes(
        &self,
        bytes: &[u8],
        filename: &str,
        slot: u8,
        source: BackupSource,
        operation_id: &str,
        steam_sha1: Option<String>,
    ) -> Result<BackupRecord> {
        let backup_id = Uuid::new_v4().to_string();
        let directory = self.root.join(format!("slot{slot}"));
        fs::create_dir_all(&directory)?;
        let data_name = format!("{backup_id}-{filename}");
        let data_path = directory.join(&data_name);
        atomic::write_bytes(&data_path, bytes)?;

        let identity = identity_for_bytes(bytes);
        let record = BackupRecord {
            backup_id: backup_id.clone(),
            operation_id: operation_id.to_owned(),
            created_unix_ms: unix_ms(),
            source,
            slot,
            filename: filename.to_owned(),
            sha256: identity.sha256,
            steam_sha1: steam_sha1.or(identity.steam_sha1),
            size: bytes.len() as u64,
            data_relative_path: format!("slot{slot}/{data_name}"),
        };
        let manifest = directory.join(format!("{backup_id}.json"));
        atomic::write_bytes(&manifest, &serde_json::to_vec_pretty(&record)?)?;
        self.apply_retention(slot, &record.source)?;
        Ok(record)
    }

    pub fn list(&self) -> Result<Vec<BackupRecord>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for slot_dir in fs::read_dir(&self.root)? {
            let slot_dir = slot_dir?;
            if !slot_dir.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(slot_dir.path())? {
                let entry = entry?;
                if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(data) = fs::read(entry.path())
                    && let Ok(record) = serde_json::from_slice::<BackupRecord>(&data)
                {
                    records.push(record);
                }
            }
        }
        records.sort_by_key(|record| std::cmp::Reverse(record.created_unix_ms));
        Ok(records)
    }

    pub fn restore_local(&self, backup_id: &str, destination: &Path) -> Result<BackupRecord> {
        let record = self
            .list()?
            .into_iter()
            .find(|record| record.backup_id == backup_id)
            .context("backup not found")?;
        let data_path = self.verify(&record)?;
        if destination.exists() {
            self.backup_file(
                destination,
                record.slot,
                BackupSource::LocalBeforeRestore,
                &Uuid::new_v4().to_string(),
            )?;
        }
        atomic::replace_from_file(&data_path, destination)?;
        let restored = identity_for_path(destination)?;
        if restored.sha256 != record.sha256 {
            bail!("restored save verification failed");
        }
        Ok(record)
    }

    pub fn verify(&self, record: &BackupRecord) -> Result<PathBuf> {
        let data_path = self.root.join(&record.data_relative_path);
        let identity = identity_for_path(&data_path)?;
        if identity.sha256 != record.sha256 || identity.size != record.size {
            bail!("backup verification failed");
        }
        Ok(data_path)
    }

    fn apply_retention(&self, slot: u8, source: &BackupSource) -> Result<()> {
        let mut matching: Vec<_> = self
            .list()?
            .into_iter()
            .filter(|record| record.slot == slot && &record.source == source)
            .collect();
        matching.sort_by_key(|record| std::cmp::Reverse(record.created_unix_ms));
        for record in matching.into_iter().skip(RETAIN_PER_SLOT_AND_SOURCE) {
            let data = self.root.join(&record.data_relative_path);
            let manifest = self
                .root
                .join(format!("slot{slot}/{}.json", record.backup_id));
            // Retention only removes our verified duplicate copies, never a live save.
            let _ = fs::remove_file(data);
            let _ = fs::remove_file(manifest);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_and_restore_verify_content() {
        let root = std::env::temp_dir().join(format!("isaaccloud-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let live = root.join("rep_persistentgamedata1.dat");
        fs::write(&live, b"original").unwrap();
        let manager = BackupManager::new(&root.join("support"));
        let record = manager
            .backup_file(&live, 1, BackupSource::Manual, "test")
            .unwrap();
        fs::write(&live, b"changed").unwrap();
        manager.restore_local(&record.backup_id, &live).unwrap();
        assert_eq!(fs::read(&live).unwrap(), b"original");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn retention_keeps_twelve_versions_per_slot_and_source() {
        let root = std::env::temp_dir().join(format!("isaaccloud-retention-{}", Uuid::new_v4()));
        let manager = BackupManager::new(&root.join("support"));
        for index in 0..15 {
            manager
                .backup_bytes(
                    format!("version-{index}").as_bytes(),
                    "rep_persistentgamedata2.dat",
                    2,
                    BackupSource::Manual,
                    "retention-test",
                    None,
                )
                .unwrap();
        }
        let matching = manager
            .list()
            .unwrap()
            .into_iter()
            .filter(|record| record.slot == 2 && record.source == BackupSource::Manual)
            .count();
        assert_eq!(matching, RETAIN_PER_SLOT_AND_SOURCE);
        let _ = fs::remove_dir_all(root);
    }
}
