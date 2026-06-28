use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use base64::Engine;
use serde::{Deserialize, Serialize};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_sdk::{hash::Hash, signature::Signature, transaction::VersionedTransaction};
use tokio::sync::RwLock;
use tracing::warn;

use crate::{
    config::Config,
    jito::{
        client::{BundleEvent, JitoClient, JitoConfig},
        tip::TipData,
    },
    lifecycle::{AgentAuditRecord, CommitmentStage, FailureKind, LifecycleLogger, LifecycleRecord},
    networking::{Geyser, GeyserStream},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkState {
    pub current_slot: u64,
    pub latest_blockhash_slot: Option<u64>,
    pub next_jito_leader_slot: Option<u64>,
    pub slots_until_jito_leader: Option<u64>,
    pub recent_tip: TipData,
    pub tip_floor_lamports: u64,
    pub max_tip_lamports: u64,
    pub leader_lookahead_slots: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlledSignedSubmitRequest {
    pub submission_id: String,
    pub attempt: u32,
    pub encoded_transaction: String,
    pub encoding: SignedTransactionEncoding,
    pub wait_for_leader: bool,
    pub max_wait_slots: Option<u64>,
    pub observed_tip_lamports: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignedTransactionEncoding {
    Base64,
    Base58,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureReport {
    pub submission_id: String,
    pub attempt: u32,
    pub failure: FailureKind,
    pub detail: String,
    pub submitted_slot: Option<u64>,
    pub leader_slot: Option<u64>,
    pub blockhash: String,
    pub tip_lamports: u64,
    pub processed_latency_ms: Option<u128>,
    pub confirmed_latency_ms: Option<u128>,
}

#[derive(Debug, Clone)]
struct SignedBundleTransaction {
    transaction: VersionedTransaction,
    signature: Signature,
    blockhash: Hash,
    tip_lamports: u64,
}

pub struct TxStack {
    config: Config,
    rpc: Arc<RpcClient>,
    jito: JitoClient,
    logger: LifecycleLogger,
    latest_slot: Arc<AtomicU64>,
    latest_blockhash: Arc<RwLock<Option<(u64, Hash)>>>,
}

impl TxStack {
    pub async fn connect(config: Config) -> anyhow::Result<Self> {
        let jito_auth = Arc::new(config.jito_auth_keypair()?);
        let rpc = Arc::new(RpcClient::new_with_commitment(
            config.solana_rpc_url.clone(),
            CommitmentConfig::processed(),
        ));
        let jito = JitoClient::connect(
            JitoConfig {
                block_engine_url: config.jito_block_engine_url.clone(),
            },
            jito_auth,
        )
        .await?;

        let stack = Self {
            logger: LifecycleLogger::new(config.lifecycle_log_path.clone()),
            latest_slot: Arc::new(AtomicU64::new(0)),
            latest_blockhash: Arc::new(RwLock::new(None)),
            config,
            rpc,
            jito,
        };
        stack.spawn_slot_stream();
        Ok(stack)
    }

    pub async fn operational_state(&self) -> NetworkState {
        let current_slot = self.latest_slot.load(Ordering::Relaxed);
        let latest_blockhash_slot = self.latest_blockhash.read().await.map(|(slot, _)| slot);
        let next = self.jito.next_leader_slot().await.map(|(slot, _)| slot);
        NetworkState {
            current_slot,
            latest_blockhash_slot,
            next_jito_leader_slot: next,
            slots_until_jito_leader: next.map(|slot| slot.saturating_sub(current_slot)),
            recent_tip: self.jito.tip_data().await,
            tip_floor_lamports: self.config.tip_floor_lamports,
            max_tip_lamports: self.config.max_agent_tip_lamports,
            leader_lookahead_slots: self.config.leader_lookahead_slots,
        }
    }

    pub async fn submit_signed_encoded(
        &self,
        request: ControlledSignedSubmitRequest,
    ) -> anyhow::Result<LifecycleRecord> {
        self.wait_for_slot().await;

        let leader_slot = if request.wait_for_leader {
            self.jito
                .wait_for_leader_window(
                    self.config.leader_lookahead_slots,
                    request
                        .max_wait_slots
                        .unwrap_or(self.config.max_agent_wait_slots),
                )
                .await?
        } else {
            self.jito.next_leader_slot().await.map(|(slot, _)| slot)
        };

        let built = decode_signed_transaction(
            &request.encoded_transaction,
            &request.encoding,
            request.observed_tip_lamports.unwrap_or(0),
        )?;
        let mut record = LifecycleRecord::new(
            request.submission_id,
            request.attempt,
            built.signature,
            built.tip_lamports,
            Some(self.latest_slot.load(Ordering::Relaxed)),
            leader_slot,
            built.blockhash.to_string(),
            None,
        );

        let bundle_events = self.jito.subscribe_bundle_events();
        match self.jito.send_bundle(&built.transaction).await {
            Ok(sent) => {
                record.bundle_id = Some(sent.bundle_id.clone());
                record.add_stage(
                    CommitmentStage::Submitted,
                    Some(self.latest_slot.load(Ordering::Relaxed)),
                    Some(format!("bundle_id={}", sent.bundle_id)),
                );
            }
            Err(err) => {
                let detail = format!("{err:?}");
                record.fail(FailureKind::classify(&detail), detail);
                return Ok(record);
            }
        }

        Ok(self.wait_for_lifecycle(record, built, bundle_events).await)
    }

    pub async fn log_lifecycle(&self, record: &LifecycleRecord) -> anyhow::Result<()> {
        self.logger.append(record).await
    }

    pub async fn log_agent_audit(&self, record: &AgentAuditRecord) -> anyhow::Result<()> {
        self.logger.append_agent_audit(record).await
    }

    pub fn failure_report(record: &LifecycleRecord) -> Option<FailureReport> {
        let failure = record.failure.clone()?;
        Some(FailureReport {
            submission_id: record.submission_id.clone(),
            attempt: record.attempt,
            failure,
            detail: record.failure_detail.clone().unwrap_or_default(),
            submitted_slot: record.submitted_slot,
            leader_slot: record.leader_slot,
            blockhash: record.blockhash.clone(),
            tip_lamports: record.tip_lamports,
            processed_latency_ms: record.processed_latency_ms,
            confirmed_latency_ms: record.confirmed_latency_ms,
        })
    }

    async fn wait_for_lifecycle(
        &self,
        mut record: LifecycleRecord,
        built: SignedBundleTransaction,
        mut bundle_events: tokio::sync::broadcast::Receiver<BundleEvent>,
    ) -> LifecycleRecord {
        let mut signature_events = self.spawn_signature_status_stream(built.signature);
        let mut poll = tokio::time::interval(Duration::from_millis(500));
        let timeout = tokio::time::sleep(self.config.confirmation_timeout);
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                event = bundle_events.recv() => {
                    match event {
                        Ok(event) => self.apply_bundle_event(&mut record, event),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!("bundle event receiver lagged by {skipped} messages");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            record.fail(FailureKind::BundleFailure, "Jito bundle event stream closed");
                            return record;
                        }
                    }
                }
                event = signature_events.recv() => {
                    if let Some(event) = event {
                        match event {
                            GeyserStream::TransactionStatus { slot, signature, error }
                                if signature == built.signature =>
                            {
                                if let Some(error) = error {
                                    let kind = FailureKind::classify(&error);
                                    record.fail(kind, error);
                                    return record;
                                }
                                record.add_stage(
                                    CommitmentStage::Processed,
                                    Some(slot),
                                    Some("observed by Yellowstone transactions_status".to_string()),
                                );
                            }
                            _ => {}
                        }
                    }
                }
                _ = poll.tick() => {
                    self.apply_rpc_commitments(&mut record, built.signature).await;
                    if record.failure.is_some() || record.finalized_at.is_some() {
                        return record;
                    }
                }
                _ = &mut timeout => {
                    if !record.is_terminal_success() {
                        record.fail(FailureKind::Timeout, "confirmation timeout elapsed");
                    }
                    return record;
                }
            }
        }
    }

    fn apply_bundle_event(&self, record: &mut LifecycleRecord, event: BundleEvent) {
        let Some(bundle_id) = &record.bundle_id else {
            return;
        };

        match event {
            BundleEvent::Accepted {
                bundle_id: event_bundle_id,
                slot,
                validator_identity,
            } if &event_bundle_id == bundle_id => record.add_stage(
                CommitmentStage::Accepted,
                Some(slot),
                Some(format!("forwarded_to={validator_identity}")),
            ),
            BundleEvent::Processed {
                bundle_id: event_bundle_id,
                slot,
                validator_identity,
            } if &event_bundle_id == bundle_id => record.add_stage(
                CommitmentStage::Processed,
                Some(slot),
                Some(format!("jito_validator={validator_identity}")),
            ),
            BundleEvent::Finalized {
                bundle_id: event_bundle_id,
            } if &event_bundle_id == bundle_id => {
                record.add_stage(
                    CommitmentStage::Finalized,
                    None,
                    Some("Jito finalized".to_string()),
                );
            }
            BundleEvent::Dropped {
                bundle_id: event_bundle_id,
                reason,
            } if &event_bundle_id == bundle_id => {
                record.fail(FailureKind::classify(&reason), reason);
            }
            BundleEvent::Rejected {
                bundle_id: event_bundle_id,
                reason,
            } if &event_bundle_id == bundle_id => {
                record.fail(FailureKind::classify(&reason), reason);
            }
            _ => {}
        }
    }

    async fn apply_rpc_commitments(&self, record: &mut LifecycleRecord, signature: Signature) {
        self.apply_rpc_commitment(
            record,
            signature,
            CommitmentStage::Processed,
            CommitmentConfig::processed(),
        )
        .await;
        self.apply_rpc_commitment(
            record,
            signature,
            CommitmentStage::Confirmed,
            CommitmentConfig::confirmed(),
        )
        .await;
        self.apply_rpc_commitment(
            record,
            signature,
            CommitmentStage::Finalized,
            CommitmentConfig::finalized(),
        )
        .await;
    }

    async fn apply_rpc_commitment(
        &self,
        record: &mut LifecycleRecord,
        signature: Signature,
        stage: CommitmentStage,
        commitment: CommitmentConfig,
    ) {
        match self
            .rpc
            .get_signature_status_with_commitment(&signature, commitment)
            .await
        {
            Ok(Some(Ok(()))) => {
                record.add_stage(stage, None, Some("RPC commitment fallback".to_string()));
            }
            Ok(Some(Err(err))) => {
                let detail = format!("{err:?}");
                record.fail(FailureKind::classify(&detail), detail);
            }
            Ok(None) => {}
            Err(err) => warn!("RPC status lookup failed for {signature}: {err:?}"),
        }
    }

    fn spawn_signature_status_stream(
        &self,
        signature: Signature,
    ) -> tokio::sync::mpsc::UnboundedReceiver<GeyserStream> {
        let (send, receive) = tokio::sync::mpsc::unbounded_channel();
        let geyser = Arc::new(Geyser { stream: send });
        let endpoint = self.config.yellowstone_endpoint.clone();
        let token = self.config.yellowstone_token.clone();

        tokio::spawn(async move {
            geyser
                .new_geyser_signature_status_subscription(endpoint, token, signature)
                .await;
        });

        receive
    }

    fn spawn_slot_stream(&self) {
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let geyser = Arc::new(Geyser { stream: send });
        let endpoint = self.config.yellowstone_endpoint.clone();
        let token = self.config.yellowstone_token.clone();
        let latest_slot = self.latest_slot.clone();
        let latest_blockhash = self.latest_blockhash.clone();
        let jito = self.jito.clone();

        tokio::spawn(async move {
            geyser.new_geyser_slot_subscription(endpoint, token).await;
        });

        tokio::spawn(async move {
            while let Some(event) = receive.recv().await {
                match event {
                    GeyserStream::BlockHash { slot, hash } => {
                        latest_slot.store(slot, Ordering::Relaxed);
                        jito.update_current_slot(slot);
                        *latest_blockhash.write().await = Some((slot, hash));
                    }
                    GeyserStream::StreamCompleteOnSlot(slot) => {
                        latest_slot.store(slot, Ordering::Relaxed);
                        jito.update_current_slot(slot);
                    }
                    _ => {}
                }
            }
        });
    }

    async fn wait_for_slot(&self) {
        while self.latest_slot.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn decode_signed_transaction(
    encoded_transaction: &str,
    encoding: &SignedTransactionEncoding,
    observed_tip_lamports: u64,
) -> anyhow::Result<SignedBundleTransaction> {
    let bytes = match encoding {
        SignedTransactionEncoding::Base64 => base64::engine::general_purpose::STANDARD
            .decode(encoded_transaction.trim())
            .context("failed to decode base64 signed transaction")?,
        SignedTransactionEncoding::Base58 => bs58::decode(encoded_transaction.trim())
            .into_vec()
            .context("failed to decode base58 signed transaction")?,
    };
    let transaction: VersionedTransaction =
        bincode::deserialize(&bytes).context("failed to deserialize signed transaction")?;
    transaction
        .sanitize()
        .context("signed transaction failed sanitization")?;
    transaction
        .verify_and_hash_message()
        .context("signed transaction signature verification failed")?;

    let signature: Signature = *transaction
        .signatures
        .first()
        .context("signed transaction has no signatures")?;
    let blockhash = *transaction.message.recent_blockhash();

    Ok(SignedBundleTransaction {
        transaction,
        signature,
        blockhash,
        tip_lamports: observed_tip_lamports,
    })
}
