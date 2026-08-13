use anyhow::{Context, Result, bail};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use steam_cm_protocol::{
    auth::{
        credentials::{self, GuardKind},
        qr,
    },
    connection::Connection,
    emsg::EMsg,
    protobuf::{
        CAuthenticationDeviceDetails, CMsgClientChangeStatus, CMsgClientGamesPlayed,
        CMsgClientHello, CMsgClientLogon, CMsgClientLogonResponse, CMsgProtoBufHeader,
        EAuthTokenPlatformType, c_msg_client_games_played::GamePlayed,
    },
    serverlist::ServerListCache,
    token::steamid_from_refresh_token,
};
use tokio::{
    sync::mpsc,
    task::JoinSet,
    time::{sleep, timeout},
};

const PROTOCOL_VERSION: u32 = 65_580;
const CLIENT_OS_TYPE: u32 = 20;
const DEVICE_NAME: &str = "IsaacCloudSync iPhone";
const WEBSITE_ID: &str = "Unknown";
const GAMING_DEVICE_TYPE: u32 = 1;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const CM_TOTAL_TIMEOUT: Duration = Duration::from_secs(25);
const QR_APPROVAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CREDENTIAL_APPROVAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CM_PARALLEL_ATTEMPTS: usize = 6;
const PERSONA_STATE_ONLINE: u32 = 1;

pub struct SteamSession {
    pub connection: Connection,
    pub steam_id: u64,
    pub account_name: String,
}

#[derive(Debug, Clone, Copy)]
pub enum CredentialPrompt {
    EmailCode,
    DeviceCode,
    DeviceConfirmation,
}

pub async fn set_playing(session: &SteamSession, playing: bool) -> Result<()> {
    let state = session.connection.state_snapshot().await;
    let header = CMsgProtoBufHeader {
        steamid: state.steamid,
        client_sessionid: state.client_session_id,
        ..Default::default()
    };
    if playing {
        session
            .connection
            .send_message(
                EMsg::ClientChangeStatus,
                &header,
                &CMsgClientChangeStatus {
                    persona_state: Some(PERSONA_STATE_ONLINE),
                    high_priority: Some(true),
                    persona_set_by_user: Some(false),
                    ..Default::default()
                },
            )
            .await
            .context("set Steam persona online for game presence")?;
    }
    session
        .connection
        .send_message(
            EMsg::ClientGamesPlayed,
            &header,
            &CMsgClientGamesPlayed {
                games_played: if playing {
                    vec![GamePlayed {
                        game_id: Some(crate::model::STEAM_APP_ID as u64),
                        ..Default::default()
                    }]
                } else {
                    vec![]
                },
                client_os_type: Some(CLIENT_OS_TYPE),
                ..Default::default()
            },
        )
        .await
        .context("update Steam AppID 250900 presence")
}

pub async fn connect_with_refresh_token(token: &str) -> Result<SteamSession> {
    let connection = connect_cm().await?;
    log_on(connection, token, None).await
}

pub async fn connect_with_qr<F>(
    mut challenge_callback: F,
    cancelled: Arc<AtomicBool>,
) -> Result<(SteamSession, String)>
where
    F: FnMut(&str),
{
    let connection = connect_cm().await?;
    let mut challenge = qr::begin(
        &connection,
        DEVICE_NAME,
        EAuthTokenPlatformType::KEAuthTokenPlatformTypeSteamClient as i32,
        device_details(),
        WEBSITE_ID,
    )
    .await
    .context("begin Steam QR authentication")?;
    challenge_callback(&challenge.challenge_url);

    let completed = timeout(QR_APPROVAL_TIMEOUT, async {
        loop {
            sleep(challenge.interval).await;
            if cancelled.load(Ordering::Acquire) {
                bail!("Steam sign-in cancelled");
            }
            match qr::poll(&connection, &mut challenge).await? {
                qr::PollState::Pending { challenge_changed } => {
                    if challenge_changed {
                        challenge_callback(&challenge.challenge_url);
                    }
                }
                qr::PollState::Complete(completed) => break Ok::<_, anyhow::Error>(completed),
            }
        }
    })
    .await
    .context("Steam QR approval timed out")??;

    let session = log_on(
        connection,
        &completed.refresh_token,
        Some(completed.account_name),
    )
    .await?;
    Ok((session, completed.refresh_token))
}

pub async fn connect_with_credentials<F>(
    account: &str,
    password: &str,
    mut prompt_callback: F,
    mut guard_codes: mpsc::UnboundedReceiver<String>,
    cancelled: Arc<AtomicBool>,
) -> Result<(SteamSession, String)>
where
    F: FnMut(CredentialPrompt),
{
    let connection = connect_cm().await?;
    let auth = credentials::begin(
        &connection,
        account,
        password,
        DEVICE_NAME,
        device_details(),
        WEBSITE_ID,
    )
    .await
    .context("begin Steam username/password authentication")?;

    if let Some(kind) = auth.preferred_guard_kind() {
        let prompt = match kind {
            GuardKind::EmailCode => CredentialPrompt::EmailCode,
            GuardKind::DeviceCode => CredentialPrompt::DeviceCode,
            GuardKind::DeviceConfirmation => CredentialPrompt::DeviceConfirmation,
        };
        prompt_callback(prompt);
        if kind != GuardKind::DeviceConfirmation {
            let code = timeout(CREDENTIAL_APPROVAL_TIMEOUT, guard_codes.recv())
                .await
                .context("Steam Guard code timed out")?
                .context("Steam Guard sign-in was cancelled")?;
            credentials::submit_guard_code(&connection, &auth, &code, kind)
                .await
                .context("submit Steam Guard code")?;
        }
    }

    let completed = timeout(CREDENTIAL_APPROVAL_TIMEOUT, async {
        loop {
            sleep(auth.interval).await;
            if cancelled.load(Ordering::Acquire) {
                bail!("Steam sign-in cancelled");
            }
            if let Some(completed) = credentials::poll(&connection, &auth).await? {
                break Ok::<_, anyhow::Error>(completed);
            }
        }
    })
    .await
    .context("Steam authentication approval timed out")??;
    let session = log_on(
        connection,
        &completed.refresh_token,
        Some(completed.account_name),
    )
    .await?;
    Ok((session, completed.refresh_token))
}

async fn connect_cm() -> Result<Connection> {
    timeout(CM_TOTAL_TIMEOUT, connect_cm_inner())
        .await
        .context("Steam CM connection timed out")?
}

async fn connect_cm_inner() -> Result<Connection> {
    let servers = ServerListCache::new().list(false).await?;
    let mut last_error = None;
    let mut attempts = JoinSet::new();
    for server in servers.into_iter().take(CM_PARALLEL_ATTEMPTS) {
        let url = server.websocket_url();
        attempts.spawn(async move {
            match timeout(CONNECT_TIMEOUT, Connection::connect(&url)).await {
                Ok(Ok(connection)) => Ok(connection),
                Ok(Err(error)) => Err(error.to_string()),
                Err(_) => Err(format!("connection to {url} timed out")),
            }
        });
    }
    while let Some(attempt) = attempts.join_next().await {
        match attempt {
            Ok(Ok(connection)) => {
                match connection
                    .send_message(
                        EMsg::ClientHello,
                        &CMsgProtoBufHeader::default(),
                        &CMsgClientHello {
                            protocol_version: Some(PROTOCOL_VERSION),
                        },
                    )
                    .await
                {
                    Ok(()) => {
                        attempts.abort_all();
                        return Ok(connection);
                    }
                    Err(error) => last_error = Some(error.to_string()),
                }
            }
            Ok(Err(error)) => last_error = Some(error),
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    bail!(
        "could not connect to a Steam CM: {}",
        last_error.unwrap_or_else(|| "no endpoints".to_owned())
    )
}

async fn log_on(
    mut connection: Connection,
    refresh_token: &str,
    account_name: Option<String>,
) -> Result<SteamSession> {
    let steam_id = steamid_from_refresh_token(refresh_token)
        .context("refresh token did not contain a SteamID")?;
    let account_name = account_name.unwrap_or_default();
    let header = CMsgProtoBufHeader {
        steamid: Some(steam_id),
        ..Default::default()
    };
    let body = CMsgClientLogon {
        protocol_version: Some(PROTOCOL_VERSION),
        client_language: Some("english".to_owned()),
        client_os_type: Some(CLIENT_OS_TYPE),
        client_supplied_steam_id: Some(steam_id),
        machine_id: Some(b"isaaccloudsync-ios".to_vec()),
        account_name: (!account_name.is_empty()).then_some(account_name.clone()),
        should_remember_password: Some(true),
        supports_rate_limit_response: Some(true),
        access_token: Some(refresh_token.to_owned()),
        gaming_device_type: Some(GAMING_DEVICE_TYPE),
        ..Default::default()
    };
    connection
        .send_message(EMsg::ClientLogon, &header, &body)
        .await?;

    let response_packet = timeout(Duration::from_secs(20), async {
        loop {
            let packet = connection
                .next_event()
                .await
                .context("Steam connection closed")??;
            if packet.emsg == EMsg::ClientLogOnResponse.raw() {
                break Ok::<_, anyhow::Error>(packet);
            }
        }
    })
    .await
    .context("Steam logon timed out")??;

    let response = response_packet.decode_body::<CMsgClientLogonResponse>()?;
    let result = response.eresult.unwrap_or_default();
    if result != 1 {
        bail!("Steam logon failed with EResult {result}");
    }
    let session_id = response_packet
        .header
        .client_sessionid
        .context("Steam session ID missing")?;
    let heartbeat = response
        .heartbeat_seconds
        .or(response.legacy_out_of_game_heartbeat_seconds)
        .context("Steam heartbeat interval missing")?;
    connection
        .set_logged_on(steam_id, session_id, heartbeat)
        .await?;
    Ok(SteamSession {
        connection,
        steam_id,
        account_name,
    })
}

fn device_details() -> CAuthenticationDeviceDetails {
    CAuthenticationDeviceDetails {
        device_friendly_name: Some(DEVICE_NAME.to_owned()),
        platform_type: Some(EAuthTokenPlatformType::KEAuthTokenPlatformTypeSteamClient as i32),
        os_type: Some(CLIENT_OS_TYPE as i32),
        gaming_device_type: Some(GAMING_DEVICE_TYPE),
        client_count: Some(1),
        machine_id: Some(b"isaaccloudsync-ios".to_vec()),
        app_type: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playing_payload_targets_only_isaac() {
        let body = CMsgClientGamesPlayed {
            games_played: vec![GamePlayed {
                game_id: Some(crate::model::STEAM_APP_ID as u64),
                ..Default::default()
            }],
            client_os_type: Some(CLIENT_OS_TYPE),
            ..Default::default()
        };
        assert_eq!(body.games_played.len(), 1);
        assert_eq!(body.games_played[0].game_id, Some(250_900));
    }

    #[tokio::test]
    #[ignore = "requires live Valve network access"]
    async fn live_cm_directory_websocket_and_client_hello() {
        let connection = connect_cm().await.unwrap();
        assert!(!connection.is_closed().await);
    }
}
