use anyhow::Result;
#[cfg(target_os = "ios")]
use anyhow::bail;

#[cfg(target_os = "ios")]
const SERVICE: &[u8] = b"com.isaaccloudsync.steam\0";
#[cfg(target_os = "ios")]
const BRIDGE_SERVICE: &[u8] = b"com.isaacsteambridge.steam\0";
#[cfg(target_os = "ios")]
const ACCOUNT: &[u8] = b"refresh-token\0";

#[cfg(target_os = "ios")]
unsafe extern "C" {
    fn ICSKeychainStore(
        service: *const libc::c_char,
        account: *const libc::c_char,
        bytes: *const u8,
        length: usize,
    ) -> i32;
    fn ICSKeychainCopy(
        service: *const libc::c_char,
        account: *const libc::c_char,
        length: *mut usize,
    ) -> *mut u8;
    fn ICSKeychainDelete(service: *const libc::c_char, account: *const libc::c_char) -> i32;
    fn ICSFree(pointer: *mut libc::c_void);
}

#[cfg(target_os = "ios")]
pub fn store_refresh_token(token: &str) -> Result<()> {
    let status = unsafe {
        ICSKeychainStore(
            SERVICE.as_ptr().cast(),
            ACCOUNT.as_ptr().cast(),
            token.as_ptr(),
            token.len(),
        )
    };
    if status != 0 {
        bail!("Keychain store failed with OSStatus {status}");
    }
    Ok(())
}

#[cfg(target_os = "ios")]
pub fn load_refresh_token() -> Result<Option<String>> {
    if let Some(token) = load_for_service(SERVICE)? {
        return Ok(Some(token));
    }
    if let Some(token) = load_for_service(BRIDGE_SERVICE)? {
        store_refresh_token(&token)?;
        return Ok(Some(token));
    }
    Ok(None)
}

#[cfg(target_os = "ios")]
fn load_for_service(service: &[u8]) -> Result<Option<String>> {
    let mut length = 0_usize;
    let pointer = unsafe {
        ICSKeychainCopy(
            service.as_ptr().cast(),
            ACCOUNT.as_ptr().cast(),
            &mut length,
        )
    };
    if pointer.is_null() {
        return Ok(None);
    }
    let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
    let result = String::from_utf8(bytes.to_vec());
    unsafe {
        std::ptr::write_bytes(pointer, 0, length);
        ICSFree(pointer.cast());
    }
    Ok(Some(result?))
}

#[cfg(target_os = "ios")]
pub fn delete_refresh_token() -> Result<()> {
    let status = unsafe { ICSKeychainDelete(SERVICE.as_ptr().cast(), ACCOUNT.as_ptr().cast()) };
    if status != 0 && status != -25300 {
        bail!("Keychain delete failed with OSStatus {status}");
    }
    let bridge_status =
        unsafe { ICSKeychainDelete(BRIDGE_SERVICE.as_ptr().cast(), ACCOUNT.as_ptr().cast()) };
    if bridge_status != 0 && bridge_status != -25300 {
        bail!("legacy Keychain delete failed with OSStatus {bridge_status}");
    }
    Ok(())
}

// Host-side tests never persist authentication material. The iOS build above
// is the only production implementation.
#[cfg(not(target_os = "ios"))]
pub fn store_refresh_token(_token: &str) -> Result<()> {
    Ok(())
}
#[cfg(not(target_os = "ios"))]
pub fn load_refresh_token() -> Result<Option<String>> {
    Ok(None)
}
#[cfg(not(target_os = "ios"))]
pub fn delete_refresh_token() -> Result<()> {
    Ok(())
}
