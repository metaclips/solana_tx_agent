use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};

use futures::StreamExt;
use solana_sdk::{account::Account, hash::Hash, pubkey::Pubkey, signature::Signature};
use tracing::*;
use yellowstone_grpc_client::ClientTlsConfig;
use yellowstone_grpc_proto::{geyser::subscribe_update::UpdateOneof, prelude::*};

/// Events emitted by the Yellowstone/Geyser subscription client.
#[derive(Debug, Clone)]
pub enum GeyserStream {
    OnAccountUpdate {
        slot: u64,
        pubkey: Pubkey,
        account_info: Account,
    },
    TransactionsOnSlot {
        slot: u64,
        index: u64,
        tx: Box<Option<Transaction>>,
        status_meta: Box<Option<TransactionStatusMeta>>,
        signature: Vec<u8>,
    },
    BlockHash {
        slot: u64,
        hash: Hash,
    },
    TransactionStatus {
        slot: u64,
        signature: Signature,
        error: Option<String>,
    },
    StreamCompleteOnSlot(u64),
}

/// Yellowstone/Geyser client that subscribes to slots, blockhashes, and events.
#[derive(Clone)]
pub struct Geyser {
    pub stream: tokio::sync::mpsc::UnboundedSender<GeyserStream>,
}

impl Geyser {
    /// Subscribe to slot-related events on a Yellowstone/Geyser endpoint.
    pub async fn new_geyser_slot_subscription(
        self: Arc<Self>,
        endpoint: String,
        token: Option<String>,
        connect_timeout: Duration,
    ) {
        let request = SubscribeRequest {
            commitment: Some(CommitmentLevel::Confirmed.into()),
            blocks_meta: HashMap::from_iter(vec![(
                "BlockHash".to_string(),
                SubscribeRequestFilterBlocksMeta {},
            )]),
            slots: HashMap::from_iter(vec![(
                "Slots".to_string(),
                SubscribeRequestFilterSlots {
                    filter_by_commitment: Some(true),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };

        self.new_geyser_stream(endpoint, token, request, connect_timeout)
            .await;
    }

    /// Subscribe to status updates for one exact transaction signature.
    pub async fn new_geyser_signature_status_subscription(
        self: Arc<Self>,
        endpoint: String,
        token: Option<String>,
        signature: Signature,
        connect_timeout: Duration,
    ) {
        let request = SubscribeRequest {
            commitment: Some(CommitmentLevel::Processed.into()),
            transactions_status: HashMap::from_iter(vec![(
                "SignatureStatus".to_string(),
                SubscribeRequestFilterTransactions {
                    vote: Some(false),
                    failed: Some(true),
                    signature: Some(signature.to_string()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };

        self.new_geyser_stream(endpoint, token, request, connect_timeout)
            .await;
    }

    async fn new_geyser_stream(
        self: Arc<Self>,
        endpoint: String,
        token: Option<String>,
        mut request: SubscribeRequest,
        connect_timeout: Duration,
    ) {
        info!(
            yellowstone_endpoint_configured = !endpoint.is_empty(),
            yellowstone_token_present = token.is_some(),
            "starting Geyser stream"
        );
        let mut last_seen_slot = request.from_slot.unwrap_or_default();
        let mut reconnect_counter = 0;

        'restart_geyser_connection: loop {
            if reconnect_counter > 0 {
                if last_seen_slot == 0 {
                    request.from_slot = None;
                } else {
                    request.from_slot = Some(last_seen_slot);
                }

                tokio::time::sleep(Duration::from_secs(reconnect_counter)).await;
                reconnect_counter = (reconnect_counter * 2).min(64);
            }

            let client_builder = match yellowstone_grpc_client::GeyserGrpcClient::build_from_shared(
                endpoint.clone(),
            )
            .and_then(|builder| builder.x_token(token.as_ref()))
            .and_then(|builder| {
                builder
                    .timeout(connect_timeout)
                    .keep_alive_timeout(connect_timeout)
                    .tls_config(
                        ClientTlsConfig::new()
                            .with_enabled_roots()
                            .with_native_roots(),
                    )
            }) {
                Ok(builder) => builder.keep_alive_while_idle(true),
                Err(err) => {
                    error!("Invalid Yellowstone client config: {err:?}");
                    reconnect_counter = reconnect_counter.max(1);
                    continue 'restart_geyser_connection;
                }
            };

            let mut client = match client_builder.connect().await {
                Ok(c) => {
                    info!("Successful connection to geyser stream");
                    c
                }
                Err(err) => {
                    error!("Error connecting to geyser: {err:?}");
                    reconnect_counter = reconnect_counter.max(1);
                    continue 'restart_geyser_connection;
                }
            };

            let subscription = client.subscribe_with_request(Some(request.clone())).await;
            let stream = match subscription {
                Ok((_, stream)) => stream,
                Err(e) => {
                    error!("Error subscribing to stream {e:?}");
                    reconnect_counter = reconnect_counter.max(1);
                    continue 'restart_geyser_connection;
                }
            };

            tokio::pin!(stream);

            while let Some(data) = stream.next().await {
                let update = match data {
                    Ok(e) => e,
                    Err(e) => {
                        error!("Error on receiving geyser subscription: {e:?}");
                        break;
                    }
                };

                let Some(e) = update.update_oneof else {
                    continue;
                };

                match e {
                    UpdateOneof::Slot(e) => {
                        last_seen_slot = e.slot;
                        if self
                            .stream
                            .send(GeyserStream::StreamCompleteOnSlot(e.slot))
                            .is_err()
                        {
                            warn!("Geyser receiver closed");
                            return;
                        }
                    }
                    UpdateOneof::BlockMeta(e) => match Hash::from_str(&e.blockhash) {
                        Ok(hash) => {
                            if self
                                .stream
                                .send(GeyserStream::BlockHash { slot: e.slot, hash })
                                .is_err()
                            {
                                warn!("Geyser receiver closed");
                                return;
                            }
                        }
                        Err(err) => warn!("invalid blockhash from Geyser: {err:?}"),
                    },
                    UpdateOneof::TransactionStatus(e) => {
                        match Signature::try_from(e.signature.as_slice()) {
                            Ok(signature) => {
                                if self
                                    .stream
                                    .send(GeyserStream::TransactionStatus {
                                        slot: e.slot,
                                        signature,
                                        error: e.err.map(|err| format!("{err:?}")),
                                    })
                                    .is_err()
                                {
                                    warn!("Geyser receiver closed");
                                    return;
                                }
                            }
                            Err(err) => warn!("invalid signature from Geyser: {err:?}"),
                        }
                    }
                    UpdateOneof::Ping(_) | UpdateOneof::Pong(_) => {}
                    _ => {
                        // Ignore unhandled update types for this project.
                    }
                }
            }

            error!("Stream ended unexpectedly, restarting connection...");
        }
    }
}
