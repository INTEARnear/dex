//! What the OTC contract's records take in the dex's storage. Its methods
//! that add records charge them to the storage balance that the caller
//! deposited on the dex, so the CLI tops that up first when it can't pay.

use color_eyre::eyre::eyre;
use intear_dex_types::{AssetId, DexId};
use near_primitives::types::AccountId;
use near_sdk::PublicKey;

use super::storage::dex_storage_record_bytes;

// Each OTC collection lives under a one-byte prefix, a StorageKey variant
const COLLECTION_PREFIX_BYTES: usize = 1;
const U32_BYTES: usize = 4;
const U128_BYTES: usize = 16;
const NEAR_TOKEN_BYTES: usize = 16;
// near-sdk keeps a contract's state under the key STATE
const STATE_KEY_BYTES: usize = 5;
// A near-sdk map or set is stored as its prefix, a byte vector
const COLLECTION_BYTES: usize = U32_BYTES + COLLECTION_PREFIX_BYTES;
// Balances, authorized keys, storage balances, the two maps of nonces that
// expire, and the set of used nonces
const COLLECTION_COUNT: usize = 6;

fn key_bytes(key: &impl near_sdk::borsh::BorshSerialize) -> color_eyre::eyre::Result<usize> {
    near_sdk::borsh::to_vec(key)?
        .len()
        .checked_add(COLLECTION_PREFIX_BYTES)
        .ok_or_else(|| eyre!("Storage size overflow"))
}

/// The state that the first call of a new OTC dex stores, with its empty
/// collections; it has no initialization method, so the dex pays for it
pub fn initial_state_bytes(
    dex_id: &DexId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    dex_storage_record_bytes(
        dex_id,
        STATE_KEY_BYTES,
        COLLECTION_BYTES
            .checked_mul(COLLECTION_COUNT)
            .ok_or_else(|| eyre!("Storage size overflow"))?,
        extra_bytes_per_record,
    )
}

/// An account's storage balance, which its first storage deposit stores and
/// pays for
pub fn storage_balance_bytes(
    dex_id: &DexId,
    account_id: &AccountId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    dex_storage_record_bytes(
        dex_id,
        key_bytes(account_id)?,
        NEAR_TOKEN_BYTES
            .checked_mul(2)
            .ok_or_else(|| eyre!("Storage size overflow"))?,
        extra_bytes_per_record,
    )
}

/// An account's balance of one asset
pub fn balance_bytes(
    dex_id: &DexId,
    account_id: &AccountId,
    asset_id: &AssetId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    dex_storage_record_bytes(
        dex_id,
        key_bytes(&(account_id, asset_id))?,
        U128_BYTES,
        extra_bytes_per_record,
    )
}

/// An account's authorized key
pub fn authorized_key_bytes(
    dex_id: &DexId,
    account_id: &AccountId,
    key: &PublicKey,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    dex_storage_record_bytes(
        dex_id,
        key_bytes(account_id)?,
        near_sdk::borsh::to_vec(key)?.len(),
        extra_bytes_per_record,
    )
}
