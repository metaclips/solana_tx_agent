use std::{
    collections::BTreeMap,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::StreamExt;
use solana_sdk::{
    clock::Slot, pubkey, pubkey::Pubkey, signature::Keypair, transaction::VersionedTransaction,
};
use tokio::{
    sync::{Mutex, RwLock, broadcast, mpsc},
    time::{interval, sleep},
};
use tonic::{
    Code, Streaming,
    service::interceptor::InterceptedService,
    transport::{Channel, Endpoint},
};
use tracing::{error, info, warn};

use crate::jito::{
    protos::{
        auth::auth_service_client::AuthServiceClient,
        bundle::{Bundle, BundleResult, bundle_result},
        packet::{Meta as ProtoMeta, Packet as ProtoPacket},
        searcher::{
            ConnectedLeadersRequest, ConnectedLeadersResponse, GetTipAccountsRequest,
            SendBundleRequest, SubscribeBundleResultsRequest,
            searcher_service_client::SearcherServiceClient,
        },
    },
    tip::TipData,
};

type SearcherClient =
    SearcherServiceClient<InterceptedService<Channel, crate::jito::interceptor::ClientInterceptor>>;

const MAX_RETRIES: usize = 5;
const MAX_RATE_LIMIT_RETRIES: usize = 5;
const DEFAULT_RATE_LIMIT_RETRY_MS: u64 = 1_000;
const DEFAULT_JITO_TIP_ADDRESS: Pubkey = pubkey!("DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh");

#[derive(Debug, Clone)]
pub struct JitoConfig {
    pub block_engine_url: String,
}

#[derive(Debug, Clone)]
pub struct SentBundle {
    pub bundle_id: String,
}

#[derive(Debug, Clone)]
pub enum BundleEvent {
    Accepted {
        bundle_id: String,
        slot: u64,
        validator_identity: String,
    },
    Processed {
        bundle_id: String,
        slot: u64,
        validator_identity: String,
    },
    Finalized {
        bundle_id: String,
    },
    Dropped {
        bundle_id: String,
        reason: String,
    },
    Rejected {
        bundle_id: String,
        reason: String,
    },
}

#[derive(Clone)]
pub struct JitoClient {
    searcher: Arc<Mutex<SearcherClient>>,
    current_slot: Arc<AtomicU64>,
    leader_schedule: Arc<RwLock<BTreeMap<Slot, Pubkey>>>,
    tip: Arc<RwLock<TipData>>,
    tip_accounts: Arc<RwLock<Vec<Pubkey>>>,
    bundle_events: broadcast::Sender<BundleEvent>,
}

impl JitoClient {
    pub async fn connect(config: JitoConfig, auth_keypair: Arc<Keypair>) -> anyhow::Result<Self> {
        let engine_connect = Self::grpc_connect(&config.block_engine_url).await?;
        let interceptor = crate::jito::interceptor::ClientInterceptor::new(
            auth_keypair,
            AuthServiceClient::new(engine_connect.clone()),
        )
        .await?;

        let mut searcher = SearcherServiceClient::with_interceptor(engine_connect, interceptor);
        let bundle_stream = searcher
            .subscribe_bundle_results(SubscribeBundleResultsRequest {})
            .await?
            .into_inner();

        let mut tip_stream = crate::jito::tip::new_jito_tip_stream().await;
        let tip = Self::fetch_tip_data(&mut tip_stream).await?;
        let tip_accounts = Self::fetch_tip_accounts_with_fallback(&mut searcher).await;

        let (bundle_events, _) = broadcast::channel(2048);
        let client = Self {
            searcher: Arc::new(Mutex::new(searcher)),
            current_slot: Arc::new(AtomicU64::new(0)),
            leader_schedule: Arc::new(RwLock::new(BTreeMap::new())),
            tip: Arc::new(RwLock::new(tip)),
            tip_accounts: Arc::new(RwLock::new(tip_accounts)),
            bundle_events,
        };

        client.refresh_leader_schedule().await;
        client.spawn_background(bundle_stream, tip_stream);
        Ok(client)
    }

    pub fn subscribe_bundle_events(&self) -> broadcast::Receiver<BundleEvent> {
        self.bundle_events.subscribe()
    }

    pub async fn tip_data(&self) -> TipData {
        self.tip.read().await.clone()
    }

    pub async fn tip_account(&self) -> Pubkey {
        self.tip_accounts
            .read()
            .await
            .first()
            .copied()
            .unwrap_or(DEFAULT_JITO_TIP_ADDRESS)
    }

    pub fn update_current_slot(&self, slot: u64) {
        self.current_slot.store(slot, Ordering::Relaxed);
    }

    pub fn current_slot(&self) -> u64 {
        self.current_slot.load(Ordering::Relaxed)
    }

    pub async fn next_leader_slot(&self) -> Option<(Slot, Pubkey)> {
        let current_slot = self.current_slot();
        self.leader_schedule
            .read()
            .await
            .iter()
            .find(|(slot, _)| **slot >= current_slot)
            .map(|(slot, leader)| (*slot, *leader))
    }

    pub async fn slots_until_next_leader(&self) -> Option<u64> {
        let current_slot = self.current_slot();
        self.next_leader_slot()
            .await
            .map(|(slot, _)| slot.saturating_sub(current_slot))
    }

    pub async fn wait_for_leader_window(
        &self,
        lookahead_slots: u64,
        max_wait_slots: u64,
    ) -> anyhow::Result<Option<u64>> {
        let start_slot = self.current_slot();
        loop {
            if let Some((leader_slot, _)) = self.next_leader_slot().await {
                let current_slot = self.current_slot();
                if leader_slot <= current_slot.saturating_add(lookahead_slots) {
                    return Ok(Some(leader_slot));
                }
                if current_slot.saturating_sub(start_slot) >= max_wait_slots {
                    return Ok(None);
                }
            } else {
                self.refresh_leader_schedule().await;
            }
            sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn send_bundle(
        &self,
        transaction: &VersionedTransaction,
    ) -> anyhow::Result<SentBundle> {
        let mut retry_attempt = 0usize;
        loop {
            let request = SendBundleRequest {
                bundle: Some(Bundle {
                    header: None,
                    packets: vec![Self::serialize_versioned_tx(transaction)?],
                }),
            };

            let response = self.searcher.lock().await.send_bundle(request).await;
            match response {
                Ok(resp) => {
                    let bundle_id = resp.into_inner().uuid;
                    info!("Jito accepted bundle submission {bundle_id}");
                    return Ok(SentBundle { bundle_id });
                }
                Err(status) if status.code() == Code::ResourceExhausted => {
                    retry_attempt += 1;
                    if retry_attempt > MAX_RATE_LIMIT_RETRIES {
                        anyhow::bail!(
                            "Jito rate limited send_bundle after {MAX_RATE_LIMIT_RETRIES} retries: {status:?}"
                        );
                    }
                    let retry_after_ms = retry_after_from_status(&status);
                    warn!("Jito rate limited send_bundle, retrying in {retry_after_ms}ms");
                    sleep(Duration::from_millis(retry_after_ms)).await;
                }
                Err(status) => anyhow::bail!("Jito send_bundle failed: {status:?}"),
            }
        }
    }

    async fn grpc_connect(url: &str) -> anyhow::Result<Channel> {
        let endpoint = if url.starts_with("https://") {
            Endpoint::from_shared(url.to_string())?
                .tls_config(tonic::transport::ClientTlsConfig::new().with_native_roots())?
        } else {
            Endpoint::from_shared(url.to_string())?
        };
        Ok(endpoint.connect().await?)
    }

    async fn fetch_tip_data(
        tip_stream: &mut mpsc::UnboundedReceiver<TipData>,
    ) -> anyhow::Result<TipData> {
        match tokio::time::timeout(Duration::from_secs(5), tip_stream.recv()).await {
            Ok(Some(tip)) => Ok(tip),
            Ok(None) => anyhow::bail!("Jito tip stream closed before first tip"),
            Err(err) => {
                warn!("Jito tip stream timed out, using HTTP fallback: {err:?}");
                crate::jito::tip::get_tip_data_via_reqwest().await
            }
        }
    }

    async fn fetch_tip_accounts_with_fallback(searcher: &mut SearcherClient) -> Vec<Pubkey> {
        match searcher.get_tip_accounts(GetTipAccountsRequest {}).await {
            Ok(resp) => resp
                .into_inner()
                .accounts
                .into_iter()
                .filter_map(|account| Pubkey::from_str(&account).ok())
                .collect::<Vec<_>>(),
            Err(err) => {
                warn!("failed to fetch Jito tip accounts, using fallback: {err:?}");
                vec![DEFAULT_JITO_TIP_ADDRESS]
            }
        }
    }

    async fn refresh_leader_schedule(&self) {
        let current_slot = self.current_slot();
        let response = {
            let mut searcher = self.searcher.lock().await;
            Self::fetch_connected_leaders_with_retries(&mut searcher, MAX_RETRIES).await
        };

        let Some(response) = response else {
            error!("failed to refresh Jito connected leader schedule");
            return;
        };

        let mut schedule = BTreeMap::new();
        for (validator_identity, slots) in response.connected_validators {
            let Ok(validator_identity) = Pubkey::from_str(&validator_identity) else {
                warn!("invalid Jito validator identity in leader response: {validator_identity}");
                continue;
            };
            for slot in slots.slots.into_iter().filter(|slot| *slot >= current_slot) {
                schedule.insert(slot, validator_identity);
            }
        }

        let schedule_len = schedule.len();
        *self.leader_schedule.write().await = schedule;
        info!("refreshed Jito connected leader schedule with {schedule_len} future slots");
    }

    async fn fetch_connected_leaders_with_retries(
        searcher: &mut SearcherClient,
        max_retries: usize,
    ) -> Option<ConnectedLeadersResponse> {
        for attempt in 1..=max_retries {
            match searcher
                .get_connected_leaders(ConnectedLeadersRequest {})
                .await
            {
                Ok(resp) => return Some(resp.into_inner()),
                Err(err) => {
                    warn!("get_connected_leaders failed attempt {attempt}/{max_retries}: {err:?}");
                    sleep(Duration::from_millis(DEFAULT_RATE_LIMIT_RETRY_MS)).await;
                }
            }
        }
        None
    }

    fn spawn_background(
        &self,
        bundle_stream: Streaming<BundleResult>,
        mut tip_stream: mpsc::UnboundedReceiver<TipData>,
    ) {
        let client = self.clone();
        tokio::spawn(async move {
            let mut bundle_stream = bundle_stream;
            let mut leader_interval = interval(Duration::from_secs(30));

            loop {
                tokio::select! {
                    tip = tip_stream.recv() => {
                        match tip {
                            Some(tip) => *client.tip.write().await = tip,
                            None => warn!("Jito tip stream closed"),
                        }
                    }
                    bundle = bundle_stream.next() => {
                        match bundle {
                            Some(Ok(bundle)) => client.emit_bundle_result(bundle),
                            Some(Err(err)) => {
                                error!("Jito bundle result stream error: {err:?}");
                                match client.resubscribe_bundle_results().await {
                                    Some(new_stream) => bundle_stream = new_stream,
                                    None => return,
                                }
                            }
                            None => {
                                error!("Jito bundle result stream ended");
                                match client.resubscribe_bundle_results().await {
                                    Some(new_stream) => bundle_stream = new_stream,
                                    None => return,
                                }
                            }
                        }
                    }
                    _ = leader_interval.tick() => {
                        client.refresh_leader_schedule().await;
                    }
                }
            }
        });
    }

    async fn resubscribe_bundle_results(&self) -> Option<Streaming<BundleResult>> {
        for attempt in 1..=MAX_RETRIES {
            let response = self
                .searcher
                .lock()
                .await
                .subscribe_bundle_results(SubscribeBundleResultsRequest {})
                .await;
            match response {
                Ok(resp) => {
                    info!("resubscribed to Jito bundle results on attempt {attempt}");
                    return Some(resp.into_inner());
                }
                Err(err) => {
                    warn!(
                        "failed to resubscribe bundle results attempt {attempt}/{MAX_RETRIES}: {err:?}"
                    );
                    sleep(Duration::from_millis(DEFAULT_RATE_LIMIT_RETRY_MS)).await;
                }
            }
        }
        None
    }

    fn emit_bundle_result(&self, bundle: BundleResult) {
        let bundle_id = bundle.bundle_id.clone();
        let event = match bundle.result {
            Some(bundle_result::Result::Accepted(accepted)) => BundleEvent::Accepted {
                bundle_id,
                slot: accepted.slot,
                validator_identity: accepted.validator_identity,
            },
            Some(bundle_result::Result::Processed(processed)) => BundleEvent::Processed {
                bundle_id,
                slot: processed.slot,
                validator_identity: processed.validator_identity,
            },
            Some(bundle_result::Result::Finalized(_)) => BundleEvent::Finalized { bundle_id },
            Some(bundle_result::Result::Dropped(dropped)) => BundleEvent::Dropped {
                bundle_id,
                reason: format!("{:?}", dropped.reason()),
            },
            Some(bundle_result::Result::Rejected(rejected)) => BundleEvent::Rejected {
                bundle_id,
                reason: format!("{:?}", rejected.reason),
            },
            None => BundleEvent::Rejected {
                bundle_id,
                reason: "empty Jito bundle result".to_string(),
            },
        };

        let _ = self.bundle_events.send(event);
    }

    fn serialize_versioned_tx(tx: &VersionedTransaction) -> anyhow::Result<ProtoPacket> {
        let data = bincode::serialize(tx)?;
        let size = data.len() as u64;
        Ok(ProtoPacket {
            data,
            meta: Some(ProtoMeta {
                size,
                addr: String::new(),
                port: 0,
                flags: None,
                sender_stake: 0,
            }),
        })
    }
}

fn retry_after_from_status(status: &tonic::Status) -> u64 {
    status
        .metadata()
        .get("x-wait-to-retry-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_RATE_LIMIT_RETRY_MS)
}
