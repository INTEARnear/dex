use color_eyre::eyre::eyre;
use intear_dex_types::{AccountOrDexId, AssetId, DexId};
use near_sdk::NearToken;

// The engine's collections live under a one-byte prefix (a StorageKey
// variant). IterableMaps add one more byte for their key list and their
// value map, and keep values under a hash of the key. The balances of an
// account or a dex are an IterableMap under a prefix of their own: a
// StorageKey variant and a hash of the owner.
const COLLECTION_PREFIX_BYTES: usize = 1;
const ITERABLE_MAP_PART_PREFIX_BYTES: usize = 1;
const HASHED_KEY_BYTES: usize = 32;
const OWNER_BALANCES_PREFIX_BYTES: usize = COLLECTION_PREFIX_BYTES + 32;
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
    let part_prefix_bytes = BORSH_LENGTH_BYTES
        .checked_add(OWNER_BALANCES_PREFIX_BYTES)
        .and_then(|bytes| bytes.checked_add(ITERABLE_MAP_PART_PREFIX_BYTES))
        .ok_or_else(|| eyre!("Storage size overflow"))?;
    record_bytes(
        &[
            COLLECTION_PREFIX_BYTES,
            owner_bytes(owner)?,
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
    asset_id: &AssetId,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    sum(&[
        record_bytes(
            &[
                OWNER_BALANCES_PREFIX_BYTES,
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
    let mut bytes = vec![balance_record_bytes(asset_id, extra_bytes_per_record)?];
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
        // key list entry: (33 + 1 + 4) + (1 + 4 + 22) + 40, value: 32 + 16 + 4 + 40
        let bytes_for_owner_with_balances = asset_registration_bytes(
            &AccountOrDexId::Account(account_id.clone()),
            &usdt,
            true,
            true,
            40,
        )
        .unwrap();
        assert_eq!(bytes_for_owner_with_balances, 105 + 92);
        // where the owner's balances are: 1 + (4 + 10) + 4 + 2 * (4 + 33 + 1) + 40
        let bytes_for_new_owner =
            asset_registration_bytes(&AccountOrDexId::Account(account_id), &usdt, false, true, 40)
                .unwrap();
        assert_eq!(bytes_for_new_owner, 105 + 92 + 135);
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
