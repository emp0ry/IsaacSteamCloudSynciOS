use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use num_bigint::BigUint;
use rand::{RngCore, rngs::OsRng};

use crate::{
    connection::Connection,
    error::{Error, Result},
    protobuf::{
        CAuthenticationBeginAuthSessionViaCredentialsRequest,
        CAuthenticationBeginAuthSessionViaCredentialsResponse, CAuthenticationDeviceDetails,
        CAuthenticationGetPasswordRsaPublicKeyRequest,
        CAuthenticationGetPasswordRsaPublicKeyResponse,
        CAuthenticationPollAuthSessionStatusRequest, CAuthenticationPollAuthSessionStatusResponse,
        CAuthenticationUpdateAuthSessionWithSteamGuardCodeRequest, EAuthSessionGuardType,
        EAuthTokenPlatformType, ESessionPersistence,
    },
    service_method::{ServiceMethod, call},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardKind {
    EmailCode,
    DeviceCode,
    DeviceConfirmation,
}

const GET_PASSWORD_RSA_KEY_METHOD: &str = "Authentication.GetPasswordRSAPublicKey#1";
const BEGIN_CREDENTIALS_METHOD: &str = "Authentication.BeginAuthSessionViaCredentials#1";
const POLL_METHOD: &str = "Authentication.PollAuthSessionStatus#1";
const UPDATE_GUARD_CODE_METHOD: &str = "Authentication.UpdateAuthSessionWithSteamGuardCode#1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSession {
    pub client_id: u64,
    pub request_id: Vec<u8>,
    pub steamid: u64,
    pub interval: Duration,
    pub allowed_confirmations: Vec<GuardKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedAuth {
    pub refresh_token: String,
    pub account_name: String,
}

impl CredentialSession {
    pub fn preferred_guard_kind(&self) -> Option<GuardKind> {
        self.allowed_confirmations
            .iter()
            .find(|kind| **kind == GuardKind::DeviceConfirmation)
            .cloned()
            .or_else(|| {
                self.allowed_confirmations
                    .iter()
                    .find_map(|kind| match kind {
                        GuardKind::EmailCode => Some(GuardKind::EmailCode),
                        GuardKind::DeviceCode => Some(GuardKind::DeviceCode),
                        GuardKind::DeviceConfirmation => None,
                    })
            })
    }
}

pub async fn begin(
    connection: &Connection,
    account_name: &str,
    password: &str,
    device_friendly_name: &str,
    device_details: CAuthenticationDeviceDetails,
    website_id: &str,
) -> Result<CredentialSession> {
    let rsa_key = get_password_rsa_key(connection, account_name).await?;
    let encrypted_password = encrypt_password(password, &rsa_key)?;

    let response: CAuthenticationBeginAuthSessionViaCredentialsResponse = call(
        connection,
        &ServiceMethod::new(BEGIN_CREDENTIALS_METHOD),
        &CAuthenticationBeginAuthSessionViaCredentialsRequest {
            device_friendly_name: Some(device_friendly_name.to_owned()),
            account_name: Some(account_name.to_owned()),
            encrypted_password: Some(encrypted_password),
            encryption_timestamp: rsa_key.timestamp,
            remember_login: Some(true),
            platform_type: Some(EAuthTokenPlatformType::KEAuthTokenPlatformTypeSteamClient as i32),
            persistence: Some(ESessionPersistence::KESessionPersistencePersistent as i32),
            website_id: Some(website_id.to_owned()),
            device_details: Some(device_details),
            guard_data: None,
            language: None,
            qos_level: Some(2),
        },
    )
    .await?;

    let auth_error = response
        .extended_error_message
        .as_deref()
        .filter(|message| !message.is_empty())
        .unwrap_or("Steam rejected the account name or password")
        .to_owned();
    Ok(CredentialSession {
        client_id: response
            .client_id
            .ok_or_else(|| Error::Authentication(auth_error.clone()))?,
        request_id: response.request_id.ok_or(Error::MissingField(
            "CAuthenticationBeginAuthSessionViaCredentialsResponse.request_id",
        ))?,
        steamid: response.steamid.ok_or(Error::MissingField(
            "CAuthenticationBeginAuthSessionViaCredentialsResponse.steamid",
        ))?,
        interval: Duration::from_secs_f32(response.interval.unwrap_or(5.0).max(1.0)),
        allowed_confirmations: response
            .allowed_confirmations
            .into_iter()
            .filter_map(|confirmation| map_guard_kind(confirmation.confirmation_type))
            .collect(),
    })
}

pub async fn submit_guard_code(
    connection: &Connection,
    session: &CredentialSession,
    code: &str,
    kind: GuardKind,
) -> Result<()> {
    let _: crate::protobuf::CAuthenticationUpdateAuthSessionWithSteamGuardCodeResponse = call(
        connection,
        &ServiceMethod::new(UPDATE_GUARD_CODE_METHOD),
        &CAuthenticationUpdateAuthSessionWithSteamGuardCodeRequest {
            client_id: Some(session.client_id),
            steamid: Some(session.steamid),
            code: Some(code.to_owned()),
            code_type: Some(guard_kind_to_proto(kind) as i32),
        },
    )
    .await?;

    Ok(())
}

pub async fn poll(
    connection: &Connection,
    session: &CredentialSession,
) -> Result<Option<CompletedAuth>> {
    let response: CAuthenticationPollAuthSessionStatusResponse = call(
        connection,
        &ServiceMethod::new(POLL_METHOD),
        &CAuthenticationPollAuthSessionStatusRequest {
            client_id: Some(session.client_id),
            request_id: Some(session.request_id.clone()),
            token_to_revoke: None,
        },
    )
    .await?;

    match response.refresh_token {
        Some(refresh_token) => Ok(Some(CompletedAuth {
            refresh_token,
            account_name: response.account_name.ok_or(Error::MissingField(
                "CAuthenticationPollAuthSessionStatusResponse.account_name",
            ))?,
        })),
        None => Ok(None),
    }
}

struct PasswordRsaKey {
    modulus: BigUint,
    exponent: BigUint,
    timestamp: Option<u64>,
}

async fn get_password_rsa_key(
    connection: &Connection,
    account_name: &str,
) -> Result<PasswordRsaKey> {
    let response: CAuthenticationGetPasswordRsaPublicKeyResponse = call(
        connection,
        &ServiceMethod::new(GET_PASSWORD_RSA_KEY_METHOD),
        &CAuthenticationGetPasswordRsaPublicKeyRequest {
            account_name: Some(account_name.to_owned()),
        },
    )
    .await?;

    let modulus = parse_hex_biguint(&response.publickey_mod.ok_or(Error::MissingField(
        "CAuthenticationGetPasswordRsaPublicKeyResponse.publickey_mod",
    ))?)?;
    let exponent = parse_hex_biguint(&response.publickey_exp.ok_or(Error::MissingField(
        "CAuthenticationGetPasswordRsaPublicKeyResponse.publickey_exp",
    ))?)?;

    Ok(PasswordRsaKey {
        modulus,
        exponent,
        timestamp: response.timestamp,
    })
}

fn encrypt_password(password: &str, key: &PasswordRsaKey) -> Result<String> {
    // Valve requires RSAES-PKCS1-v1_5. This path performs only a public-key
    // operation; no private RSA material exists on the device. Avoid the
    // vulnerable/unmaintained `rsa` crate that the upstream facade used.
    let modulus_len = key.modulus.bits().div_ceil(8) as usize;
    let message = password.as_bytes();
    if modulus_len < message.len() + 11 {
        return Err(Error::Authentication("Steam password is too long".to_owned()));
    }
    let padding_len = modulus_len - message.len() - 3;
    let mut encoded = vec![0_u8; modulus_len];
    encoded[1] = 2;
    let mut rng = OsRng;
    for byte in &mut encoded[2..2 + padding_len] {
        while *byte == 0 {
            *byte = rng.next_u32() as u8;
        }
    }
    encoded[2 + padding_len] = 0;
    encoded[3 + padding_len..].copy_from_slice(message);
    let value = BigUint::from_bytes_be(&encoded).modpow(&key.exponent, &key.modulus);
    let encrypted = value.to_bytes_be();
    let mut padded = vec![0_u8; modulus_len.saturating_sub(encrypted.len())];
    padded.extend_from_slice(&encrypted);
    Ok(STANDARD.encode(padded))
}

fn parse_hex_biguint(value: &str) -> Result<BigUint> {
    BigUint::parse_bytes(value.as_bytes(), 16)
        .ok_or(Error::Authentication("invalid RSA key response".to_owned()))
}

fn map_guard_kind(code: Option<i32>) -> Option<GuardKind> {
    match code.and_then(|value| EAuthSessionGuardType::try_from(value).ok()) {
        Some(EAuthSessionGuardType::KEAuthSessionGuardTypeEmailCode) => Some(GuardKind::EmailCode),
        Some(EAuthSessionGuardType::KEAuthSessionGuardTypeDeviceCode) => {
            Some(GuardKind::DeviceCode)
        }
        Some(EAuthSessionGuardType::KEAuthSessionGuardTypeDeviceConfirmation) => {
            Some(GuardKind::DeviceConfirmation)
        }
        _ => None,
    }
}

fn guard_kind_to_proto(kind: GuardKind) -> EAuthSessionGuardType {
    match kind {
        GuardKind::EmailCode => EAuthSessionGuardType::KEAuthSessionGuardTypeEmailCode,
        GuardKind::DeviceCode => EAuthSessionGuardType::KEAuthSessionGuardTypeDeviceCode,
        GuardKind::DeviceConfirmation => {
            EAuthSessionGuardType::KEAuthSessionGuardTypeDeviceConfirmation
        }
    }
}
