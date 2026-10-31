use color_eyre::eyre::{bail, eyre};
use intear_dex_types::{AssetId, DexId};
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::NearToken;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};
use tracing_indicatif::span_ext::IndicatifSpanExt;
use xyk_dex_types::{
    AssetIndex, ClaimMode, FeeConfigurationV2, FeeReceiver, PoolAssets, PoolId, PoolView,
};

use crate::chain::asset_metadata::{AssetMetadata, AssetMetadataCache};
use crate::deployment::{DEFAULT_XYK_DEX_DEPLOYER, DEFAULT_XYK_DEX_NAME};
use crate::errors::PreflightError;
use crate::inputs::amount::AmountArg;
use crate::inputs::dex_id::DexIdArg;
use crate::inputs::pool_id::PoolIdArg;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, Preflight, format_near, xyk_storage};

pub mod community_fees;
pub mod create_pool;
pub mod deploy;
pub mod fee_discounts;
pub mod fees;
pub mod init;
pub mod liquidity;
pub mod pool;
pub mod pools;
pub mod presentation;

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = XykContext)]
pub struct XykCommands {
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// The XYK dex as <deployer>/<name> (default: slimedragon.near/xyk)
    dex: Option<DexIdArg>,
    #[interactive_clap(subcommand)]
    action: XykAction,
}

#[derive(Clone)]
pub struct XykContext {
    pub global_context: crate::GlobalContext,
    pub dex_id: DexId,
    /// Whether --dex chose the dex, which deploy doesn't take
    pub is_dex_chosen: bool,
}

impl XykContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<XykCommands as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = match &scope.dex {
            Some(DexIdArg(dex_id)) => dex_id.clone(),
            None => DexId {
                deployer: DEFAULT_XYK_DEX_DEPLOYER.to_owned(),
                id: DEFAULT_XYK_DEX_NAME.to_string(),
            },
        };
        Ok(Self {
            global_context: previous_context,
            dex_id,
            is_dex_chosen: scope.dex.is_some(),
        })
    }
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do?
pub enum XykAction {
    #[strum_discriminants(strum(message = "pools           - List or count pools"))]
    /// List or count pools
    Pools(self::pools::PoolsCommands),
    #[strum_discriminants(strum(
        message = "pool            - One pool in detail, or change it: lock, edit-fees"
    ))]
    /// One pool in detail, or change it: lock, edit-fees
    Pool(self::pool::PoolCommands),
    #[strum_discriminants(strum(message = "liquidity       - Show, add or remove liquidity"))]
    /// Show, add or remove liquidity
    Liquidity(self::liquidity::LiquidityCommands),
    #[strum_discriminants(strum(
        message = "fees            - Fees an account collected: pending or withdraw"
    ))]
    /// Fees an account collected: pending or withdraw
    Fees(self::fees::FeesCommands),
    #[strum_discriminants(strum(
        message = "community-fees  - Fees that anyone can claim for community accounts"
    ))]
    /// Fees that anyone can claim for community accounts
    CommunityFees(self::community_fees::CommunityFeesCommands),
    #[strum_discriminants(strum(
        message = "fee-discounts   - Who signs fee discounts: show, set-signer, disable"
    ))]
    /// Who signs fee discounts: show, set-signer, disable
    FeeDiscounts(self::fee_discounts::FeeDiscountsCommands),
    #[strum_discriminants(strum(
        message = "create-pool     - Create a private, public or launch pool"
    ))]
    /// Create a private, public or launch pool
    CreatePool(self::create_pool::CreatePool),
    #[strum_discriminants(strum(
        message = "deploy          - Deploy xyk code as <signer>/<name>; --migrate converts the state too"
    ))]
    /// Deploy xyk code as <signer>/<name>; --migrate converts the state too
    Deploy(self::deploy::Deploy),
    #[strum_discriminants(strum(message = "init            - Initialize a newly deployed dex"))]
    /// Initialize a newly deployed dex
    Init(self::init::Init),
}

struct PoolChoice {
    pool_id: PoolId,
    label: String,
}

impl std::fmt::Display for PoolChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[tracing::instrument(name = "Getting the pools of", skip_all)]
fn pool_choices(context: &XykContext) -> color_eyre::eyre::Result<Vec<PoolChoice>> {
    tracing::Span::current().pb_set_message(&context.dex_id.to_string());
    let pools_by_network = crate::chain::networks::read_on_every_network(
        &context.global_context.config,
        |network_config, block_reference| {
            let block_reference =
                crate::chain::engine::engine_view_block(network_config, block_reference)?;
            let metadata_cache = AssetMetadataCache::new(network_config, block_reference.clone());
            let mut choices = Vec::new();
            for (pool_id, pool) in
                crate::chain::xyk::all_pools(network_config, &block_reference, &context.dex_id)?
            {
                choices.push(PoolChoice {
                    pool_id,
                    label: presentation::pool_label(pool_id, &pool, &metadata_cache)?,
                });
            }
            Ok(choices)
        },
    )?;
    let is_single_network = pools_by_network.len() == 1;
    Ok(pools_by_network
        .into_iter()
        .flat_map(|(network_name, choices)| {
            choices.into_iter().map(move |choice| PoolChoice {
                pool_id: choice.pool_id,
                label: if is_single_network {
                    choice.label
                } else {
                    format!("[{network_name}] {}", choice.label)
                },
            })
        })
        .collect())
}

/// A pool that a write changes, with the metadata of its two assets
pub struct PoolToWrite {
    pub pool: PoolView,
    pub assets: [(AssetId, Option<AssetMetadata>); 2],
}

impl PoolToWrite {
    pub fn read(
        preflight: &Preflight,
        dex_id: &DexId,
        pool_id: PoolId,
    ) -> color_eyre::eyre::Result<Self> {
        let pool = crate::chain::xyk::pool(
            &preflight.network_config,
            &preflight.block_reference,
            dex_id,
            pool_id,
        )?;
        let metadata_cache =
            AssetMetadataCache::new(&preflight.network_config, preflight.block_reference.clone());
        let [(asset_id_0, _), (asset_id_1, _)] = presentation::pool_assets(&pool);
        let metadata_0 = metadata_cache.get(&asset_id_0)?;
        let metadata_1 = metadata_cache.get(&asset_id_1)?;
        Ok(Self {
            pool,
            assets: [(asset_id_0, metadata_0), (asset_id_1, metadata_1)],
        })
    }

    pub fn pair_label(&self) -> String {
        let [(asset_id_0, metadata_0), (asset_id_1, metadata_1)] = &self.assets;
        format!(
            "{} / {}",
            crate::display::asset_label(asset_id_0, metadata_0.as_ref()),
            crate::display::asset_label(asset_id_1, metadata_1.as_ref())
        )
    }

    pub fn format_amount(&self, side: usize, raw_amount: u128) -> String {
        let (_, metadata) = &self.assets[side];
        crate::display::format_amount(raw_amount, metadata.as_ref())
    }
}

/// The signer's amounts of the two assets of a pool, in pool order
pub fn amounts_in_pool_order(
    assets: &[(AssetId, Option<AssetMetadata>); 2],
    pair_label: &str,
    amounts: &(AmountArg, AmountArg),
) -> color_eyre::eyre::Result<(u128, u128)> {
    let (first_asset_id, first_amount) = amounts.0.resolve(assets)?;
    let (second_asset_id, second_amount) = amounts.1.resolve(assets)?;
    if first_asset_id == second_asset_id {
        return Err(PreflightError::AmountsOfOneAsset {
            pair: pair_label.to_string(),
        }
        .into_report());
    }
    let amounts = if first_asset_id == assets[0].0 {
        (first_amount, second_amount)
    } else {
        (second_amount, first_amount)
    };
    if amounts.0 == 0 || amounts.1 == 0 {
        return Err(PreflightError::ZeroAmount.into_report());
    }
    Ok(amounts)
}

/// Every asset that some pool of the dex holds, once each, in pool order
pub fn all_pool_asset_ids(
    network_config: &near_cli_rs::config::NetworkConfig,
    block_reference: &near_primitives::types::BlockReference,
    dex_id: &DexId,
) -> color_eyre::eyre::Result<Vec<AssetId>> {
    let mut asset_ids: Vec<AssetId> = Vec::new();
    for (_, pool) in crate::chain::xyk::all_pools(network_config, block_reference, dex_id)? {
        for (asset_id, _) in presentation::pool_assets(&pool) {
            if !asset_ids.contains(&asset_id) {
                asset_ids.push(asset_id);
            }
        }
    }
    Ok(asset_ids)
}

/// Who can move the fees of an account's balances out of the dex
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FeeBalanceKind {
    /// Fees that only the account withdraws
    CollectedByOwner,
    /// Fees that anyone can claim for the account
    ClaimableByAnyone,
}

/// The fee balances that an account starts in some assets
pub struct FeeBalances {
    pub kind: FeeBalanceKind,
    pub account_id: AccountId,
    pub asset_ids: Vec<AssetId>,
}

/// The account receivers of `fees`, each with the assets of the pool that
/// its payout takes fees in, which the pool starts fee balances in
pub fn receiver_fee_balances(
    fees: &FeeConfigurationV2,
    now_nanoseconds: u64,
    pool_asset_ids: PoolAssets<&AssetId>,
) -> Vec<FeeBalances> {
    let mut fee_balances: Vec<FeeBalances> = Vec::new();
    for (receiver, _) in fees.receivers_at(now_nanoseconds) {
        let (kind, account_id, payout_type) = match receiver {
            FeeReceiver::Account {
                account_id,
                payout_type,
                claim_mode: ClaimMode::OnlyOwner,
            } => (FeeBalanceKind::CollectedByOwner, account_id, payout_type),
            FeeReceiver::Account {
                account_id,
                payout_type,
                claim_mode: ClaimMode::Anyone,
            } => (FeeBalanceKind::ClaimableByAnyone, account_id, payout_type),
            // The protocol starts fee balances in both assets of every pool
            FeeReceiver::Pool | FeeReceiver::Protocol => continue,
        };
        if fee_balances.iter().any(|listed_fee_balances| {
            listed_fee_balances.kind == kind && listed_fee_balances.account_id == account_id
        }) {
            continue;
        }
        let mut asset_ids: Vec<AssetId> = Vec::new();
        for fee_asset_index in AssetIndex::BOTH {
            let asset_id = pool_asset_ids[payout_type.asset_index(fee_asset_index)];
            if !asset_ids.contains(asset_id) {
                asset_ids.push(asset_id.clone());
            }
        }
        fee_balances.push(FeeBalances {
            kind,
            account_id,
            asset_ids,
        });
    }
    fee_balances
}

/// What starting `fee_balances` stores for the ones that don't exist yet
pub fn new_fee_balances_bytes(
    preflight: &Preflight,
    dex_id: &DexId,
    fee_balances: &[FeeBalances],
) -> color_eyre::eyre::Result<u64> {
    let mut total_bytes = 0u64;
    for FeeBalances {
        kind,
        account_id,
        asset_ids,
    } in fee_balances
    {
        let existing_fee_balances = match kind {
            FeeBalanceKind::CollectedByOwner => crate::chain::xyk::pending_fees(
                &preflight.network_config,
                &preflight.block_reference,
                dex_id,
                account_id,
                asset_ids.clone(),
            )?,
            FeeBalanceKind::ClaimableByAnyone => crate::chain::xyk::claimable_fees(
                &preflight.network_config,
                &preflight.block_reference,
                dex_id,
                account_id,
                asset_ids.clone(),
            )?,
        };
        for asset_id in asset_ids {
            if !existing_fee_balances.contains_key(asset_id) {
                total_bytes = total_bytes
                    .checked_add(xyk_storage::fee_balance_bytes(
                        dex_id,
                        account_id,
                        asset_id,
                        preflight.protocol_limits.extra_bytes_per_record,
                    )?)
                    .ok_or_else(|| eyre!("Storage size overflow"))?;
            }
        }
    }
    Ok(total_bytes)
}

/// The steps and calls of a dex method that stores records and takes
/// `storage_near` for them. The NEAR goes with the transaction into the
/// signer's balance on the engine, and from there to the dex, which sends
/// back what it doesn't use.
pub fn dex_call_paying_for_storage(
    preflight: &Preflight,
    dex_id: &DexId,
    action_step: String,
    (method, args): (&str, &impl near_sdk::borsh::BorshSerialize),
    storage_near: NearToken,
    gas: Gas,
) -> color_eyre::eyre::Result<(Vec<String>, Vec<FunctionCall>)> {
    if storage_near.is_zero() {
        return Ok((
            vec![action_step],
            vec![FunctionCall {
                method_name: "execute_operations",
                args: serde_json::json!({
                    "operations": [planning::dex_call_operation(dex_id, method, args, &[])?],
                }),
                deposit: ONE_YOCTO_NEAR,
                gas,
            }],
        ));
    }
    let registration_calls = preflight.registration_calls(
        &planning::storage_near_registrations(&preflight.signer_id, dex_id),
    )?;
    let mut steps = registration_calls.steps;
    let mut function_calls = registration_calls.function_calls;
    steps.push(action_step);
    steps.push(format!(
        "Attach {} from {}'s wallet for the storage this takes; {dex_id} sends back what it doesn't use",
        format_near(storage_near),
        preflight.signer_id
    ));
    function_calls.push(FunctionCall {
        method_name: "execute_operations",
        args: serde_json::json!({
            "operations": [planning::dex_call_operation(
                dex_id,
                method,
                args,
                &[(AssetId::Near, storage_near.as_yoctonear())],
            )?],
        }),
        deposit: storage_near,
        gas,
    });
    Ok((steps, function_calls))
}

pub fn input_pool_id(context: &XykContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
    let choices = pool_choices(context)?;
    if choices.is_empty() {
        bail!("{} has no pools", context.dex_id);
    }
    match inquire::Select::new("Which pool? (type to filter)", choices)
        .with_page_size(15)
        .prompt()
    {
        Ok(choice) => Ok(Some(PoolIdArg(choice.pool_id))),
        Err(
            inquire::error::InquireError::OperationCanceled
            | inquire::error::InquireError::OperationInterrupted,
        ) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
