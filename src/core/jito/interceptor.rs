use std::{
    sync::{Arc, RwLock},
    time::{Duration, SystemTime},
};

use crate::core::jito::protos::auth::{
    GenerateAuthChallengeRequest, GenerateAuthTokensRequest, RefreshAccessTokenRequest, Role,
    Token, auth_service_client::AuthServiceClient,
};
use anyhow::Context;
use prost_types::Timestamp;
use solana_sdk::{signature::Keypair, signer::Signer};
use tokio::time::sleep;
use tonic::{service::Interceptor, transport::Channel};
use tracing::error;

/// Interceptor that adds Jito auth tokens to gRPC requests.
#[derive(Clone)]
pub struct ClientInterceptor {
    token: Arc<RwLock<String>>,
}

impl ClientInterceptor {
    /// Create a new authenticated interceptor using the provided keypair.
    pub async fn new(
        auth_keypair: Arc<Keypair>,
        mut auth_service_client: AuthServiceClient<Channel>,
    ) -> anyhow::Result<Self> {
        let role = Role::Searcher;
        let (access_token, refresh_token) =
            Self::auth(&mut auth_service_client, &auth_keypair, role).await?;
        let bearer_token = Arc::new(RwLock::new(access_token.value.clone()));

        std::thread::spawn({
            let bearer_token = bearer_token.clone();
            let Some(access_token_expiration) = access_token.expires_at_utc.clone() else {
                error!("Jito access token did not include expiration; token refresh loop disabled");
                return Ok(Self {
                    token: bearer_token,
                });
            };
            move || {
                Self::token_refresh_loop(
                    auth_service_client,
                    bearer_token,
                    refresh_token,
                    access_token_expiration,
                    auth_keypair,
                    role,
                )
            }
        });

        Ok(Self {
            token: bearer_token,
        })
    }

    async fn auth(
        auth_service_client: &mut AuthServiceClient<Channel>,
        keypair: &Keypair,
        role: Role,
    ) -> anyhow::Result<(Token, Token)> {
        let challenge_resp = auth_service_client
            .generate_auth_challenge(GenerateAuthChallengeRequest {
                role: role as i32,
                pubkey: keypair.pubkey().as_ref().to_vec(),
            })
            .await?
            .into_inner();

        let challenge = format!("{}-{}", keypair.pubkey(), challenge_resp.challenge);
        let signed_challenge = keypair.sign_message(challenge.as_bytes()).as_ref().to_vec();

        let tokens = auth_service_client
            .generate_auth_tokens(GenerateAuthTokensRequest {
                challenge,
                client_pubkey: keypair.pubkey().as_ref().to_vec(),
                signed_challenge,
            })
            .await?
            .into_inner();

        Ok((
            tokens
                .access_token
                .context("Jito auth response missing access token")?,
            tokens
                .refresh_token
                .context("Jito auth response missing refresh token")?,
        ))
    }

    async fn token_refresh_loop(
        mut auth_service_client: AuthServiceClient<Channel>,
        bearer_token: Arc<RwLock<String>>,
        mut refresh_token: Token,
        mut access_token_expiration: Timestamp,
        keypair: Arc<Keypair>,
        role: Role,
    ) {
        loop {
            let access_token_ttl = SystemTime::try_from(access_token_expiration.clone())
                .unwrap_or(SystemTime::UNIX_EPOCH)
                .duration_since(SystemTime::now())
                .unwrap_or_else(|_| Duration::from_secs(0));
            let refresh_token_ttl = refresh_token
                .expires_at_utc
                .as_ref()
                .and_then(|expires| SystemTime::try_from(expires.clone()).ok())
                .unwrap_or(SystemTime::UNIX_EPOCH)
                .duration_since(SystemTime::now())
                .unwrap_or_else(|_| Duration::from_secs(0));

            let access_soon = access_token_ttl < Duration::from_secs(5 * 60);
            let refresh_soon = refresh_token_ttl < Duration::from_secs(5 * 60);

            match (refresh_soon, access_soon) {
                (true, _) => match Self::auth(&mut auth_service_client, &keypair, role).await {
                    Ok((new_access, new_refresh)) => {
                        if let Ok(mut token) = bearer_token.write() {
                            *token = new_access.value.clone();
                        }
                        if let Some(expires_at) = new_access.expires_at_utc {
                            access_token_expiration = expires_at;
                        }
                        refresh_token = new_refresh;
                    }
                    Err(e) => error!("Failed re-auth Jito: {e:?}"),
                },
                (_, true) => match auth_service_client
                    .refresh_access_token(RefreshAccessTokenRequest {
                        refresh_token: refresh_token.value.clone(),
                    })
                    .await
                {
                    Ok(resp) => {
                        if let Some(access_token) = resp.into_inner().access_token {
                            if let Ok(mut token) = bearer_token.write() {
                                *token = access_token.value.clone();
                            }
                            if let Some(expires_at) = access_token.expires_at_utc {
                                access_token_expiration = expires_at;
                            }
                        }
                    }
                    Err(e) => error!("Refresh access token failed: {e:?}"),
                },
                _ => {
                    sleep(Duration::from_secs(60)).await;
                }
            }
        }
    }
}

const AUTHORIZATION_HEADER: &str = "authorization";

impl Interceptor for ClientInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        if let Ok(token) = self.token.read() {
            if !token.is_empty() {
                let header = format!("Bearer {token}")
                    .parse()
                    .map_err(|_| tonic::Status::internal("invalid Jito auth token"))?;
                request.metadata_mut().insert(AUTHORIZATION_HEADER, header);
            }
        }
        Ok(request)
    }
}
