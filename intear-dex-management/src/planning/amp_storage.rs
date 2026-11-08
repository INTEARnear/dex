//! What the amp contract's records take in the dex's storage. Its fee
//! balances are stored as xyk's, so [`super::xyk_storage`] counts them.

use intear_dex_common::{AssetId, DexId};
use near_primitives::types::AccountId;
use near_sdk::PublicKeyHandle;

use super::storage::dex_storage_record_bytes;
use super::xyk_storage::{
    COLLECTION_PREFIX_BYTES, ENUM_TAG_BYTES, FEE_GROWTH_BYTES, MAP_BYTES, POOL_ID_BYTES,
    POOL_KEY_BYTES, SHARE_MAP_BYTES, STATE_KEY_BYTES, U32_BYTES, U128_BYTES, VECTOR_BYTES,
    borsh_bytes, sum_bytes,
};

// The decimals of both assets
const DECIMALS_BYTES: usize = 2;
const AMPLIFICATION_BYTES: usize = U32_BYTES;
const FEE_BYTES: usize = U32_BYTES;
const PRICE_BYTES: usize = U128_BYTES;
// The shares and fee growth of each of the three payout groups
const PAYOUT_GROUPS_BYTES: usize = (U128_BYTES + FEE_GROWTH_BYTES) * 3;
// A provider's payout, shares, and the fee growth it last collected at
const LIQUIDITY_POSITION_BYTES: usize = ENUM_TAG_BYTES + U128_BYTES + FEE_GROWTH_BYTES;

/// The state that initializing an amp dex stores: the pool vector, the map
/// of collected fees, and no fee discount signer
pub fn initial_state_bytes(
    dex_id: &DexId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    dex_storage_record_bytes(
        dex_id,
        STATE_KEY_BYTES,
        sum_bytes(&[VECTOR_BYTES, MAP_BYTES, ENUM_TAG_BYTES])?,
        extra_bytes_per_record,
    )
}

/// A pool as a new element of the pool list, with the shares that the
/// creator's first liquidity minted
pub fn new_pool_bytes(
    dex_id: &DexId,
    assets: (&AssetId, &AssetId),
    oracle_key: &PublicKeyHandle,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    let asset_with_balance_bytes =
        |asset_id: &AssetId| sum_bytes(&[borsh_bytes(asset_id)?, U128_BYTES]);
    let pool_bytes = sum_bytes(&[
        asset_with_balance_bytes(assets.0)?,
        asset_with_balance_bytes(assets.1)?,
        DECIMALS_BYTES,
        AMPLIFICATION_BYTES,
        borsh_bytes(oracle_key)?,
        // The last oracle price, which takes its full size before the first swap
        PRICE_BYTES,
        FEE_BYTES,
        SHARE_MAP_BYTES,
        ENUM_TAG_BYTES,
        U128_BYTES,
        PAYOUT_GROUPS_BYTES,
    ])?;
    dex_storage_record_bytes(dex_id, POOL_KEY_BYTES, pool_bytes, extra_bytes_per_record)
}

/// An account's position in a pool, which registering for liquidity starts,
/// and creating a pool starts for its creator
pub fn liquidity_position_bytes(
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

/// What a pool's record grows by when its oracle changes to one with a
/// longer key; a shorter key frees storage instead
pub fn oracle_key_growth_bytes(
    current_oracle_key: &PublicKeyHandle,
    new_oracle_key: &PublicKeyHandle,
) -> color_eyre::eyre::Result<u64> {
    let growth = borsh_bytes(new_oracle_key)?.saturating_sub(borsh_bytes(current_oracle_key)?);
    Ok(u64::try_from(growth)?)
}

#[cfg(test)]
mod tests {
    use amp_dex_types::{
        Amplification, AssetWithBalance, FeeFraction, FeePayoutType, INITIAL_SHARES,
        LiquidityPosition, PayoutGroup, PayoutGroups, PoolId, Price, TokenAmount, TotalShares,
    };

    use super::*;

    #[test]
    fn new_pool_counts_every_byte_of_the_pool() {
        let dex_id: DexId = "slimedragon.near/amp".parse().unwrap();
        let assets: (AssetId, AssetId) = (
            AssetId::Near,
            "nep141:17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1"
                .parse()
                .unwrap(),
        );
        let oracle_key: PublicKeyHandle = "ed25519:6E8sCci9badyRkXb3JoRpBj5p8C6Tw41ELDZoiihKEtp"
            .parse()
            .unwrap();
        // The fields of the contract's Pool in order, with its providers' map
        // stored as its prefix: the variant after its pools, and the pool id
        let providers_prefix = near_sdk::borsh::to_vec(&(1u8, PoolId::new(0))).unwrap();
        let pool_fields = (
            (
                AssetWithBalance {
                    asset_id: assets.0.clone(),
                    balance: TokenAmount::new(1),
                },
                AssetWithBalance {
                    asset_id: assets.1.clone(),
                    balance: TokenAmount::new(1),
                },
            ),
            (24u8, 6u8),
            Amplification::new(100),
            oracle_key.clone(),
            Price::from_units(0),
            FeeFraction::new(500),
            providers_prefix,
            TotalShares::NONE.minted(INITIAL_SHARES).unwrap(),
            PayoutGroups::default(),
        );
        assert_eq!(
            new_pool_bytes(&dex_id, (&assets.0, &assets.1), &oracle_key, 40).unwrap(),
            dex_storage_record_bytes(
                &dex_id,
                POOL_KEY_BYTES,
                near_sdk::borsh::to_vec(&pool_fields).unwrap().len(),
                40
            )
            .unwrap()
        );
    }

    #[test]
    fn liquidity_position_counts_its_key_and_every_byte_of_the_position() {
        let dex_id: DexId = "slimedragon.near/amp".parse().unwrap();
        let account_id: AccountId = "alice.near".parse().unwrap();
        // The providers' map of pool 0, as the variant after its pools, and the account
        let key = near_sdk::borsh::to_vec(&(1u8, PoolId::new(0), account_id.as_str())).unwrap();
        let position = LiquidityPosition::new(
            FeePayoutType::KeepBoth,
            INITIAL_SHARES,
            &PayoutGroup::default(),
        );
        assert_eq!(
            liquidity_position_bytes(&dex_id, &account_id, 40).unwrap(),
            dex_storage_record_bytes(
                &dex_id,
                key.len(),
                near_sdk::borsh::to_vec(&position).unwrap().len(),
                40
            )
            .unwrap()
        );
    }

    #[test]
    fn longer_oracle_keys_take_more_storage() {
        let ed25519: PublicKeyHandle = "ed25519:6E8sCci9badyRkXb3JoRpBj5p8C6Tw41ELDZoiihKEtp"
            .parse()
            .unwrap();
        let secp256k1 = PublicKeyHandle::SECP256K1([1; 64]);
        assert_eq!(oracle_key_growth_bytes(&ed25519, &secp256k1).unwrap(), 32);
        assert_eq!(oracle_key_growth_bytes(&secp256k1, &ed25519).unwrap(), 0);
    }
}
