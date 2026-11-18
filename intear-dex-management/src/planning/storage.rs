use color_eyre::eyre::eyre;
use intear_dex_common::{AssetId, DexId};
use intear_dex_types::AccountOrDexId;
use near_primitives::types::AccountId;
use near_sdk::{NearToken, PublicKeyHandle};

use crate::deployment::ENGINE_ACCOUNT_ID;

// The engine's collections live under a one-byte prefix (a StorageKey
// variant). IterableMaps add one more byte for their key list and their
// value map, and keep values under a hash of the key. The balances of an
// account or a dex are an IterableMap under a prefix of their own: a
// StorageKey variant and the owner, so the prefix grows with the owner's id.
const COLLECTION_PREFIX_BYTES: usize = 1;
const ITERABLE_MAP_PART_PREFIX_BYTES: usize = 1;
const HASHED_KEY_BYTES: usize = 32;
const BORSH_LENGTH_BYTES: usize = 4;
const U32_BYTES: usize = 4;
const U128_BYTES: usize = 16;

/// A key-value record of `parts` (key and value bytes), as the protocol counts
/// its storage
fn record_bytes(parts: &[usize], extra_bytes_per_record: u64) -> color_eyre::eyre::Result<u64> {
    let key_and_value_bytes = parts
        .iter()
        .try_fold(0usize, |sum, part| sum.checked_add(*part))
        .ok_or_else(|| eyre!("Storage size overflow"))?;
    u64::try_from(key_and_value_bytes)?
        .checked_add(extra_bytes_per_record)
        .ok_or_else(|| eyre!("Storage size overflow"))
}

fn sum(bytes: &[u64]) -> color_eyre::eyre::Result<u64> {
    bytes
        .iter()
        .try_fold(0u64, |sum, part| sum.checked_add(*part))
        .ok_or_else(|| eyre!("Storage size overflow"))
}

fn owner_bytes(owner: &AccountOrDexId) -> color_eyre::eyre::Result<usize> {
    Ok(match owner {
        AccountOrDexId::Account(account_id) => near_sdk::borsh::to_vec(account_id)?.len(),
        AccountOrDexId::Dex(dex_id) => near_sdk::borsh::to_vec(dex_id)?.len(),
    })
}

/// The record that the first balance of an account or a dex creates: where
/// its balances are, as the length and prefix of their key list and the
/// prefix of their value map
pub fn owner_balances_record_bytes(
    owner: &AccountOrDexId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    let owner_bytes = owner_bytes(owner)?;
    let part_prefix_bytes = BORSH_LENGTH_BYTES
        .checked_add(COLLECTION_PREFIX_BYTES)
        .and_then(|bytes| bytes.checked_add(owner_bytes))
        .and_then(|bytes| bytes.checked_add(ITERABLE_MAP_PART_PREFIX_BYTES))
        .ok_or_else(|| eyre!("Storage size overflow"))?;
    record_bytes(
        &[
            COLLECTION_PREFIX_BYTES,
            owner_bytes,
            U32_BYTES,
            part_prefix_bytes,
            part_prefix_bytes,
        ],
        extra_bytes_per_record,
    )
}

/// The records of one balance among the balances of its owner: the asset in
/// their key list, and the balance with its index in the list under a hash
pub fn balance_record_bytes(
    owner: &AccountOrDexId,
    asset_id: &AssetId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    sum(&[
        record_bytes(
            &[
                COLLECTION_PREFIX_BYTES,
                owner_bytes(owner)?,
                ITERABLE_MAP_PART_PREFIX_BYTES,
                U32_BYTES,
                near_sdk::borsh::to_vec(asset_id)?.len(),
            ],
            extra_bytes_per_record,
        )?,
        record_bytes(
            &[HASHED_KEY_BYTES, U128_BYTES, U32_BYTES],
            extra_bytes_per_record,
        )?,
    ])
}

/// What registering `asset_id` for `owner` adds to the engine's state; the
/// engine charges it to the account that pays for the registration
pub fn asset_registration_bytes(
    owner: &AccountOrDexId,
    asset_id: &AssetId,
    owner_has_registered_assets: bool,
    asset_is_in_custody: bool,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    let mut bytes = vec![balance_record_bytes(
        owner,
        asset_id,
        extra_bytes_per_record,
    )?];
    if !owner_has_registered_assets {
        bytes.push(owner_balances_record_bytes(owner, extra_bytes_per_record)?);
    }
    if !asset_is_in_custody {
        bytes.push(record_bytes(
            &[
                COLLECTION_PREFIX_BYTES,
                ITERABLE_MAP_PART_PREFIX_BYTES,
                U32_BYTES,
                near_sdk::borsh::to_vec(asset_id)?.len(),
            ],
            extra_bytes_per_record,
        )?);
        bytes.push(record_bytes(
            &[HASHED_KEY_BYTES, U128_BYTES, U32_BYTES],
            extra_bytes_per_record,
        )?);
    }
    sum(&bytes)
}

/// The record a first storage deposit for an account or a dex creates, paid
/// from the deposit itself
pub fn storage_balance_record_bytes(
    owner: &impl near_sdk::borsh::BorshSerialize,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    record_bytes(
        &[
            COLLECTION_PREFIX_BYTES,
            near_sdk::borsh::to_vec(owner)?.len(),
            U128_BYTES,
            U128_BYTES,
        ],
        extra_bytes_per_record,
    )
}

/// One entry of a dex's own storage: the engine keeps all of them in one
/// collection, keyed by the dex id and the dex's key as a byte vector, with
/// the dex's value as a byte vector
pub fn dex_storage_record_bytes(
    dex_id: &DexId,
    dex_key_bytes: usize,
    dex_value_bytes: usize,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    record_bytes(
        &[
            COLLECTION_PREFIX_BYTES,
            near_sdk::borsh::to_vec(dex_id)?.len(),
            U32_BYTES,
            dex_key_bytes,
            U32_BYTES,
            dex_value_bytes,
        ],
        extra_bytes_per_record,
    )
}

/// The engine's record of a dex's code
pub fn dex_code_record_bytes(
    dex_id: &DexId,
    code_bytes: usize,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    record_bytes(
        &[
            COLLECTION_PREFIX_BYTES,
            near_sdk::borsh::to_vec(dex_id)?.len(),
            U32_BYTES,
            code_bytes,
        ],
        extra_bytes_per_record,
    )
}

/// The engine's record of the translation of a dex's code, which it stores as the
/// translator returned it
pub fn dex_code_translation_record_bytes(
    dex_id: &DexId,
    translation_bytes: usize,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    record_bytes(
        &[
            COLLECTION_PREFIX_BYTES,
            near_sdk::borsh::to_vec(dex_id)?.len(),
            translation_bytes,
        ],
        extra_bytes_per_record,
    )
}

/// What `create_oracle_key` takes from the attached NEAR: the gas key of the
/// oracle with its nonces, as the protocol stores them in the engine's
/// account, and the record of its fee receiver
pub fn oracle_key_bytes(
    oracle_key: &PublicKeyHandle,
    fee_receiver_id: &AccountId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    const ORACLE_KEY_METHODS: [&str; 2] = ["oracle_respond", "remove_oracle_key"];
    const ORACLE_KEY_NONCES: usize = 4;
    // https://github.com/near/nearcore/blob/2.14.0/core/primitives-core/src/types.rs#L19-L21
    const GAS_KEY_NONCE_BYTES: usize = 8 + 2;
    let key_bytes = near_sdk::borsh::to_vec(oracle_key)?.len();
    // Borsh of the access key: its nonce, the tag of its permission, the gas key's
    // balance and number of nonces, unlimited allowance, receiver and methods
    let access_key_bytes = near_sdk::borsh::to_vec(&(
        0u64,
        0u8,
        0u128,
        0u16,
        None::<u128>,
        ENGINE_ACCOUNT_ID.as_str(),
        ORACLE_KEY_METHODS.to_vec(),
    ))?
    .len();
    let mut bytes = vec![
        record_bytes(&[key_bytes, access_key_bytes], extra_bytes_per_record)?,
        oracle_fee_receiver_record_bytes(oracle_key, fee_receiver_id, extra_bytes_per_record)?,
    ];
    for _ in 0..ORACLE_KEY_NONCES {
        bytes.push(record_bytes(
            &[key_bytes, GAS_KEY_NONCE_BYTES],
            extra_bytes_per_record,
        )?);
    }
    sum(&bytes)
}

/// The engine's record of the account that takes the fees of an oracle
pub fn oracle_fee_receiver_record_bytes(
    oracle_key: &PublicKeyHandle,
    fee_receiver_id: &AccountId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    record_bytes(
        &[
            COLLECTION_PREFIX_BYTES,
            near_sdk::borsh::to_vec(oracle_key)?.len(),
            near_sdk::borsh::to_vec(fee_receiver_id)?.len(),
        ],
        extra_bytes_per_record,
    )
}

/// The engine's record of what an oracle takes for a request in each of
/// `fee_asset_ids`, which it charges to the oracle's fee receiver
pub fn oracle_fees_record_bytes<'a>(
    oracle_key: &PublicKeyHandle,
    fee_asset_ids: impl IntoIterator<Item = &'a AssetId>,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    let mut parts = vec![
        COLLECTION_PREFIX_BYTES,
        near_sdk::borsh::to_vec(oracle_key)?.len(),
        BORSH_LENGTH_BYTES,
    ];
    for asset_id in fee_asset_ids {
        parts.push(near_sdk::borsh::to_vec(asset_id)?.len());
        parts.push(U128_BYTES);
    }
    record_bytes(&parts, extra_bytes_per_record)
}

pub fn storage_cost(
    bytes: u64,
    storage_byte_cost: NearToken,
) -> color_eyre::eyre::Result<NearToken> {
    storage_byte_cost
        .checked_mul(u128::from(bytes))
        .ok_or_else(|| eyre!("Storage cost overflow"))
}

/// What to deposit so that `available` covers `needed`: nothing when it
/// already does, else the shortfall, but at least the engine's minimum
pub fn storage_deposit_for(
    needed: NearToken,
    available: NearToken,
    minimum_deposit: NearToken,
) -> Option<NearToken> {
    needed
        .checked_sub(available)
        .filter(|shortfall| !shortfall.is_zero())
        .map(|shortfall| shortfall.max(minimum_deposit))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use intear_dex_common::TokenAmount;

    use super::*;

    const ED25519_ORACLE_KEY: &str = "ed25519:6E8sCci9badyRkXb3JoRpBj5p8C6Tw41ELDZoiihKEtp";

    #[test]
    fn oracle_key_counts_the_gas_key_its_nonces_and_the_fee_receiver() {
        let oracle_key: PublicKeyHandle = ED25519_ORACLE_KEY.parse().unwrap();
        let fee_receiver_id: AccountId = "oracle.intear.near".parse().unwrap();
        // access key: 33 + (8 + 1 + 16 + 2 + 1 + (4 + 15) + (4 + (4 + 14) + (4 + 17))) + 40,
        // each of the 4 nonces: 33 + (8 + 2) + 40, fee receiver: 1 + 33 + (4 + 18) + 40
        assert_eq!(
            oracle_key_bytes(&oracle_key, &fee_receiver_id, 40).unwrap(),
            163 + 4 * 83 + 96
        );
        // a secp256k1 key is 32 bytes longer in each of its 6 records
        assert_eq!(
            oracle_key_bytes(&PublicKeyHandle::SECP256K1([1; 64]), &fee_receiver_id, 40).unwrap(),
            163 + 4 * 83 + 96 + 6 * 32
        );
    }

    #[test]
    fn oracle_fee_receiver_counts_its_key_and_every_byte_of_the_account() {
        let oracle_key: PublicKeyHandle = ED25519_ORACLE_KEY.parse().unwrap();
        let fee_receiver_id: AccountId = "oracle.intear.near".parse().unwrap();
        // The engine's StorageKey variant of the fee receivers, and the oracle key
        let key = near_sdk::borsh::to_vec(&(13u8, &oracle_key)).unwrap();
        assert_eq!(
            oracle_fee_receiver_record_bytes(&oracle_key, &fee_receiver_id, 40).unwrap(),
            u64::try_from(key.len() + near_sdk::borsh::to_vec(&fee_receiver_id).unwrap().len())
                .unwrap()
                + 40
        );
    }

    #[test]
    fn oracle_fees_count_their_key_and_every_asset_with_its_fee() {
        let oracle_key: PublicKeyHandle = ED25519_ORACLE_KEY.parse().unwrap();
        let usdt: AssetId = "nep141:usdt.tether-token.near".parse().unwrap();
        // The engine's StorageKey variant of the fees, and the oracle key
        let key = near_sdk::borsh::to_vec(&(14u8, &oracle_key)).unwrap();
        let fees = HashMap::from([
            (AssetId::Near, TokenAmount::new(1)),
            (usdt.clone(), TokenAmount::new(5000)),
        ]);
        assert_eq!(
            oracle_fees_record_bytes(&oracle_key, [&AssetId::Near, &usdt], 40).unwrap(),
            u64::try_from(key.len() + near_sdk::borsh::to_vec(&fees).unwrap().len()).unwrap() + 40
        );
        // without fees, the key and the length of an empty map: 1 + 33 + 4 + 40
        assert_eq!(oracle_fees_record_bytes(&oracle_key, [], 40).unwrap(), 78);
    }

    #[test]
    fn registering_an_asset_counts_every_new_record() {
        let account_id: AccountId = "alice.near".parse().unwrap();
        let usdt: AssetId = "nep141:usdt.tether-token.near".parse().unwrap();
        // key list entry: (1 + (4 + 10) + 1 + 4) + (1 + 4 + 22) + 40, value: 32 + 16 + 4 + 40
        let bytes_for_owner_with_balances = asset_registration_bytes(
            &AccountOrDexId::Account(account_id.clone()),
            &usdt,
            true,
            true,
            40,
        )
        .unwrap();
        assert_eq!(bytes_for_owner_with_balances, 87 + 92);
        // where the owner's balances are: 1 + (4 + 10) + 4 + 2 * (4 + 1 + (4 + 10) + 1) + 40
        let bytes_for_new_owner =
            asset_registration_bytes(&AccountOrDexId::Account(account_id), &usdt, false, true, 40)
                .unwrap();
        assert_eq!(bytes_for_new_owner, 87 + 92 + 99);
        // the owner is part of every key, so an implicit account takes more:
        // (1 + (4 + 64) + 1 + 4) + (1 + 4 + 22) + 40, 32 + 16 + 4 + 40,
        // and 1 + (4 + 64) + 4 + 2 * (4 + 1 + (4 + 64) + 1) + 40
        let implicit_account_id: AccountId = "a".repeat(64).parse().unwrap();
        let bytes_for_new_implicit_owner = asset_registration_bytes(
            &AccountOrDexId::Account(implicit_account_id),
            &usdt,
            false,
            true,
            40,
        )
        .unwrap();
        assert_eq!(bytes_for_new_implicit_owner, 141 + 92 + 261);
    }

    #[test]
    fn deposits_cover_the_shortfall_but_at_least_the_minimum() {
        let minimum = NearToken::from_millinear(5);
        assert_eq!(
            storage_deposit_for(
                NearToken::from_millinear(1),
                NearToken::from_millinear(2),
                minimum
            ),
            None
        );
        assert_eq!(
            storage_deposit_for(
                NearToken::from_millinear(3),
                NearToken::from_millinear(2),
                minimum
            ),
            Some(minimum)
        );
        assert_eq!(
            storage_deposit_for(
                NearToken::from_millinear(20),
                NearToken::from_millinear(2),
                minimum
            ),
            Some(NearToken::from_millinear(18))
        );
    }
}
