use crate::{
    achievements, atomic,
    backups::BackupManager,
    isaac_format::{SaveEncoding, canonical_identity_for_path, canonicalize_save},
    keychain,
    local::{
        discover_saves, discover_saves_for_slots, discover_saves_immediate, identity_for_bytes,
        identity_for_path, locate_save_path_for_restore, unique_temp_path, unix_ms,
        wait_for_stable_snapshot,
    },
    logging::Logger,
    model::{
        BackupSource, FileIdentity, FileSyncState, LocalSave, PendingLocalRestore,
        PendingRemotePull, SyncState,
    },
    state::StateStore,
    steam::{
        cloud::{CloudFile, SteamCloud},
        cm::{self, SteamSession},
    },
    sync_engine::{SyncDecision, decide},
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    runtime::Runtime,
    sync::{Mutex as AsyncMutex, mpsc},
};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Debug, Clone, Serialize)]
pub struct SaveSummary {
    pub slot: u8,
    pub filename: String,
    pub size: u64,
    pub modified_unix_ms: Option<u64>,
    pub sha256: Option<String>,
    pub steam_sha1: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingChoice {
    pub slot: u8,
    pub kind: String,
    pub local: SaveSummary,
    pub remote: SaveSummary,
    pub remote_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusSnapshot {
    pub phase: String,
    pub detail: String,
    pub authenticated: bool,
    pub account_connected: bool,
    pub steam_id: Option<u64>,
    pub steam_playing: bool,
    pub qr_url: Option<String>,
    pub guard_kind: Option<String>,
    pub last_sync_unix_ms: Option<u64>,
    pub local_saves: Vec<SaveSummary>,
    pub remote_saves: Vec<SaveSummary>,
    pub pending_choices: Vec<PendingChoice>,
    pub backup_count: usize,
    pub offline_play_allowed: bool,
}

impl Default for StatusSnapshot {
    fn default() -> Self {
        Self {
            phase: "idle".to_owned(),
            detail: "Not connected".to_owned(),
            authenticated: false,
            account_connected: false,
            steam_id: None,
            steam_playing: false,
            qr_url: None,
            guard_kind: None,
            last_sync_unix_ms: None,
            local_saves: vec![],
            remote_saves: vec![],
            pending_choices: vec![],
            backup_count: 0,
            offline_play_allowed: true,
        }
    }
}

pub struct Engine {
    home: PathBuf,
    support: PathBuf,
    runtime: Runtime,
    session: AsyncMutex<Option<SteamSession>>,
    guard_sender: Mutex<Option<mpsc::UnboundedSender<String>>>,
    auth_cancel: Mutex<Option<Arc<AtomicBool>>>,
    status: Mutex<StatusSnapshot>,
    preflight_saves: Mutex<Option<Vec<LocalSave>>>,
    operation_active: AtomicBool,
    foreground: AtomicBool,
    replacement_allowed: AtomicBool,
    replacement_guard: Mutex<()>,
    logger: Logger,
}

impl Engine {
    pub fn new(home: PathBuf) -> Result<Arc<Self>> {
        let support = home.join("Library/Application Support/IsaacCloudSync");
        for directory in ["state", "backups", "logs", "tmp"] {
            fs::create_dir_all(support.join(directory))?;
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("IsaacCloudCore")
            .build()?;
        let logger = Logger::new(&support);
        logger.log("info", "bootstrap", "portable core initialized");
        let mut initial_status = StatusSnapshot::default();
        if matches!(keychain::load_refresh_token(), Ok(Some(_))) {
            initial_status.account_connected = true;
            initial_status.detail = "Steam account connected".to_owned();
        }
        let engine = Arc::new(Self {
            home,
            support,
            runtime,
            session: AsyncMutex::new(None),
            guard_sender: Mutex::new(None),
            auth_cancel: Mutex::new(None),
            status: Mutex::new(initial_status),
            preflight_saves: Mutex::new(None),
            operation_active: AtomicBool::new(false),
            foreground: AtomicBool::new(true),
            replacement_allowed: AtomicBool::new(false),
            replacement_guard: Mutex::new(()),
            logger,
        });
        match discover_saves_immediate(&engine.home).and_then(Self::ensure_unique_slots) {
            Ok(saves) => {
                engine.logger.log(
                    "info",
                    "preflight",
                    &format!(
                        "captured {} stable local save identities during dylib construction",
                        saves.len()
                    ),
                );
                *engine
                    .preflight_saves
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(saves);
            }
            Err(error) => engine.logger.log(
                "error",
                "preflight",
                &format!("constructor save capture failed: {}", safe_error(&error)),
            ),
        }
        Ok(engine)
    }

    pub fn snapshot(&self) -> StatusSnapshot {
        self.status
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default()
    }

    pub fn backups_json(&self) -> Result<String> {
        Ok(serde_json::to_string(
            &BackupManager::new(&self.support).list()?,
        )?)
    }

    pub fn record_host_event(&self, category: &str, message: &str) {
        self.logger.log("info", category, message);
    }

    pub fn connect_qr(self: &Arc<Self>) -> bool {
        let cancelled = Arc::new(AtomicBool::new(false));
        let previous_cancel = self
            .auth_cancel
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(Arc::clone(&cancelled));
        let started = self.spawn_operation("connecting", |engine| async move {
            engine.update_status(|status| {
                status.detail = "Connecting to Steam…".to_owned();
                status.qr_url = None;
                status.guard_kind = None;
            });
            let callback_engine = Arc::clone(&engine);
            let result = cm::connect_with_qr(
                move |url| {
                    callback_engine.update_status(|status| {
                        status.phase = "awaiting_steam_guard".to_owned();
                        status.detail = "Approve this sign-in with Steam Mobile".to_owned();
                        status.qr_url = Some(url.to_owned());
                        status.guard_kind = Some("qr".to_owned());
                    });
                },
                Arc::clone(&cancelled),
            )
            .await;
            *engine
                .auth_cancel
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            if cancelled.load(Ordering::Acquire) {
                engine.update_status(|status| {
                    status.phase = "idle".to_owned();
                    status.detail = "Steam sign-in cancelled".to_owned();
                    status.qr_url = None;
                    status.guard_kind = None;
                });
                return Ok(());
            }
            let (session, refresh_token) = result?;
            keychain::store_refresh_token(&refresh_token)?;
            engine.finish_login(session).await?;
            engine.run_sync("connect").await
        });
        if !started {
            *self
                .auth_cancel
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = previous_cancel;
        }
        started
    }

    pub fn connect_credentials(self: &Arc<Self>, account: String, password: String) -> bool {
        let cancelled = Arc::new(AtomicBool::new(false));
        let previous_cancel = self
            .auth_cancel
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(Arc::clone(&cancelled));
        let (sender, receiver) = mpsc::unbounded_channel();
        *self
            .guard_sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sender);
        let started = self.spawn_operation("connecting", move |engine| async move {
            engine.update_status(|status| {
                status.detail = "Signing in to Steam…".to_owned();
                status.qr_url = None;
                status.guard_kind = None;
            });
            let password = Zeroizing::new(password);
            let callback_engine = Arc::clone(&engine);
            let result = cm::connect_with_credentials(
                &account,
                &password,
                move |prompt| {
                    callback_engine.update_status(|status| {
                        status.phase = "awaiting_steam_guard".to_owned();
                        status.qr_url = None;
                        match prompt {
                            cm::CredentialPrompt::EmailCode => {
                                status.guard_kind = Some("email_code".to_owned());
                                status.detail = "Enter the Steam Guard email code".to_owned();
                            }
                            cm::CredentialPrompt::DeviceCode => {
                                status.guard_kind = Some("device_code".to_owned());
                                status.detail = "Enter the Steam Guard code".to_owned();
                            }
                            cm::CredentialPrompt::DeviceConfirmation => {
                                status.guard_kind = Some("device_confirmation".to_owned());
                                status.detail = "Approve this sign-in in Steam Mobile".to_owned();
                            }
                        }
                    });
                },
                receiver,
                Arc::clone(&cancelled),
            )
            .await;
            *engine
                .guard_sender
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            *engine
                .auth_cancel
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            if cancelled.load(Ordering::Acquire) {
                engine.update_status(|status| {
                    status.phase = "idle".to_owned();
                    status.detail = "Steam sign-in cancelled".to_owned();
                    status.qr_url = None;
                    status.guard_kind = None;
                });
                return Ok(());
            }
            let (session, refresh_token) = result?;
            keychain::store_refresh_token(&refresh_token)?;
            engine.finish_login(session).await?;
            engine.run_sync("connect").await
        });
        if !started {
            *self
                .guard_sender
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            *self
                .auth_cancel
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = previous_cancel;
        }
        started
    }

    pub fn cancel_login(&self) -> bool {
        let cancelled = self
            .auth_cancel
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(cancelled) = cancelled else {
            return false;
        };
        cancelled.store(true, Ordering::Release);
        self.guard_sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        self.update_status(|status| {
            status.phase = "cancelling".to_owned();
            status.detail = "Cancelling Steam sign-in…".to_owned();
            status.qr_url = None;
            status.guard_kind = None;
        });
        true
    }

    pub fn submit_guard_code(&self, code: String) -> bool {
        let sender = self
            .guard_sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        sender.is_some_and(|sender| sender.send(code).is_ok())
    }

    pub fn set_foreground(self: &Arc<Self>, foreground: bool) -> bool {
        self.foreground.store(foreground, Ordering::Release);
        let engine = Arc::clone(self);
        self.runtime.spawn(async move {
            let result = if foreground {
                engine.ensure_session().await
            } else {
                let session = engine.session.lock().await;
                match session.as_ref() {
                    Some(session) => cm::set_playing(session, false).await,
                    None => Ok(()),
                }
            };
            match result {
                Ok(()) => {
                    engine.update_status(|status| status.steam_playing = foreground);
                    engine.logger.log(
                        "info",
                        "presence",
                        if foreground {
                            "AppID 250900 presence active"
                        } else {
                            "AppID 250900 presence cleared"
                        },
                    );
                }
                Err(error) => engine.logger.log(
                    "error",
                    "presence",
                    &format!("presence update failed: {}", safe_error(&error)),
                ),
            }
        });
        true
    }

    pub fn automatic_sync(self: &Arc<Self>, trigger: &str) -> bool {
        let trigger = trigger.to_owned();
        self.spawn_operation("syncing", move |engine| async move {
            if let Err(error) = engine.ensure_session().await {
                engine.logger.log(
                    "info",
                    "offline",
                    "Steam unavailable; continuing with local save",
                );
                engine.update_status(|status| {
                    status.phase = "offline".to_owned();
                    status.detail = safe_error(&error);
                    status.offline_play_allowed = true;
                });
                return Ok(());
            }
            engine.run_sync(&trigger).await?;
            if trigger == "manual" {
                match engine.run_achievement_sync().await {
                    Ok(added) => engine.report_achievement_result(added),
                    Err(error) => {
                        let message = safe_error(&error);
                        engine.logger.log(
                            "error",
                            "achievements",
                            &format!("save-derived achievement sync failed: {message}"),
                        );
                        engine.update_status(|status| {
                            if status.phase == "idle" || status.phase == "syncing" {
                                status.phase = "idle".to_owned();
                                status.detail = format!(
                                    "Steam Cloud synchronized; achievement sync failed: {message}"
                                );
                            }
                        });
                    }
                }
            }
            Ok(())
        })
    }

    /// Synchronize achievements through the same serialized Steam operation
    /// lane as UFS. This prevents a second CM login from racing the Cloud
    /// session and potentially causing Steam to close either connection.
    pub fn sync_achievements(self: &Arc<Self>) -> bool {
        self.spawn_operation("syncing", |engine| async move {
            engine.ensure_session().await?;
            let added = engine.run_achievement_sync().await?;
            engine.report_achievement_result(added);
            Ok(())
        })
    }

    pub fn disconnect(self: &Arc<Self>) -> bool {
        self.spawn_operation("disconnecting", |engine| async move {
            keychain::delete_refresh_token()?;
            let mut session = engine.session.lock().await;
            if let Some(existing) = session.as_ref() {
                let _ = cm::set_playing(existing, false).await;
            }
            *session = None;
            drop(session);
            engine.update_status(|status| {
                *status = StatusSnapshot::default();
                status.detail = "Disconnected from Steam".to_owned();
            });
            engine
                .logger
                .log("info", "authentication", "Steam session disconnected");
            Ok(())
        })
    }

    pub fn resolve_choice(self: &Arc<Self>, slot: u8, use_local: bool) -> bool {
        self.spawn_operation("resolving", move |engine| async move {
            engine.ensure_session().await?;
            engine.force_slot(slot, use_local, true).await?;
            if use_local {
                engine.run_sync("resolution").await?;
            }
            Ok(())
        })
    }

    pub fn force(self: &Arc<Self>, slot: u8, use_local: bool) -> bool {
        self.spawn_operation("forcing", move |engine| async move {
            engine.ensure_session().await?;
            engine.force_slot(slot, use_local, false).await
        })
    }

    pub fn restore_backup(self: &Arc<Self>, backup_id: String) -> bool {
        self.spawn_operation("restoring", move |engine| async move {
            let manager = BackupManager::new(&engine.support);
            let record = manager
                .list()?
                .into_iter()
                .find(|record| record.backup_id == backup_id)
                .context("backup not found")?;
            let backup_path = manager.verify(&record)?;
            canonicalize_save(&fs::read(backup_path)?)
                .context("backup is not a valid Repentance save and cannot be restored")?;
            let store = StateStore::new(&engine.support);
            let mut state = store.load()?;
            state.pending_local_restore = Some(PendingLocalRestore {
                backup_id,
                slot: record.slot,
                expected_sha256: record.sha256,
                requested_unix_ms: unix_ms(),
            });
            store.save(&state)?;
            engine.update_status(|status| {
                status.phase = "restart_required".to_owned();
                status.detail =
                    "Backup verified and queued; relaunch Isaac to restore it safely".to_owned();
            });
            engine.logger.log(
                "info",
                "backup",
                "verified backup queued for prelaunch restore",
            );
            Ok(())
        })
    }

    pub fn preflight(self: &Arc<Self>, timeout_ms: u64) -> bool {
        self.replacement_allowed.store(true, Ordering::Release);
        if let Err(error) = self.apply_pending_restore() {
            self.logger.log(
                "error",
                "backup",
                &format!("queued restore was not applied: {}", safe_error(&error)),
            );
        }
        let started = self.automatic_sync("prelaunch");
        if !started {
            self.replacement_allowed.store(false, Ordering::Release);
            return true;
        }
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while self.operation_active.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        let completed = !self.operation_active.load(Ordering::Acquire);
        // A pull holds this guard from its final permission check until the
        // atomic replacement is durable. Once this lock is acquired no save
        // can be replaced after the prelaunch barrier returns.
        let _guard = self
            .replacement_guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.replacement_allowed.store(false, Ordering::Release);
        completed
    }

    fn apply_pending_restore(&self) -> Result<()> {
        let store = StateStore::new(&self.support);
        let mut state = store.load()?;
        let Some(pending) = state.pending_local_restore.clone() else {
            return Ok(());
        };
        let manager = BackupManager::new(&self.support);
        let record = manager
            .list()?
            .into_iter()
            .find(|record| record.backup_id == pending.backup_id)
            .context("queued backup no longer exists")?;
        if record.slot != pending.slot || record.sha256 != pending.expected_sha256 {
            bail!("queued backup manifest no longer matches the approved version");
        }
        let backup_path = manager.verify(&record)?;
        canonicalize_save(&fs::read(backup_path)?)
            .context("queued backup is not a valid Repentance save")?;
        let destination = locate_save_path_for_restore(&self.home, pending.slot, &record.filename)?;
        let _replacement_guard = self
            .replacement_guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.replacement_allowed.load(Ordering::Acquire) {
            bail!("queued restore is only allowed during prelaunch");
        }
        manager.restore_local(&pending.backup_id, &destination)?;
        let restored_identity = canonical_identity_for_path(&destination)?;
        let restored = LocalSave {
            slot: pending.slot,
            relative_path: destination
                .strip_prefix(&self.home)
                .unwrap_or(&destination)
                .to_string_lossy()
                .into_owned(),
            path: destination,
            identity: restored_identity,
        };
        let mut preflight = self
            .preflight_saves
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saves = preflight.get_or_insert_with(Vec::new);
        saves.retain(|save| save.slot != pending.slot);
        saves.push(restored);
        saves.sort_by_key(|save| save.slot);
        state.pending_local_restore = None;
        store.save(&state)?;
        self.logger.log(
            "info",
            "backup",
            "queued backup restored atomically and verified before launch",
        );
        Ok(())
    }

    fn spawn_operation<F, Fut>(self: &Arc<Self>, phase: &str, build: F) -> bool
    where
        F: FnOnce(Arc<Engine>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        if self
            .operation_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.update_status(|status| {
            status.phase = phase.to_owned();
            status.detail = phase.to_owned();
        });
        let engine = Arc::clone(self);
        self.runtime.spawn(async move {
            let result = build(Arc::clone(&engine)).await;
            if let Err(error) = result {
                // A failed CM/UFS operation may leave the transport alive but
                // unusable. Drop it so the next attempt performs a clean
                // refresh-token reconnect. The Keychain credential is kept.
                *engine.session.lock().await = None;
                let message = safe_error(&error);
                engine.logger.log("error", "operation", &message);
                engine.update_status(|status| {
                    status.phase = "error".to_owned();
                    status.detail = message;
                    status.offline_play_allowed = true;
                    status.authenticated = false;
                    status.steam_playing = false;
                });
            } else {
                engine.update_status(|status| {
                    if status.phase != "conflict"
                        && status.phase != "first_sync"
                        && status.phase != "offline"
                        && status.phase != "restart_required"
                        && status.phase != "upload_required"
                    {
                        status.phase = "idle".to_owned();
                    }
                });
            }
            engine.operation_active.store(false, Ordering::Release);
        });
        true
    }

    async fn ensure_session(&self) -> Result<()> {
        let mut session = self.session.lock().await;
        if let Some(existing) = session.as_ref()
            && !existing.connection.is_closed().await
        {
            self.apply_presence(existing).await?;
            return Ok(());
        }
        self.logger.log(
            "info",
            "authentication",
            "loading refresh credential from Keychain",
        );
        let token = keychain::load_refresh_token()?
            .context("Connect a Steam account to enable cloud sync")?;
        self.logger.log(
            "info",
            "authentication",
            "refresh credential loaded; connecting to Steam CM",
        );
        let connected = cm::connect_with_refresh_token(&token).await?;
        self.logger.log(
            "info",
            "authentication",
            "refresh-token Steam CM login succeeded",
        );
        let steam_id = connected.steam_id;
        self.bind_state_to_account(steam_id, None)?;
        self.apply_presence(&connected).await?;
        *session = Some(connected);
        self.update_status(|status| {
            status.authenticated = true;
            status.account_connected = true;
            status.steam_id = Some(steam_id);
            status.qr_url = None;
            status.guard_kind = None;
            status.detail = "Steam connected".to_owned();
        });
        Ok(())
    }

    async fn finish_login(&self, session: SteamSession) -> Result<()> {
        let steam_id = session.steam_id;
        let account_name = session.account_name.clone();
        self.bind_state_to_account(steam_id, (!account_name.is_empty()).then_some(account_name))?;
        self.apply_presence(&session).await?;
        *self.session.lock().await = Some(session);
        self.update_status(|status| {
            status.authenticated = true;
            status.account_connected = true;
            status.steam_id = Some(steam_id);
            status.qr_url = None;
            status.guard_kind = None;
            status.detail = "Steam connected".to_owned();
        });
        self.logger
            .log("info", "authentication", "Steam authentication succeeded");
        Ok(())
    }

    async fn apply_presence(&self, session: &SteamSession) -> Result<()> {
        let playing = self.foreground.load(Ordering::Acquire);
        cm::set_playing(session, playing).await?;
        self.update_status(|status| status.steam_playing = playing);
        Ok(())
    }

    async fn run_achievement_sync(&self) -> Result<usize> {
        self.logger.log(
            "info",
            "achievements",
            "reading unlocks from native Isaac persistent saves",
        );
        self.update_status(|status| {
            if status.phase == "idle" {
                status.phase = "syncing".to_owned();
                status.detail = "Comparing native save achievements with Steam…".to_owned();
            }
        });
        let session_guard = self.session.lock().await;
        let session = session_guard
            .as_ref()
            .context("Steam session unavailable for achievement sync")?;
        achievements::sync_from_local_saves(&self.home, session).await
    }

    fn report_achievement_result(&self, added: usize) {
        self.logger.log(
            "info",
            "achievements",
            &format!("save-derived achievement sync verified; added={added}; cleared=0"),
        );
        self.update_status(|status| {
            if status.phase == "idle" || status.phase == "syncing" {
                status.detail = if added == 0 {
                    "Steam Cloud and save-derived achievements are synchronized".to_owned()
                } else if added == 1 {
                    "Steam Cloud synchronized; 1 missing save achievement added to Steam".to_owned()
                } else {
                    format!(
                        "Steam Cloud synchronized; {added} missing save achievements added to Steam"
                    )
                };
            }
        });
    }

    fn bind_state_to_account(&self, steam_id: u64, account_name: Option<String>) -> Result<()> {
        let store = StateStore::new(&self.support);
        let mut state = store.load()?;
        if state.steam_id.is_some_and(|prior| prior != steam_id) {
            state.files.clear();
            state.cloud_change_number = None;
            state.pending_remote_pulls.clear();
            self.logger.log(
                "info",
                "authentication",
                "Steam account changed; synchronization BASE was cleared",
            );
        }
        state.steam_id = Some(steam_id);
        if account_name.is_some() {
            state.account_name = account_name;
        }
        store.save(&state)?;
        Ok(())
    }

    fn local_saves(&self) -> Result<Vec<LocalSave>> {
        let saves = discover_saves(&self.home)?;
        Self::ensure_unique_slots(saves)
    }

    fn local_saves_for_slots(&self, slots: &BTreeSet<u8>) -> Result<Vec<LocalSave>> {
        let saves = discover_saves_for_slots(&self.home, Some(slots))?;
        Self::ensure_unique_slots(saves)
    }

    fn ensure_unique_slots(saves: Vec<LocalSave>) -> Result<Vec<LocalSave>> {
        for pair in saves.windows(2) {
            if pair[0].slot == pair[1].slot {
                bail!(
                    "multiple local save candidates found for slot {}",
                    pair[0].slot
                );
            }
        }
        Ok(saves)
    }

    async fn run_sync(&self, trigger: &str) -> Result<()> {
        let allow_upload = trigger == "manual";
        self.logger.log("info", "lifecycle", trigger);
        self.update_status(|status| {
            status.phase = "syncing".to_owned();
            status.detail = "Discovering Isaac saves…".to_owned();
            status.pending_choices.clear();
        });
        let cloud = SteamCloud::new()?;
        let session_guard = self.session.lock().await;
        let session = session_guard
            .as_ref()
            .context("Steam session unavailable")?;
        let (change_number, remote) = cloud.list_isaac_saves(session).await?;
        let local = if trigger == "prelaunch" {
            self.preflight_saves
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .context("early preflight save snapshot unavailable")?
        } else if allow_upload {
            self.local_saves()?
        } else {
            let remote_slots = remote.iter().map(|file| file.save.slot).collect();
            self.local_saves_for_slots(&remote_slots)?
        };
        let local = if trigger == "prelaunch" {
            local
                .into_iter()
                .filter(|save| remote.iter().any(|file| file.save.slot == save.slot))
                .collect()
        } else {
            local
        };
        if local.is_empty() {
            bail!("no matching Isaac persistent save files were found inside the app sandbox");
        }
        for file in &remote {
            self.logger.log(
                "info",
                "cloud",
                &format!(
                    "slot={} remote_name={} size={} steam_sha1={}",
                    file.save.slot,
                    file.request_name,
                    file.save.identity.size,
                    file.save
                        .identity
                        .steam_sha1
                        .as_deref()
                        .map(short_hash)
                        .unwrap_or("unavailable")
                ),
            );
        }
        self.update_status(|status| {
            status.local_saves = local.iter().map(local_summary).collect();
            status.remote_saves = remote.iter().map(remote_summary).collect();
            status.detail = "Comparing local, Steam, and base hashes…".to_owned();
        });

        let store = StateStore::new(&self.support);
        let mut state = store.load()?;
        let mut pending = Vec::new();
        let mut uploads_waiting = 0usize;
        let operation_id = Uuid::new_v4().to_string();

        for local_save in &local {
            let Some(remote_file) = remote.iter().find(|file| file.save.slot == local_save.slot)
            else {
                BackupManager::new(&self.support).backup_file(
                    &local_save.path,
                    local_save.slot,
                    BackupSource::LocalFirstSync,
                    &operation_id,
                )?;
                pending.push(missing_remote_choice(local_save));
                continue;
            };
            let prior = state
                .files
                .values()
                .find(|entry| entry.slot == local_save.slot)
                .cloned();
            // Saves are tiny, so download every candidate. Besides providing a
            // SHA-256 identity, this detects the raw-LZ4 representation written
            // by native iOS and prevents it from ever being sent to Windows.
            let remote_raw_bytes = cloud.download(session, remote_file).await?;
            let remote_canonical = match canonicalize_save(&remote_raw_bytes) {
                Ok(save) => save,
                Err(error) => {
                    BackupManager::new(&self.support).backup_bytes(
                        &remote_raw_bytes,
                        &remote_file.save.filename,
                        local_save.slot,
                        BackupSource::SteamInvalid,
                        &operation_id,
                        remote_file.save.identity.steam_sha1.clone(),
                    )?;
                    return Err(error.context(format!(
                        "Steam save slot {} is not a valid canonical or iOS LZ4 Repentance save; exact cloud bytes were backed up",
                        local_save.slot
                    )));
                }
            };
            let remote_needs_compatibility_repair =
                remote_canonical.encoding == SaveEncoding::IosRawLz4;
            let mut remote_identity = identity_for_bytes(&remote_canonical.bytes);
            remote_identity.modified_unix_ms = remote_file.save.identity.modified_unix_ms;
            remote_identity.steam_sha1 = remote_file.save.identity.steam_sha1.clone();
            if remote_needs_compatibility_repair {
                self.logger.log(
                    "warn",
                    "compatibility",
                    &format!(
                        "slot={} Steam copy contains iOS LZ4 framing; canonical repair is required",
                        local_save.slot
                    ),
                );
            }

            if let Some(requested) = state.pending_remote_pulls.get(&local_save.slot).cloned() {
                let still_same_remote = requested.remote_name == remote_file.request_name
                    && requested.expected_steam_sha1 == remote_identity.steam_sha1;
                if still_same_remote && self.replacement_allowed.load(Ordering::Acquire) {
                    self.pull_one(
                        local_save,
                        remote_file,
                        &remote_canonical.bytes,
                        &operation_id,
                        &mut state,
                    )?;
                    state.pending_remote_pulls.remove(&local_save.slot);
                    continue;
                }
                if !still_same_remote {
                    state.pending_remote_pulls.remove(&local_save.slot);
                    self.logger.log(
                        "info",
                        "download",
                        "queued Steam version changed; requesting a new choice",
                    );
                } else {
                    pending.push(choice_summary(
                        local_save,
                        remote_file,
                        &remote_identity,
                        "remote_update_restart_required",
                    ));
                    continue;
                }
            }
            let decision = decide(
                prior.as_ref().map(|entry| entry.base_sha256.as_str()),
                &local_save.identity.sha256,
                &remote_identity.sha256,
            );
            self.logger.log(
                "info",
                "comparison",
                &format!(
                    "slot={} local_sha256={} remote_sha256={} base_sha256={} result={}",
                    local_save.slot,
                    short_hash(&local_save.identity.sha256),
                    short_hash(&remote_identity.sha256),
                    prior
                        .as_ref()
                        .map(|entry| short_hash(&entry.base_sha256))
                        .unwrap_or("none"),
                    decision_label(decision)
                ),
            );

            match decision {
                SyncDecision::Unchanged => {
                    if remote_needs_compatibility_repair {
                        if allow_upload {
                            self.push_one(
                                &cloud,
                                session,
                                local_save,
                                remote_file,
                                &remote_raw_bytes,
                                &operation_id,
                                &mut state,
                            )
                            .await?;
                        } else {
                            uploads_waiting += 1;
                        }
                    }
                }
                SyncDecision::Converged => {
                    if remote_needs_compatibility_repair && allow_upload {
                        self.push_one(
                            &cloud,
                            session,
                            local_save,
                            remote_file,
                            &remote_raw_bytes,
                            &operation_id,
                            &mut state,
                        )
                        .await?;
                    } else {
                        upsert_state(
                            &mut state,
                            make_file_state(local_save, remote_file, &remote_identity),
                        );
                        if remote_needs_compatibility_repair {
                            uploads_waiting += 1;
                        }
                    }
                }
                SyncDecision::UploadLocal => {
                    if allow_upload {
                        self.push_one(
                            &cloud,
                            session,
                            local_save,
                            remote_file,
                            &remote_raw_bytes,
                            &operation_id,
                            &mut state,
                        )
                        .await?;
                    } else {
                        uploads_waiting += 1;
                        self.logger.log(
                            "info",
                            "upload",
                            &format!(
                                "slot={} iPhone change is waiting for explicit Sync Now",
                                local_save.slot
                            ),
                        );
                    }
                }
                SyncDecision::DownloadRemote => {
                    if self.replacement_allowed.load(Ordering::Acquire) {
                        self.pull_one(
                            local_save,
                            remote_file,
                            &remote_canonical.bytes,
                            &operation_id,
                            &mut state,
                        )?;
                    } else {
                        pending.push(choice_summary(
                            local_save,
                            remote_file,
                            &remote_identity,
                            "remote_update_restart_required",
                        ));
                    }
                }
                SyncDecision::FirstSyncChoiceRequired | SyncDecision::Conflict => {
                    self.backup_both(
                        local_save,
                        remote_file,
                        &remote_raw_bytes,
                        &operation_id,
                        if decision == SyncDecision::FirstSyncChoiceRequired {
                            (BackupSource::LocalFirstSync, BackupSource::SteamFirstSync)
                        } else {
                            (BackupSource::LocalConflict, BackupSource::SteamConflict)
                        },
                    )?;
                    pending.push(choice_summary(
                        local_save,
                        remote_file,
                        &remote_identity,
                        if decision == SyncDecision::FirstSyncChoiceRequired {
                            "first_sync"
                        } else {
                            "conflict"
                        },
                    ));
                }
            }
        }

        state.cloud_change_number = Some(change_number);
        store.save(&state)?;
        let backup_count = BackupManager::new(&self.support).list()?.len();
        self.update_status(|status| {
            status.pending_choices = pending.clone();
            status.backup_count = backup_count;
            status.last_sync_unix_ms = Some(unix_ms());
            if pending.is_empty() && uploads_waiting == 0 {
                status.phase = "idle".to_owned();
                status.detail = "Steam Cloud is synchronized".to_owned();
            } else if pending.iter().any(|choice| choice.kind == "conflict") {
                status.phase = "conflict".to_owned();
                status.detail = "A true save conflict requires your choice".to_owned();
            } else if pending
                .iter()
                .any(|choice| choice.kind == "remote_update_restart_required")
            {
                status.phase = "restart_required".to_owned();
                status.detail = "Relaunch Isaac to apply the selected Steam save safely".to_owned();
            } else if uploads_waiting > 0 {
                status.phase = "upload_required".to_owned();
                status.detail = if uploads_waiting == 1 {
                    "iPhone progress is waiting; tap Sync Now to upload a Windows-compatible save"
                        .to_owned()
                } else {
                    format!(
                        "{uploads_waiting} iPhone saves are waiting; tap Sync Now to upload them"
                    )
                };
            } else {
                status.phase = "first_sync".to_owned();
                status.detail = "First sync requires your choice".to_owned();
            }
        });
        Ok(())
    }

    async fn force_slot(&self, slot: u8, use_local: bool, must_be_pending: bool) -> Result<()> {
        if must_be_pending
            && !self
                .snapshot()
                .pending_choices
                .iter()
                .any(|choice| choice.slot == slot)
        {
            bail!("save slot does not have a pending conflict or first-sync choice");
        }
        let allowed_slots = BTreeSet::from([slot]);
        let local = self
            .local_saves_for_slots(&allowed_slots)?
            .into_iter()
            .find(|save| save.slot == slot)
            .context("local save slot not found")?;
        let cloud = SteamCloud::new()?;
        let session_guard = self.session.lock().await;
        let session = session_guard
            .as_ref()
            .context("Steam session unavailable")?;
        let (_, remote) = cloud.list_isaac_saves(session).await?;
        let remote = remote.iter().find(|file| file.save.slot == slot);
        let operation_id = Uuid::new_v4().to_string();
        let store = StateStore::new(&self.support);
        let mut state = store.load()?;
        if let Some(remote) = remote {
            let remote_bytes = cloud.download(session, remote).await?;
            self.backup_both(
                &local,
                remote,
                &remote_bytes,
                &operation_id,
                (
                    BackupSource::LocalBeforeForce,
                    BackupSource::SteamBeforeForce,
                ),
            )?;
            if use_local {
                self.push_one(
                    &cloud,
                    session,
                    &local,
                    remote,
                    &remote_bytes,
                    &operation_id,
                    &mut state,
                )
                .await?;
            } else {
                state.pending_remote_pulls.insert(
                    slot,
                    PendingRemotePull {
                        slot,
                        remote_name: remote.request_name.clone(),
                        expected_steam_sha1: remote.save.identity.steam_sha1.clone(),
                        requested_unix_ms: unix_ms(),
                    },
                );
            }
        } else if use_local {
            let remote_name = self
                .snapshot()
                .pending_choices
                .iter()
                .find(|choice| choice.slot == slot)
                .map(|choice| choice.remote_name.clone())
                .or_else(|| {
                    state
                        .files
                        .values()
                        .find(|entry| entry.slot == slot)
                        .map(|entry| entry.remote_name.clone())
                })
                .unwrap_or_else(|| {
                    local
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
            self.push_new(
                &cloud,
                session,
                &local,
                &remote_name,
                &operation_id,
                &mut state,
            )
            .await?;
        } else {
            bail!("Steam save slot not found; there is no remote version to pull");
        }
        store.save(&state)?;
        self.update_status(|status| {
            status.pending_choices.retain(|choice| choice.slot != slot);
            if use_local {
                status.detail = format!("Slot {slot} resolved and verified");
            } else {
                status.phase = "restart_required".to_owned();
                status.detail = format!(
                    "Slot {slot} Steam save is verified and queued; relaunch Isaac to apply it"
                );
            }
        });
        Ok(())
    }

    async fn push_new(
        &self,
        cloud: &SteamCloud,
        session: &SteamSession,
        local: &LocalSave,
        remote_name: &str,
        operation_id: &str,
        state: &mut SyncState,
    ) -> Result<()> {
        let (stable_raw, bytes, canonical_identity) = self.prepare_upload(local).await?;
        let (_, latest) = cloud.list_isaac_saves(session).await?;
        if latest
            .iter()
            .any(|file| file.save.slot == local.slot || file.request_name == remote_name)
        {
            bail!("Steam created this save while the upload was being prepared; synchronize again");
        }
        BackupManager::new(&self.support).backup_file(
            &local.path,
            local.slot,
            BackupSource::LocalFirstSync,
            operation_id,
        )?;
        let verified = cloud.upload_verified(session, remote_name, &bytes).await?;
        let observed_raw = identity_for_path(&local.path)?;
        if observed_raw.sha256 != stable_raw.sha256 {
            bail!("local save changed during Steam upload");
        }
        let observed_local = canonical_identity_for_path(&local.path)?;
        if observed_local.sha256 != canonical_identity.sha256 {
            bail!("local save changed semantically during Steam upload");
        }
        let mut remote_identity = verified.save.identity;
        remote_identity.sha256 = canonical_identity.sha256.clone();
        remote_identity.size = canonical_identity.size;
        upsert_state(
            state,
            FileSyncState {
                slot: local.slot,
                local_relative_path: local.relative_path.clone(),
                remote_name: verified.request_name,
                base_sha256: canonical_identity.sha256,
                base_steam_sha1: remote_identity.steam_sha1.clone(),
                last_local: observed_local,
                last_remote: remote_identity,
                last_successful_sync_unix_ms: unix_ms(),
            },
        );
        self.logger
            .log("info", "upload", "new Steam file committed and verified");
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn push_one(
        &self,
        cloud: &SteamCloud,
        session: &SteamSession,
        local: &LocalSave,
        remote: &CloudFile,
        remote_bytes: &[u8],
        operation_id: &str,
        state: &mut SyncState,
    ) -> Result<()> {
        let (stable_raw, bytes, canonical_identity) = self.prepare_upload(local).await?;
        self.assert_remote_unchanged(cloud, session, remote, remote_bytes)
            .await?;
        let manager = BackupManager::new(&self.support);
        manager.backup_bytes(
            remote_bytes,
            &remote.save.filename,
            local.slot,
            BackupSource::SteamBeforePush,
            operation_id,
            remote.save.identity.steam_sha1.clone(),
        )?;
        let verified = cloud
            .upload_verified(session, &remote.request_name, &bytes)
            .await?;
        let mut remote_identity = verified.save.identity;
        remote_identity.sha256 = canonical_identity.sha256.clone();
        remote_identity.size = canonical_identity.size;
        let observed_raw = identity_for_path(&local.path)?;
        if observed_raw.sha256 != stable_raw.sha256 {
            bail!("local save changed during Steam upload");
        }
        let observed_local = canonical_identity_for_path(&local.path)?;
        if observed_local.sha256 != canonical_identity.sha256 {
            bail!("local save changed semantically during Steam upload");
        }
        upsert_state(
            state,
            FileSyncState {
                slot: local.slot,
                local_relative_path: local.relative_path.clone(),
                remote_name: remote.request_name.clone(),
                base_sha256: observed_local.sha256.clone(),
                base_steam_sha1: remote_identity.steam_sha1.clone(),
                last_local: observed_local,
                last_remote: remote_identity,
                last_successful_sync_unix_ms: unix_ms(),
            },
        );
        self.logger
            .log("info", "upload", "Steam upload committed and verified");
        Ok(())
    }

    async fn prepare_upload(
        &self,
        local: &LocalSave,
    ) -> Result<(FileIdentity, Vec<u8>, FileIdentity)> {
        let staging = unique_temp_path(&self.support.join("tmp"), "upload");
        let stable_raw = wait_for_stable_snapshot(&local.path, &staging).await?;
        let staged_bytes = fs::read(&staging);
        let _ = fs::remove_file(&staging);
        let canonical = canonicalize_save(&staged_bytes?)?;
        if canonical.encoding == SaveEncoding::IosRawLz4 {
            self.logger.log(
                "info",
                "compatibility",
                &format!(
                    "slot={} decoded native iOS LZ4 save for Windows-compatible upload",
                    local.slot
                ),
            );
        }
        let mut canonical_identity = identity_for_bytes(&canonical.bytes);
        canonical_identity.modified_unix_ms = stable_raw.modified_unix_ms;
        Ok((stable_raw, canonical.bytes, canonical_identity))
    }

    async fn assert_remote_unchanged(
        &self,
        cloud: &SteamCloud,
        session: &SteamSession,
        expected: &CloudFile,
        expected_bytes: &[u8],
    ) -> Result<()> {
        let (_, latest_files) = cloud.list_isaac_saves(session).await?;
        let latest = latest_files
            .iter()
            .find(|file| file.request_name == expected.request_name)
            .context("Steam save disappeared while the upload was being prepared")?;
        let unchanged = match (
            expected.save.identity.steam_sha1.as_deref(),
            latest.save.identity.steam_sha1.as_deref(),
        ) {
            (Some(old), Some(new)) => old == new,
            _ => {
                let latest_bytes = cloud.download(session, latest).await?;
                identity_for_bytes(&latest_bytes).sha256
                    == identity_for_bytes(expected_bytes).sha256
            }
        };
        if !unchanged {
            bail!("Steam save changed while the upload was being prepared; synchronize again");
        }
        Ok(())
    }

    fn pull_one(
        &self,
        local: &LocalSave,
        remote: &CloudFile,
        remote_bytes: &[u8],
        operation_id: &str,
        state: &mut SyncState,
    ) -> Result<()> {
        let _replacement_guard = self
            .replacement_guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.replacement_allowed.load(Ordering::Acquire) {
            bail!(
                "local replacement is only allowed at the prelaunch barrier or after an explicit warning"
            );
        }
        let manager = BackupManager::new(&self.support);
        let backup = manager.backup_file(
            &local.path,
            local.slot,
            BackupSource::LocalBeforePull,
            operation_id,
        )?;
        let remote_identity = identity_for_bytes(remote_bytes);
        if let Err(write_error) = atomic::write_bytes(&local.path, remote_bytes) {
            manager
                .restore_local(&backup.backup_id, &local.path)
                .context("rollback local save after failed atomic replacement")?;
            return Err(write_error.context("atomic save replacement failed; old save restored"));
        }
        let verified = match identity_for_path(&local.path) {
            Ok(identity) => identity,
            Err(verification_error) => {
                manager
                    .restore_local(&backup.backup_id, &local.path)
                    .context("rollback local save after replacement verification error")?;
                return Err(
                    verification_error.context("could not verify replaced save; old save restored")
                );
            }
        };
        if verified.sha256 != remote_identity.sha256 {
            manager
                .restore_local(&backup.backup_id, &local.path)
                .context("rollback local save after replacement verification failure")?;
            bail!("atomic save replacement verification failed; old save restored");
        }
        let mut remote_observed = remote.save.identity.clone();
        remote_observed.sha256 = remote_identity.sha256.clone();
        remote_observed.size = remote_identity.size;
        upsert_state(
            state,
            FileSyncState {
                slot: local.slot,
                local_relative_path: local.relative_path.clone(),
                remote_name: remote.request_name.clone(),
                base_sha256: verified.sha256.clone(),
                base_steam_sha1: remote_observed.steam_sha1.clone(),
                last_local: verified,
                last_remote: remote_observed,
                last_successful_sync_unix_ms: unix_ms(),
            },
        );
        self.logger.log(
            "info",
            "download",
            "atomic local replacement succeeded and verified",
        );
        Ok(())
    }

    fn backup_both(
        &self,
        local: &LocalSave,
        remote: &CloudFile,
        remote_bytes: &[u8],
        operation_id: &str,
        sources: (BackupSource, BackupSource),
    ) -> Result<()> {
        let manager = BackupManager::new(&self.support);
        manager.backup_file(&local.path, local.slot, sources.0, operation_id)?;
        manager.backup_bytes(
            remote_bytes,
            &remote.save.filename,
            local.slot,
            sources.1,
            operation_id,
            remote.save.identity.steam_sha1.clone(),
        )?;
        Ok(())
    }

    fn update_status(&self, update: impl FnOnce(&mut StatusSnapshot)) {
        if let Ok(mut status) = self.status.lock() {
            update(&mut status);
        }
    }
}

fn local_summary(save: &LocalSave) -> SaveSummary {
    SaveSummary {
        slot: save.slot,
        filename: save.relative_path.clone(),
        size: save.identity.size,
        modified_unix_ms: save.identity.modified_unix_ms,
        sha256: Some(save.identity.sha256.clone()),
        steam_sha1: save.identity.steam_sha1.clone(),
    }
}

fn remote_summary(file: &CloudFile) -> SaveSummary {
    SaveSummary {
        slot: file.save.slot,
        filename: file.request_name.clone(),
        size: file.save.identity.size,
        modified_unix_ms: file.save.identity.modified_unix_ms,
        sha256: (!file.save.identity.sha256.is_empty()).then(|| file.save.identity.sha256.clone()),
        steam_sha1: file.save.identity.steam_sha1.clone(),
    }
}

fn choice_summary(
    local: &LocalSave,
    remote: &CloudFile,
    remote_identity: &FileIdentity,
    kind: &str,
) -> PendingChoice {
    let mut remote_summary = remote_summary(remote);
    remote_summary.sha256 = Some(remote_identity.sha256.clone());
    PendingChoice {
        slot: local.slot,
        kind: kind.to_owned(),
        local: local_summary(local),
        remote: remote_summary,
        remote_name: remote.request_name.clone(),
    }
}

fn missing_remote_choice(local: &LocalSave) -> PendingChoice {
    PendingChoice {
        slot: local.slot,
        kind: "first_sync_remote_missing".to_owned(),
        local: local_summary(local),
        remote: SaveSummary {
            slot: local.slot,
            filename: "Not present on Steam".to_owned(),
            size: 0,
            modified_unix_ms: None,
            sha256: None,
            steam_sha1: None,
        },
        remote_name: local
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    }
}

fn make_file_state(
    local: &LocalSave,
    remote: &CloudFile,
    remote_identity: &FileIdentity,
) -> FileSyncState {
    FileSyncState {
        slot: local.slot,
        local_relative_path: local.relative_path.clone(),
        remote_name: remote.request_name.clone(),
        base_sha256: local.identity.sha256.clone(),
        base_steam_sha1: remote_identity.steam_sha1.clone(),
        last_local: local.identity.clone(),
        last_remote: remote_identity.clone(),
        last_successful_sync_unix_ms: unix_ms(),
    }
}

fn upsert_state(state: &mut SyncState, entry: FileSyncState) {
    let stale: Vec<String> = state
        .files
        .iter()
        .filter(|(_, value)| value.slot == entry.slot)
        .map(|(key, _)| key.clone())
        .collect();
    for key in stale {
        state.files.remove(&key);
    }
    state.files.insert(entry.remote_name.clone(), entry);
}

fn decision_label(decision: SyncDecision) -> &'static str {
    match decision {
        SyncDecision::Unchanged => "unchanged",
        SyncDecision::UploadLocal => "local-only change",
        SyncDecision::DownloadRemote => "remote-only change",
        SyncDecision::Converged => "already converged",
        SyncDecision::Conflict => "true divergence",
        SyncDecision::FirstSyncChoiceRequired => "first sync choice required",
    }
}

fn short_hash(value: &str) -> &str {
    value.get(..16).unwrap_or(value)
}

fn safe_error(error: &anyhow::Error) -> String {
    let message = format!("{error:#}");
    if message.contains("http://")
        || message.contains("https://")
        || message.matches('.').count() >= 2
    {
        "Steam network operation failed; local play is still available".to_owned()
    } else {
        message.chars().take(400).collect()
    }
}
