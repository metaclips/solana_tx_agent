use std::fs;

use anyhow::Context;
use base64::Engine;
use chrono::Utc;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    signature::{Keypair, Signer},
    transaction::{Transaction, VersionedTransaction},
};
use solana_system_interface::instruction as system_instruction;

use crate::{
    config::ReportConfig,
    types::{ExpectedOutcome, GeneratedTransaction, TransactionKind},
};

pub(crate) fn generate_funded_tip_transaction(
    rpc: &RpcClient,
    payer: &Keypair,
    config: &ReportConfig,
    request_id: &str,
) -> anyhow::Result<GeneratedTransaction> {
    generate_tip_transfer_transaction(
        rpc,
        payer,
        config,
        request_id,
        TransactionKind::FundedTipTransfer,
        ExpectedOutcome::Success,
    )
}

pub(crate) fn generate_unfunded_tip_transaction(
    rpc: &RpcClient,
    config: &ReportConfig,
    request_id: &str,
) -> anyhow::Result<GeneratedTransaction> {
    let payer = Keypair::new();
    generate_tip_transfer_transaction(
        rpc,
        &payer,
        config,
        request_id,
        TransactionKind::UnfundedTipTransfer,
        ExpectedOutcome::Failure,
    )
}

fn generate_tip_transfer_transaction(
    rpc: &RpcClient,
    payer: &Keypair,
    config: &ReportConfig,
    request_id: &str,
    kind: TransactionKind,
    expected_outcome: ExpectedOutcome,
) -> anyhow::Result<GeneratedTransaction> {
    let blockhash = rpc.get_latest_blockhash()?;
    let instruction =
        system_instruction::transfer(&payer.pubkey(), &config.tip_account, config.tip_lamports);
    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    let versioned = VersionedTransaction::from(transaction);
    let signature = versioned
        .signatures
        .first()
        .copied()
        .context("constructed transaction has no signature")?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bincode::serialize(&versioned)?);
    let transaction_file = config.work_dir.join(format!("{request_id}.base64"));
    fs::write(&transaction_file, encoded)
        .with_context(|| format!("failed writing {}", transaction_file.display()))?;

    Ok(GeneratedTransaction {
        request_id: request_id.to_string(),
        kind,
        expected_outcome,
        encoding: "base64".to_string(),
        transaction_file,
        payer: payer.pubkey().to_string(),
        tip_account: config.tip_account.to_string(),
        tip_lamports: config.tip_lamports,
        blockhash: blockhash.to_string(),
        signature: signature.to_string(),
        constructed_at: Utc::now(),
    })
}
