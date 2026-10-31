//! What the xyk contract's records take in the dex's storage. Its methods
//! that add records charge the NEAR attached to them for it and send the
//! rest back, so the CLI attaches what these add up to.

use color_eyre::eyre::eyre;
use intear_dex_types::{AssetId, DexId};
use near_primitives::types::AccountId;
use xyk_dex_types::FeeConfigurationV2;

use super::storage::dex_storage_record_bytes;

// Each xyk collection lives under a one-byte prefix, a StorageKey variant
const COLLECTION_PREFIX_BYTES: usize = 1;
const POOL_ID_BYTES: usize = 4;
// Pools are elements of a vector, keyed by their index
const POOL_KEY_BYTES: usize = COLLECTION_PREFIX_BYTES + POOL_ID_BYTES;
const U32_BYTES: usize = 4;
const U128_BYTES: usize = 16;
const ENUM_TAG_BYTES: usize = 1;
const BOOL_BYTES: usize = 1;
// A public pool's share map is stored as its prefix: the StorageKey variant
// and the pool id, as a length-prefixed byte vector
const SHARE_MAP_BYTES: usize = U32_BYTES + COLLECTION_PREFIX_BYTES + POOL_ID_BYTES;
// A 256-bit fee growth in both assets
const FEE_GROWTH_BYTES: usize = U128_BYTES * 4;
// The shares and fee growth of each of the three payout groups
const PAYOUT_GROUPS_BYTES: usize = (U128_BYTES + FEE_GROWTH_BYTES) * 3;
// A provider's payout, shares, and the fee growth it last collected at
const LIQUIDITY_POSITION_BYTES: usize = ENUM_TAG_BYTES + U128_BYTES + FEE_GROWTH_BYTES;

// near-sdk keeps a contract's state under the key STATE
const STATE_KEY_BYTES: usize = 5;
// A near-sdk map or vector is stored as its prefix, a byte vector; a vector
// also stores its length before that
const MAP_BYTES: usize = U32_BYTES + COLLECTION_PREFIX_BYTES;
const VECTOR_BYTES: usize = U32_BYTES + MAP_BYTES;

/// The state that initializing an xyk dex stores: the pool vector, the maps
/// of collected fees, unused referral settings and community fees, no fee
/// discount signer, and the map of fees that anyone can claim
pub fn initial_state_bytes(
    dex_id: &DexId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    dex_storage_record_bytes(
        dex_id,
        STATE_KEY_BYTES,
        sum_bytes(&[
            VECTOR_BYTES,
            MAP_BYTES,
            MAP_BYTES,
            MAP_BYTES,
            ENUM_TAG_BYTES,
            MAP_BYTES,
        ])?,
        extra_bytes_per_record,
    )
}

pub enum NewPoolKind<'owner> {
    Private { owner_id: &'owner AccountId },
    Public,
    Launch,
}

fn sum_bytes(parts: &[usize]) -> color_eyre::eyre::Result<usize> {
    parts
        .iter()
        .try_fold(0usize, |sum, part| sum.checked_add(*part))
        .ok_or_else(|| eyre!("Storage size overflow"))
}

fn borsh_bytes(value: &impl near_sdk::borsh::BorshSerialize) -> color_eyre::eyre::Result<usize> {
    Ok(near_sdk::borsh::to_vec(value)?.len())
}

/// A pool as a new element of the pool list
pub fn new_pool_bytes(
    dex_id: &DexId,
    kind: NewPoolKind,
    assets: (&AssetId, &AssetId),
    fees: &FeeConfigurationV2,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    let asset_with_balance_bytes =
        |asset_id: &AssetId| sum_bytes(&[borsh_bytes(asset_id)?, U128_BYTES]);
    let fees_bytes = borsh_bytes(fees)?;
    let pool_bytes = match kind {
        NewPoolKind::Private { owner_id } => sum_bytes(&[
            ENUM_TAG_BYTES,
            asset_with_balance_bytes(assets.0)?,
            asset_with_balance_bytes(assets.1)?,
            borsh_bytes(owner_id)?,
            fees_bytes,
            BOOL_BYTES,
        ])?,
        // The creator's first liquidity mints the share total
        NewPoolKind::Public => sum_bytes(&[
            ENUM_TAG_BYTES,
            asset_with_balance_bytes(assets.0)?,
            asset_with_balance_bytes(assets.1)?,
            fees_bytes,
            SHARE_MAP_BYTES,
            ENUM_TAG_BYTES,
            U128_BYTES,
            PAYOUT_GROUPS_BYTES,
        ])?,
        // The quote asset, the launched asset, and the phantom liquidity
        NewPoolKind::Launch => sum_bytes(&[
            ENUM_TAG_BYTES,
            asset_with_balance_bytes(assets.0)?,
            asset_with_balance_bytes(assets.1)?,
            fees_bytes,
            U128_BYTES,
        ])?,
    };
    dex_storage_record_bytes(dex_id, POOL_KEY_BYTES, pool_bytes, extra_bytes_per_record)
}

/// What a pool's record grows by when its fees change; a shrinking record
/// frees storage instead
pub fn fee_configuration_growth_bytes(
    current_fees: &FeeConfigurationV2,
    new_fees: &FeeConfigurationV2,
) -> color_eyre::eyre::Result<u64> {
    let growth = borsh_bytes(new_fees)?.saturating_sub(borsh_bytes(current_fees)?);
    Ok(u64::try_from(growth)?)
}

/// The fees an account collected in one asset
pub fn fee_balance_bytes(
    dex_id: &DexId,
    account_id: &AccountId,
    asset_id: &AssetId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    dex_storage_record_bytes(
        dex_id,
        sum_bytes(&[
            COLLECTION_PREFIX_BYTES,
            borsh_bytes(&(account_id, asset_id))?,
        ])?,
        U128_BYTES,
        extra_bytes_per_record,
    )
}

/// An account's position in a public pool, without shares yet
pub fn liquidity_registration_bytes(
    dex_id: &DexId,
    account_id: &AccountId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    dex_storage_record_bytes(
        dex_id,
        sum_bytes(&[
            COLLECTION_PREFIX_BYTES,
            POOL_ID_BYTES,
            borsh_bytes(account_id)?,
        ])?,
        LIQUIDITY_POSITION_BYTES,
        extra_bytes_per_record,
    )
}

/// The creator's position in a new public pool, with the shares its first
/// liquidity minted
pub fn first_liquidity_shares_bytes(
    dex_id: &DexId,
    creator_id: &AccountId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    dex_storage_record_bytes(
        dex_id,
        sum_bytes(&[
            COLLECTION_PREFIX_BYTES,
            POOL_ID_BYTES,
            borsh_bytes(creator_id)?,
        ])?,
        LIQUIDITY_POSITION_BYTES,
        extra_bytes_per_record,
    )
}
