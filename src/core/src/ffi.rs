use crate::{achievements, engine::Engine};
use std::{
    ffi::{CStr, CString, c_char},
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();
static HOME: OnceLock<PathBuf> = OnceLock::new();
static ACHIEVEMENT_SYNC_ACTIVE: AtomicBool = AtomicBool::new(false);

fn engine() -> Option<&'static Arc<Engine>> {
    ENGINE.get()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ICSCoreStart(home: *const c_char) -> i32 {
    if home.is_null() { return -1; }
    let value = match unsafe { CStr::from_ptr(home) }.to_str() { Ok(value) => value, Err(_) => return -2 };
    let home_path = PathBuf::from(value);
    match Engine::new(home_path.clone()) {
        Ok(created) => { let _ = HOME.set(home_path); let _ = ENGINE.set(created); 0 }
        Err(_) => -3,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ICSCoreConnectSteam() -> bool { engine().is_some_and(|engine| engine.connect_qr()) }

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ICSCoreConnectSteamWithPassword(account: *const c_char, password: *const c_char) -> bool {
    if account.is_null() || password.is_null() { return false; }
    let Ok(account) = unsafe { CStr::from_ptr(account) }.to_str() else { return false; };
    let Ok(password) = unsafe { CStr::from_ptr(password) }.to_str() else { return false; };
    if account.trim().is_empty() || password.is_empty() { return false; }
    engine().is_some_and(|engine| engine.connect_credentials(account.trim().to_owned(), password.to_owned()))
}

#[unsafe(no_mangle)]
pub extern "C" fn ICSCoreCancelSteamLogin() -> bool { engine().is_some_and(|engine| engine.cancel_login()) }

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ICSCoreSubmitSteamGuardCode(code: *const c_char) -> bool {
    if code.is_null() { return false; }
    let Ok(code) = unsafe { CStr::from_ptr(code) }.to_str() else { return false; };
    let code = code.trim();
    !code.is_empty() && engine().is_some_and(|engine| engine.submit_guard_code(code.to_owned()))
}

#[unsafe(no_mangle)]
pub extern "C" fn ICSCoreDisconnectSteam() -> bool { engine().is_some_and(|engine| engine.disconnect()) }

#[unsafe(no_mangle)]
pub extern "C" fn ICSCoreSetForeground(foreground: bool) -> bool { engine().is_some_and(|engine| engine.set_foreground(foreground)) }

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ICSCoreSyncNow(trigger: *const c_char) -> bool {
    let trigger = if trigger.is_null() { "manual" } else { unsafe { CStr::from_ptr(trigger) }.to_str().unwrap_or("manual") };
    let started = engine().is_some_and(|engine| engine.automatic_sync(trigger));
    if started {
        // Achievement synchronization is independent of cloud-save conflict resolution.
        // It reads only the real local persistentgamedata save and only ADDS Steam
        // unlocks that are missing; it never clears or overwrites Steam achievements.
        let _ = ICSCoreSyncAchievements();
    }
    started
}

#[unsafe(no_mangle)]
pub extern "C" fn ICSCoreSyncAchievements() -> bool {
    let Some(home) = HOME.get().cloned() else { return false; };
    if ACHIEVEMENT_SYNC_ACTIVE.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() { return false; }
    std::thread::Builder::new()
        .name("IsaacSaveAchievementSync".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
            if let Ok(runtime) = runtime { let _ = runtime.block_on(achievements::sync_from_local_saves(&home)); }
            ACHIEVEMENT_SYNC_ACTIVE.store(false, Ordering::Release);
        })
        .map(|_| true)
        .unwrap_or_else(|_| { ACHIEVEMENT_SYNC_ACTIVE.store(false, Ordering::Release); false })
}

#[unsafe(no_mangle)]
pub extern "C" fn ICSCoreCopyAchievementsJSON() -> *mut c_char {
    CString::new(achievements::achievements_json()).map(CString::into_raw).unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ICSCoreLog(category: *const c_char, message: *const c_char) {
    if category.is_null() || message.is_null() { return; }
    let Ok(category) = unsafe { CStr::from_ptr(category) }.to_str() else { return; };
    let Ok(message) = unsafe { CStr::from_ptr(message) }.to_str() else { return; };
    if let Some(engine) = engine() { engine.record_host_event(category, message); }
}

#[unsafe(no_mangle)]
pub extern "C" fn ICSCoreResolve(slot: u8, use_local: bool) -> bool { engine().is_some_and(|engine| engine.resolve_choice(slot, use_local)) }
#[unsafe(no_mangle)]
pub extern "C" fn ICSCoreForce(slot: u8, use_local: bool) -> bool { engine().is_some_and(|engine| engine.force(slot, use_local)) }

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ICSCoreRestoreBackup(backup_id: *const c_char) -> bool {
    if backup_id.is_null() { return false; }
    let Ok(value) = unsafe { CStr::from_ptr(backup_id) }.to_str() else { return false; };
    engine().is_some_and(|engine| engine.restore_backup(value.to_owned()))
}

#[unsafe(no_mangle)]
pub extern "C" fn ICSCorePreflight(timeout_ms: u64) -> bool { engine().is_some_and(|engine| engine.preflight(timeout_ms)) }

#[unsafe(no_mangle)]
pub extern "C" fn ICSCoreCopyStatusJSON() -> *mut c_char {
    let value = engine().and_then(|engine| serde_json::to_string(&engine.snapshot()).ok()).unwrap_or_else(|| "{\"phase\":\"not_started\"}".to_owned());
    CString::new(value).map(CString::into_raw).unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub extern "C" fn ICSCoreCopyBackupsJSON() -> *mut c_char {
    let value = engine().and_then(|engine| engine.backups_json().ok()).unwrap_or_else(|| "[]".to_owned());
    CString::new(value).map(CString::into_raw).unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ICSCoreFreeString(value: *mut c_char) {
    if !value.is_null() { drop(unsafe { CString::from_raw(value) }); }
}
