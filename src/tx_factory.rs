use solana_sdk::{
    hash::Hash,
    message::{VersionedMessage, v0},
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::VersionedTransaction,
};
use solana_system_interface::instruction as system_instruction;

#[derive(Debug, Clone)]
pub struct BuiltTransaction {
    pub transaction: VersionedTransaction,
    pub signature: Signature,
    pub blockhash: Hash,
    pub tip_lamports: u64,
}

#[derive(Debug, Clone)]
pub struct TransactionFactory {
    tip_account: Pubkey,
    self_transfer_lamports: u64,
}

impl TransactionFactory {
    pub fn new(tip_account: Pubkey, self_transfer_lamports: u64) -> Self {
        Self {
            tip_account,
            self_transfer_lamports,
        }
    }

    pub fn build(
        &self,
        payer: &Keypair,
        blockhash: Hash,
        tip_lamports: u64,
    ) -> anyhow::Result<BuiltTransaction> {
        let payer_pubkey = payer.pubkey();
        let instructions = vec![
            system_instruction::transfer(&payer_pubkey, &payer_pubkey, self.self_transfer_lamports),
            system_instruction::transfer(&payer_pubkey, &self.tip_account, tip_lamports),
        ];
        let message = v0::Message::try_compile(&payer_pubkey, &instructions, &[], blockhash)?;
        let transaction = VersionedTransaction::try_new(VersionedMessage::V0(message), &[payer])?;
        let signature = transaction.signatures[0];

        Ok(BuiltTransaction {
            transaction,
            signature,
            blockhash,
            tip_lamports,
        })
    }
}
