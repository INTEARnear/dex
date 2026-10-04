use color_eyre::eyre::eyre;
use intear_dex_types::{AccountOrDexId, AssetId, DexId};
use near_sdk::NearToken;

// The engine's collections live under a one-byte prefix (a StorageKey
// variant); total_in_custody is an IterableMap, whose key list and value map
// add one more byte each
const COLLECTION_PREFIX_BYTES: usize = 1;
const ITERABLE_MAP_PART_PREFIX_BYTES: usize = 1;
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

/// What registering `asset_id` for `owner` adds to the engine's state; the
/// engine charges it to the account that pays for the registration
pub fn asset_registration_bytes(
    owner: &AccountOrDexId,
    asset_id: &AssetId,
    owner_has_registered_assets: bool,
    asset_is_in_custody: bool,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    let asset_bytes = near_sdk::borsh::to_vec(asset_id)?.len();
    let mut bytes = Vec::new();
    match owner {
        AccountOrDexId::Account(account_id) => {
            bytes.push(record_bytes(
                &[
                    COLLECTION_PREFIX_BYTES,
                    near_sdk::borsh::to_vec(&(account_id, asset_id))?.len(),
                    U128_BYTES,
                ],
                extra_bytes_per_record,
            )?);
            // user_registered_assets keeps a Vec<AssetId> per account
            bytes.push(if owner_has_registered_assets {
                u64::try_from(asset_bytes)?
            } else {
                record_bytes(
                    &[
                        COLLECTION_PREFIX_BYTES,
                        near_sdk::borsh::to_vec(account_id)?.len(),
                        U32_BYTES,
                        asset_bytes,
                    ],
                    extra_bytes_per_record,
                )?
            });
        }
        AccountOrDexId::Dex(dex_id) => bytes.push(record_bytes(
            &[
                COLLECTION_PREFIX_BYTES,
                near_sdk::borsh::to_vec(&(dex_id, asset_id))?.len(),
                U128_BYTES,
            ],
            extra_bytes_per_record,
        )?),
    }
    if !asset_is_in_custody {
        bytes.push(record_bytes(
            &[
                COLLECTION_PREFIX_BYTES,
                ITERABLE_MAP_PART_PREFIX_BYTES,
                U32_BYTES,
                asset_bytes,
            ],
            extra_bytes_per_record,
        )?);
        bytes.push(record_bytes(
            &[
                COLLECTION_PREFIX_BYTES,
                ITERABLE_MAP_PART_PREFIX_BYTES,
                asset_bytes,
                U128_BYTES,
                U32_BYTES,
            ],
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
    use near_primitives::types::AccountId;

    use super::*;

    #[test]
    fn registering_an_asset_counts_every_new_record() {
        let account_id: AccountId = "alice.near".parse().unwrap();
        let usdt: AssetId = "nep141:usdt.tether-token.near".parse().unwrap();
        // balance: 1 + (4 + 10) + (1 + 4 + 22) + 16 + 40, index entry grows by the asset
        let existing_index_bytes = asset_registration_bytes(
            &AccountOrDexId::Account(account_id.clone()),
            &usdt,
            true,
            true,
            40,
        )
        .unwrap();
        assert_eq!(existing_index_bytes, 98 + 27);
        // a new index entry: 1 + (4 + 10) + 4 + 27 + 40
        let new_index_bytes =
            asset_registration_bytes(&AccountOrDexId::Account(account_id), &usdt, false, true, 40)
                .unwrap();
        assert_eq!(new_index_bytes, 98 + 86);
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
