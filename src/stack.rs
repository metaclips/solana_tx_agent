use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_sdk::{hash::Hash, signature::Keypair};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::{
    agent::{AgentContext, AiAgent, RetryAction},
    config::Config,
    jito::client::{BundleEvent, JitoClient, JitoConfig},
    lifecycle::{CommitmentStage, FailureKind, LifecycleLogger, LifecycleRecord},
    networking::{Geyser, GeyserStream},
    tx_factory::{BuiltTransaction, TransactionFactory},
};

pub struct TxStack {
    config: Config,
    rpc: Arc<RpcClient>,
    payer: Arc<Keypair>,
    jito: JitoClient,
    agent: AiAgent,
    logger: LifecycleLogger,
    latest_slot: Arc<AtomicU64>,
    latest_blockhash: Arc<RwLock<Option<(u64, Hash)>>>,
}

impl TxStack {
    pub async fn connect(config: Config) -> anyhow::Result<Self> {
        let payer = Arc::new(config.payer_keypair()?);
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
            agent: AiAgent::new(&config),
            logger: LifecycleLogger::new(config.lifecycle_log_path.clone()),
            latest_slot: Arc::new(AtomicU64::new(0)),
            latest_blockhash: Arc::new(RwLock::new(None)),
            config,
            rpc,
            payer,
            jito,
        };
        stack.spawn_slot_stream();
        Ok(stack)
    }

    pub async fn submit_many(
        &self,
        count: usize,
        inject_expired_blockhash: bool,
    ) -> anyhow::Result<()> {
        for index in 0..count {
            let submission_id =
                format!("tx-agent-{}-{index}", chrono::Utc::now().timestamp_millis());
            self.submit_with_agent_retry(submission_id, inject_expired_blockhash && index == 0)
                .await?;
        }
        Ok(())
    }

    async fn submit_with_agent_retry(
        &self,
        submission_id: String,
        inject_expired_blockhash: bool,
    ) -> anyhow::Result<()> {
        let mut attempt = 0u32;
        let mut force_fresh_blockhash = !inject_expired_blockhash;
        let mut previous_tip = 0u64;

        loop {
            let result = self
                .submit_once(
                    submission_id.clone(),
                    attempt,
                    inject_expired_blockhash && attempt == 0,
                    force_fresh_blockhash,
                    previous_tip,
                )
                .await?;

            previous_tip = result.tip_lamports;
            if result.is_terminal_success() || attempt >= 2 {
                self.logger.append(&result).await?;
                return Ok(());
            }

            let failure = result.failure.clone().unwrap_or(FailureKind::Unknown);
            let tip = self.jito.tip_data().await;
            let decision = self
                .agent
                .decide_retry(&AgentContext {
                    submission_id: submission_id.clone(),
                    attempt,
                    failure: failure.clone(),
                    failure_detail: result.failure_detail.clone().unwrap_or_default(),
                    current_slot: Some(self.latest_slot.load(Ordering::Relaxed)),
                    leader_slot: result.leader_slot,
                    previous_tip_lamports: previous_tip,
                    blockhash_age_slots: result.submitted_slot.map(|slot| {
                        self.latest_slot
                            .load(Ordering::Relaxed)
                            .saturating_sub(slot)
                    }),
                    tip,
                })
                .await;

            let mut decision_record = result.clone();
            decision_record.agent_decision = Some(decision.clone());
            self.logger.append(&decision_record).await?;

            match decision.action {
                RetryAction::Retry => {
                    info!(
                        "agent retrying submission {} after {:?}: {}",
                        submission_id, failure, decision.reason
                    );
                    attempt += 1;
                    force_fresh_blockhash = decision.refresh_blockhash;
                    previous_tip = decision.tip_lamports;
                }
                RetryAction::Hold | RetryAction::Abort => {
                    info!(
                        "agent stopped submission {} after {:?}: {}",
                        submission_id, failure, decision.reason
                    );
                    return Ok(());
                }
            }
        }
    }

    async fn submit_once(
        &self,
        submission_id: String,
        attempt: u32,
        inject_expired_blockhash: bool,
        force_fresh_blockhash: bool,
        requested_tip_lamports: u64,
    ) -> anyhow::Result<LifecycleRecord> {
        self.wait_for_slot().await;

        let max_wait_slots = self.config.leader_lookahead_slots.saturating_mul(3).max(3);
        let leader_slot = self
            .jito
            .wait_for_leader_window(self.config.leader_lookahead_slots, max_wait_slots)
            .await?;

        let tip_data = self.jito.tip_data().await;
        let slot_pressure = self
            .jito
            .slots_until_next_leader()
            .await
            .map(|slots| 1.0 - (slots as f64 / self.config.leader_lookahead_slots.max(1) as f64))
            .unwrap_or(0.0);
        let tip_lamports = requested_tip_lamports.max(self.agent.choose_tip_lamports(
            &tip_data,
            slot_pressure,
            self.config.tip_floor_lamports,
        ));

        let (blockhash, last_valid_block_height) = if inject_expired_blockhash {
            (Hash::default(), None)
        } else if force_fresh_blockhash {
            let (blockhash, last_valid_block_height) = self
                .rpc
                .get_latest_blockhash_with_commitment(CommitmentConfig::processed())
                .await
                .context("failed fetching processed blockhash")?;
            (blockhash, Some(last_valid_block_height))
        } else {
            self.current_blockhash()
                .await
                .map(|(_, hash)| (hash, None))
                .unwrap_or_else(|| (Hash::default(), None))
        };

        let tip_account = self.jito.tip_account().await;
        let factory = TransactionFactory::new(tip_account, self.config.self_transfer_lamports);
        let built = factory.build(&self.payer, blockhash, tip_lamports)?;
        let mut record = LifecycleRecord::new(
            submission_id,
            attempt,
            built.signature,
            built.tip_lamports,
            Some(self.latest_slot.load(Ordering::Relaxed)),
            leader_slot,
            built.blockhash.to_string(),
            last_valid_block_height,
        );

        let bundle_events = self.jito.subscribe_bundle_events();
        let send_result = self.jito.send_bundle(&built.transaction).await;
        match send_result {
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

    async fn wait_for_lifecycle(
        &self,
        mut record: LifecycleRecord,
        built: BuiltTransaction,
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

    async fn apply_rpc_commitments(
        &self,
        record: &mut LifecycleRecord,
        signature: solana_sdk::signature::Signature,
    ) {
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
        signature: solana_sdk::signature::Signature,
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
        signature: solana_sdk::signature::Signature,
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

    async fn current_blockhash(&self) -> Option<(u64, Hash)> {
        *self.latest_blockhash.read().await
    }
}
