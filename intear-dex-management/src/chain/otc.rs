use intear_dex_types::{AssetId, DexId};
use near_cli_rs::config::NetworkConfig;
use near_primitives::types::{AccountId, BlockReference};
use near_sdk::PublicKey;
use near_sdk::json_types::U128;
use otc_dex_types::{
    GetAuthorizedKeyArgs, GetBalanceArgs, IsNonceUsedArgs, StorageBalance, StorageBalanceOfArgs,
};

use super::engine::dex_view;

/// The NEAR an account deposited on the OTC dex for its records, and how much
/// of it they use
pub fn storage_balance_of(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    account_id: &AccountId,
) -> color_eyre::eyre::Result<Option<StorageBalance>> {
    dex_view(
        network_config,
        block_reference,
        dex_id,
        "storage_balance_of",
        &StorageBalanceOfArgs {
            account_id: account_id.clone(),
        },
    )
}

/// The key whose signatures authorize the trade intents of an account
pub fn authorized_key(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    account_id: &AccountId,
) -> color_eyre::eyre::Result<Option<PublicKey>> {
    dex_view(
        network_config,
        block_reference,
        dex_id,
        "get_authorized_key",
        &GetAuthorizedKeyArgs {
            account_id: account_id.clone(),
        },
    )
}

/// An account's balance of an asset on the OTC dex: `None` where the asset
/// isn't registered for it
pub fn balance(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    account_id: &AccountId,
    asset_id: &AssetId,
) -> color_eyre::eyre::Result<Option<U128>> {
    dex_view(
        network_config,
        block_reference,
        dex_id,
        "get_balance",
        &GetBalanceArgs {
            account_id: account_id.clone(),
            asset_id: asset_id.clone(),
        },
    )
}

pub fn is_nonce_used(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    dex_id: &DexId,
    account_id: &AccountId,
    nonce: u128,
) -> color_eyre::eyre::Result<bool> {
    dex_view(
        network_config,
        block_reference,
        dex_id,
        "is_nonce_used",
        &IsNonceUsedArgs {
            nonce: U128(nonce),
            account_id: account_id.clone(),
        },
    )
}
