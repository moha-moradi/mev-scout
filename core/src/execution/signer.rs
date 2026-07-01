use alloy::eips::Encodable2718;
use alloy::consensus::{transaction::SignableTransaction, TxEip1559, TxEnvelope};
use alloy::network::TxSigner;
use alloy::primitives::{Address, Bytes, TxKind, U256};
use alloy::signers::local::PrivateKeySigner;

use crate::error;

/// Tracks and persists nonce state for a single chain.
#[derive(Debug, Clone)]
pub struct NonceManager {
    chain_id: u64,
    current_nonce: u64,
}

impl NonceManager {
    pub fn new(chain_id: u64, start_nonce: u64) -> Self {
        NonceManager {
            chain_id,
            current_nonce: start_nonce,
        }
    }

    pub fn next_nonce(&mut self) -> u64 {
        let nonce = self.current_nonce;
        self.current_nonce += 1;
        nonce
    }

    pub fn current_nonce(&self) -> u64 {
        self.current_nonce
    }

    pub fn set_nonce(&mut self, nonce: u64) {
        self.current_nonce = nonce;
    }
}

/// Manages private key, signing, and nonce tracking for on-chain execution.
#[derive(Debug, Clone)]
pub struct ExecutionSigner {
    signer: PrivateKeySigner,
    chain_id: u64,
    nonce_manager: NonceManager,
}

impl ExecutionSigner {
    pub fn from_private_key(key: &str, chain_id: u64) -> Result<Self, error::Error> {
        let signer = key
            .parse::<PrivateKeySigner>()
            .map_err(|e| error::Error::Other(format!("Invalid private key: {e}")))?;
        Ok(ExecutionSigner {
            signer,
            chain_id,
            nonce_manager: NonceManager::new(chain_id, 0),
        })
    }

    pub fn next_nonce(&mut self) -> u64 {
        self.nonce_manager.next_nonce()
    }

    pub fn address(&self) -> Address {
        self.signer.address()
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub async fn sign_eip1559(
        &self,
        to: Address,
        value: U256,
        data: Bytes,
        nonce: u64,
        gas_limit: u64,
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
    ) -> Result<Vec<u8>, error::Error> {
        let mut tx = TxEip1559 {
            chain_id: self.chain_id,
            nonce,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            gas_limit,
            to: TxKind::Call(to),
            value,
            input: data,
            access_list: Default::default(),
        };
        let sig = self
            .signer
            .sign_transaction(&mut tx)
            .await
            .map_err(|e| error::Error::Other(format!("signing failed: {e}")))?;
        let signed = tx.into_signed(sig);
        let envelope: TxEnvelope = signed.into();
        Ok(envelope.encoded_2718())
    }
}
