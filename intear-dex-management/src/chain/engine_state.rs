//! What the engine's views can't tell, read from its state: balances in the
//! layout from before its migration, and what storage balances add up to

use color_eyre::eyre::{Context, eyre};
use intear_dex_types::{AssetId, DexId};
use near_cli_rs::config::NetworkConfig;
use near_primitives::types::{AccountId, BlockReference};

use crate::deployment::ENGINE_ACCOUNT_ID;

/// Prefixes of the engine's collections in its state, in the order of its
/// `StorageKey`
const FLAT_DEX_BALANCES_PREFIX: u8 = 0;
const DEX_STORAGE_BALANCES_PREFIX: u8 = 3;
const FLAT_USER_BALANCES_PREFIX: u8 = 4;
const USER_STORAGE_BALANCES_PREFIX: u8 = 5;
/// IterableMaps keep their values under hashes, which can start with any
/// prefix
const HASHED_KEY_BYTES: usize = 32;
const BALANCE_BYTES: usize = 16;
/// The NEAR deposited and the part of it that pays for storage
const STORAGE_BALANCE_BYTES: usize = 32;

/// Balances in the layout from before the migration, one record per owner
/// and asset, in order
pub struct FlatBalances {
    pub users: Vec<(AccountId, AssetId)>,
    pub dexes: Vec<(DexId, AssetId)>,
}

impl FlatBalances {
    pub fn is_empty(&self) -> bool {
        self.users.is_empty() && self.dexes.is_empty()
    }
}

/// What the storage balances of accounts or of dexes add up to
#[derive(Clone, Copy, Default)]
pub struct StorageBalanceSum {
    pub total: u128,
    pub used: u128,
}

pub fn flat_balances(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<FlatBalances> {
    let mut users = Vec::new();
    for (key, value) in super::engine::state_records_with_prefix(
        network_config,
        block_reference,
        FLAT_USER_BALANCES_PREFIX,
    )? {
        if let Some(balance_key) = parse_record(&key, &value, BALANCE_BYTES)? {
            users.push(balance_key);
        }
    }
    let mut dexes = Vec::new();
    for (key, value) in super::engine::state_records_with_prefix(
        network_config,
        block_reference,
        FLAT_DEX_BALANCES_PREFIX,
    )? {
        if let Some(balance_key) = parse_record(&key, &value, BALANCE_BYTES)? {
            dexes.push(balance_key);
        }
    }
    users.sort();
    dexes.sort();
    Ok(FlatBalances { users, dexes })
}

pub fn user_storage_balance_sum(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<StorageBalanceSum> {
    storage_balance_sum::<AccountId>(
        network_config,
        block_reference,
        USER_STORAGE_BALANCES_PREFIX,
    )
}

pub fn dex_storage_balance_sum(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
) -> color_eyre::eyre::Result<StorageBalanceSum> {
    storage_balance_sum::<DexId>(network_config, block_reference, DEX_STORAGE_BALANCES_PREFIX)
}

fn storage_balance_sum<Owner: near_sdk::borsh::BorshDeserialize>(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    prefix: u8,
) -> color_eyre::eyre::Result<StorageBalanceSum> {
    let mut sum = StorageBalanceSum::default();
    for (key, value) in
        super::engine::state_records_with_prefix(network_config, block_reference, prefix)?
    {
        if parse_record::<Owner>(&key, &value, STORAGE_BALANCE_BYTES)?.is_none() {
            continue;
        }
        let (total, used): (u128, u128) = near_sdk::borsh::from_slice(&value)
            .wrap_err("Unexpected storage balance in the state of the engine")?;
        sum.total = sum
            .total
            .checked_add(total)
            .ok_or_else(|| eyre!("Storage balances overflow"))?;
        sum.used = sum
            .used
            .checked_add(used)
            .ok_or_else(|| eyre!("Storage balances overflow"))?;
    }
    Ok(sum)
}

/// The key of a record without its collection prefix, or `None` for a value
/// of an IterableMap, which is under a hash
fn parse_record<Key: near_sdk::borsh::BorshDeserialize>(
    key: &[u8],
    value: &[u8],
    value_bytes: usize,
) -> color_eyre::eyre::Result<Option<Key>> {
    let (_prefix, key_without_prefix) = key
        .split_first()
        .ok_or_else(|| eyre!("{ENGINE_ACCOUNT_ID} has an empty key in its state"))?;
    let parsed_key = near_sdk::borsh::from_slice::<Key>(key_without_prefix);
    match parsed_key {
        Ok(parsed_key) if value.len() == value_bytes => Ok(Some(parsed_key)),
        _ if key.len() == HASHED_KEY_BYTES => Ok(None),
        Ok(_) => Err(eyre!(
            "Unexpected value of {} in the state of {ENGINE_ACCOUNT_ID}",
            near_primitives::serialize::to_base64(key)
        )),
        Err(error) => Err(error).wrap_err_with(|| {
            format!(
                "Unexpected key {} in the state of {ENGINE_ACCOUNT_ID}",
                near_primitives::serialize::to_base64(key)
            )
        }),
    }
}
