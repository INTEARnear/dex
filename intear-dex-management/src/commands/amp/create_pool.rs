use amp_dex_types::{
    Amplification, CreatePoolArgs, FeePayoutType, PROTOCOL_FEE_RECEIVER_ID, PROTOCOL_FEE_SHARE,
    TokenAmount,
};
use intear_dex_common::{AssetId, DexId};
use intear_dex_types::AccountOrDexId;
use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::PublicKeyHandle;
use serde_json::json;

use super::AmpContext;
use crate::chain::asset_metadata::AssetMetadataCache;
use crate::commands::xyk::create_pool::ensure_poolable;
use crate::commands::xyk::{
    FeeBalanceKind, FeeBalances, amounts_in_pool_order, new_fee_balances_bytes,
};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display;
use crate::errors::PreflightError;
use crate::inputs::amount::AmountArg;
use crate::inputs::amplification::AmplificationArg;
use crate::inputs::asset_ids::AssetIdArg;
use crate::inputs::fees::PayoutArg;
use crate::inputs::oracle_key::OracleKeyArg;
use crate::inputs::percent::PercentArg;
use crate::outcome::{DexEvents, ExpectedOutcome};
use crate::planning::{self, FunctionCall, TransactionPlan, amp_storage, format_near, storage};

const CREATE_POOL_GAS: Gas = Gas::from_teragas(100);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = AmpContext)]
#[interactive_clap(output_context = NewPoolContext)]
pub struct CreatePool {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account, the owner of the dex, creates the pool, puts in its first liquidity and pays for its storage?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The first asset: near, nep141:<token contract> or nep245:<contract>:<token>
    asset_0: AssetIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// The second asset
    asset_1: AssetIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How much of one asset goes in as the first liquidity, from the signer's balance on dex.intear.near, e.g. '100000 NEAR'
    amount_0: AmountArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How much of the other asset, e.g. '370000 USDC'
    amount_1: AmountArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How flat the curve is around the oracle's price, from 1 to 1000000, e.g. 100
    amplification: AmplificationArg,
    #[interactive_clap(skip_default_input_arg)]
    /// The key of the oracle on dex.intear.near that swaps ask for the prices of the assets, e.g. 'ed25519:…'
    oracle_key: OracleKeyArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How the signer takes its part of the pool's fees: 'keep' to keep both assets, or the asset to take them in
    creator_payout: PayoutArg,
    #[interactive_clap(named_arg)]
    /// The fees of swaps
    fees: PoolFees,
}

impl CreatePool {
    fn input_signer_account_id(
        context: &AmpContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        super::input_dex_owner(
            context,
            "Which account creates the pool, puts in its first liquidity and pays for its storage?",
        )
    }

    fn input_asset_0(_context: &AmpContext) -> color_eyre::eyre::Result<Option<AssetIdArg>> {
        crate::inputs::prompt("The first asset? e.g. near or nep141:usdt.tether-token.near")
    }

    fn input_asset_1(_context: &AmpContext) -> color_eyre::eyre::Result<Option<AssetIdArg>> {
        crate::inputs::prompt("The second asset?")
    }

    fn input_amount_0(_context: &AmpContext) -> color_eyre::eyre::Result<Option<AmountArg>> {
        crate::inputs::prompt(
            "How much of one asset goes in as the first liquidity? e.g. '100000 NEAR'",
        )
    }

    fn input_amount_1(_context: &AmpContext) -> color_eyre::eyre::Result<Option<AmountArg>> {
        crate::inputs::prompt("How much of the other asset? e.g. '370000 USDC'")
    }

    fn input_amplification(
        _context: &AmpContext,
    ) -> color_eyre::eyre::Result<Option<AmplificationArg>> {
        crate::inputs::prompt(
            "The amplification? From 1 to 1000000; the higher, the less the pool's imbalance moves its price away from the oracle's. e.g. 100",
        )
    }

    fn input_oracle_key(_context: &AmpContext) -> color_eyre::eyre::Result<Option<OracleKeyArg>> {
        crate::inputs::prompt(
            "The key of the oracle on dex.intear.near that swaps ask for prices? e.g. ed25519:…",
        )
    }

    fn input_creator_payout(_context: &AmpContext) -> color_eyre::eyre::Result<Option<PayoutArg>> {
        crate::inputs::prompt(
            "How do you take your part of the pool's fees? 'keep' to keep both assets, or the asset to take them in",
        )
    }
}

#[derive(Clone)]
pub struct NewPoolContext {
    global_context: crate::GlobalContext,
    dex_id: DexId,
    signer_id: AccountId,
    assets: (AssetId, AssetId),
    amounts: (AmountArg, AmountArg),
    amplification: Amplification,
    oracle_key: PublicKeyHandle,
    creator_payout: PayoutArg,
}

impl NewPoolContext {
    pub fn from_previous_context(
        previous_context: AmpContext,
        scope: &<CreatePool as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let AssetIdArg(asset_0) = scope.asset_0.clone();
        let AssetIdArg(asset_1) = scope.asset_1.clone();
        let AmplificationArg(amplification) = scope.amplification;
        let OracleKeyArg(oracle_key) = scope.oracle_key.clone();
        Ok(Self {
            global_context: previous_context.global_context,
            dex_id: previous_context.dex_id,
            signer_id: scope.signer_account_id.clone().into(),
            assets: (asset_0, asset_1),
            amounts: (scope.amount_0.clone(), scope.amount_1.clone()),
            amplification,
            oracle_key,
            creator_payout: scope.creator_payout.clone(),
        })
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = NewPoolContext)]
#[interactive_clap(output_context = PoolFeesContext)]
pub struct PoolFees {
    #[interactive_clap(skip_default_input_arg)]
    /// The fee of swaps, of which the protocol takes 20%, e.g. 0.05%
    fee: PercentArg,
    #[interactive_clap(skip_default_input_arg)]
    /// The oracle fee of swaps of one asset, such as for the gas of the oracle's responses, e.g. '0.001 NEAR'
    oracle_fee_0: AmountArg,
    #[interactive_clap(skip_default_input_arg)]
    /// The oracle fee of swaps of the other asset, e.g. '0.005 USDC'
    oracle_fee_1: AmountArg,
    #[interactive_clap(skip_default_input_arg)]
    /// Which account takes the oracle fees, in the fees it collects on the dex?
    oracle_fee_receiver: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl PoolFees {
    fn input_fee(_context: &NewPoolContext) -> color_eyre::eyre::Result<Option<PercentArg>> {
        crate::inputs::prompt("The fee of swaps? The protocol takes 20% of it. e.g. 0.05%")
    }

    fn input_oracle_fee_0(
        _context: &NewPoolContext,
    ) -> color_eyre::eyre::Result<Option<AmountArg>> {
        crate::inputs::prompt(
            "The oracle fee of swaps of one asset, such as for the gas of the oracle's responses? e.g. '0.001 NEAR'",
        )
    }

    fn input_oracle_fee_1(
        _context: &NewPoolContext,
    ) -> color_eyre::eyre::Result<Option<AmountArg>> {
        crate::inputs::prompt("The oracle fee of swaps of the other asset? e.g. '0.005 USDC'")
    }

    fn input_oracle_fee_receiver(
        context: &NewPoolContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_non_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account takes the oracle fees, in the fees it collects on the dex?",
        )
    }
}

/// The decimals of an asset, which rate its amounts at the oracle's prices of
/// whole tokens
fn asset_decimals(
    asset_id: &AssetId,
    metadata_cache: &AssetMetadataCache,
) -> color_eyre::eyre::Result<u8> {
    let metadata = metadata_cache.get(asset_id)?.ok_or_else(|| {
        PreflightError::NoAssetDecimals {
            asset_id: asset_id.clone(),
        }
        .into_report()
    })?;
    Ok(metadata.decimals)
}

#[derive(Clone)]
pub struct PoolFeesContext(ActionContext);

impl PoolFeesContext {
    pub fn from_previous_context(
        previous_context: NewPoolContext,
        scope: &<PoolFees as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let NewPoolContext {
            global_context,
            dex_id,
            signer_id,
            assets,
            amounts,
            amplification,
            oracle_key,
            creator_payout,
        } = previous_context;
        let PercentArg(fee_millionths) = scope.fee;
        let oracle_fees = (scope.oracle_fee_0.clone(), scope.oracle_fee_1.clone());
        let oracle_fee_receiver: AccountId = scope.oracle_fee_receiver.clone().into();
        Ok(Self(planning::write_action_context(
            &global_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                preflight.ensure_dex_exists(&dex_id)?;
                super::ensure_dex_owner(preflight, &dex_id, "create pools")?;
                let signer_id = preflight.signer_id.clone();
                if assets.0 == assets.1 {
                    return Err(PreflightError::SameAssets {
                        asset_id: assets.0.clone(),
                    }
                    .into_report());
                }
                ensure_poolable(&assets.0)?;
                ensure_poolable(&assets.1)?;
                let metadata_cache = AssetMetadataCache::new(
                    &preflight.network_config,
                    preflight.block_reference.clone(),
                );
                let decimals = (
                    asset_decimals(&assets.0, &metadata_cache)?,
                    asset_decimals(&assets.1, &metadata_cache)?,
                );
                let asset_metadata = [
                    (assets.0.clone(), metadata_cache.get(&assets.0)?),
                    (assets.1.clone(), metadata_cache.get(&assets.1)?),
                ];
                let pair = format!(
                    "{} / {}",
                    display::asset_label(&assets.0, asset_metadata[0].1.as_ref()),
                    display::asset_label(&assets.1, asset_metadata[1].1.as_ref())
                );
                let (amount_0, amount_1) = amounts_in_pool_order(&asset_metadata, &pair, &amounts)?;
                // NEAR goes in together with the storage deposit
                let mut attached_assets = Vec::new();
                let mut amount_labels = Vec::new();
                for ((asset_id, metadata), amount) in
                    asset_metadata.iter().zip([amount_0, amount_1])
                {
                    preflight.ensure_engine_balance(asset_id, metadata.as_ref(), amount)?;
                    amount_labels.push(display::format_amount(amount, metadata.as_ref()));
                    if *asset_id != AssetId::Near {
                        attached_assets.push((asset_id.clone(), amount));
                    }
                }
                let creator_fee_payout_type = match creator_payout
                    .payout_type([&assets.0, &assets.1])
                    .map_err(color_eyre::eyre::Report::msg)?
                {
                    xyk_dex_types::FeePayoutType::KeepBoth => FeePayoutType::KeepBoth,
                    xyk_dex_types::FeePayoutType::ConvertToFirstAsset => {
                        FeePayoutType::ConvertToFirstAsset
                    }
                    xyk_dex_types::FeePayoutType::ConvertToSecondAsset => {
                        FeePayoutType::ConvertToSecondAsset
                    }
                    xyk_dex_types::FeePayoutType::Reinvest => color_eyre::eyre::bail!(
                        "Amp pools don't reinvest fees: keep them, or take them in one of the pool's assets"
                    ),
                };
                let fees = super::fee_configuration(
                    fee_millionths,
                    &oracle_fees,
                    oracle_fee_receiver.clone(),
                    &asset_metadata,
                    &pair,
                )?;

                let extra_bytes_per_record = preflight.protocol_limits.extra_bytes_per_record;
                // The pool starts fee balances for the protocol and the
                // oracle fee receiver in both assets, and for the creator in
                // the assets its payout takes fees in
                let fee_balances = [
                    FeeBalances {
                        kind: FeeBalanceKind::CollectedByOwner,
                        account_id: PROTOCOL_FEE_RECEIVER_ID.to_owned(),
                        asset_ids: vec![assets.0.clone(), assets.1.clone()],
                    },
                    FeeBalances {
                        kind: FeeBalanceKind::CollectedByOwner,
                        account_id: signer_id.clone(),
                        asset_ids: match creator_fee_payout_type {
                            FeePayoutType::KeepBoth => vec![assets.0.clone(), assets.1.clone()],
                            FeePayoutType::ConvertToFirstAsset => vec![assets.0.clone()],
                            FeePayoutType::ConvertToSecondAsset => vec![assets.1.clone()],
                        },
                    },
                    FeeBalances {
                        kind: FeeBalanceKind::CollectedByOwner,
                        account_id: oracle_fee_receiver.clone(),
                        asset_ids: vec![assets.0.clone(), assets.1.clone()],
                    },
                ];
                let storage_bytes = [
                    amp_storage::new_pool_bytes(
                        &dex_id,
                        (&assets.0, &assets.1),
                        &oracle_key,
                        &fees,
                        extra_bytes_per_record,
                    )?,
                    amp_storage::liquidity_position_bytes(
                        &dex_id,
                        &signer_id,
                        extra_bytes_per_record,
                    )?,
                    new_fee_balances_bytes(preflight, &dex_id, &fee_balances)?,
                ]
                .into_iter()
                .try_fold(0u64, |total, bytes| total.checked_add(bytes))
                .ok_or_else(|| color_eyre::eyre::eyre!("Storage size overflow"))?;
                let storage_near = storage::storage_cost(
                    storage_bytes,
                    preflight.protocol_limits.storage_byte_cost,
                )?;
                let initial_liquidity_near = if assets.0 == AssetId::Near {
                    amount_0
                } else if assets.1 == AssetId::Near {
                    amount_1
                } else {
                    0
                };
                attached_assets.insert(
                    0,
                    (
                        AssetId::Near,
                        storage_near
                            .as_yoctonear()
                            .checked_add(initial_liquidity_near)
                            .ok_or_else(|| color_eyre::eyre::eyre!("NEAR amount overflow"))?,
                    ),
                );

                let mut registrations =
                    planning::storage_near_registrations(&signer_id, &dex_id).to_vec();
                // The dex holds the pool's assets
                registrations.push((AccountOrDexId::Dex(dex_id.clone()), assets.0.clone()));
                registrations.push((AccountOrDexId::Dex(dex_id.clone()), assets.1.clone()));
                let registration_calls = preflight.registration_calls(&registrations)?;
                let mut steps = registration_calls.steps;
                let mut function_calls = registration_calls.function_calls;
                steps.push(format!(
                    "Create a {pair} pool on {dex_id} with amplification {amplification} and a fee of {}, of which the protocol takes {PROTOCOL_FEE_SHARE}",
                    display::format_fee(fees.fraction)
                ));
                steps.push(format!(
                    "Swaps that put in {} pay {oracle_fee_receiver} an oracle fee of {}, and swaps that put in {} pay it {}, before the fee",
                    display::asset_label(&assets.0, asset_metadata[0].1.as_ref()),
                    display::format_amount(fees.oracle_fee_per_swap.0.get(), asset_metadata[0].1.as_ref()),
                    display::asset_label(&assets.1, asset_metadata[1].1.as_ref()),
                    display::format_amount(fees.oracle_fee_per_swap.1.get(), asset_metadata[1].1.as_ref())
                ));
                steps.push(format!(
                    "Swaps on it ask the oracle {oracle_key} for the prices of whole tokens of both assets, which rate them at {} and {} decimals",
                    decimals.0, decimals.1
                ));
                steps.push(format!(
                    "Put {} and {} from {signer_id}'s balance on {ENGINE_ACCOUNT_ID} into the pool as its first liquidity, for all of its shares",
                    amount_labels[0], amount_labels[1]
                ));
                steps.push(match creator_fee_payout_type {
                    FeePayoutType::KeepBoth => {
                        format!("{signer_id} takes its part of the pool's fees in both assets")
                    }
                    FeePayoutType::ConvertToFirstAsset => format!(
                        "{signer_id} takes its part of the pool's fees in {}",
                        display::asset_label(&assets.0, asset_metadata[0].1.as_ref())
                    ),
                    FeePayoutType::ConvertToSecondAsset => format!(
                        "{signer_id} takes its part of the pool's fees in {}",
                        display::asset_label(&assets.1, asset_metadata[1].1.as_ref())
                    ),
                });
                steps.push(format!(
                    "Attach {} from {signer_id}'s wallet for the pool's storage, which takes about that; {dex_id} sends back what it doesn't use",
                    format_near(storage_near)
                ));
                function_calls.push(FunctionCall {
                    method_name: "execute_operations",
                    args: json!({
                        "operations": [planning::dex_call_operation(
                            &dex_id,
                            "create_pool",
                            &CreatePoolArgs {
                                assets: assets.clone(),
                                decimals,
                                amplification,
                                oracle_key: oracle_key.clone(),
                                fees,
                                initial_liquidity: (
                                    TokenAmount::new(amount_0),
                                    TokenAmount::new(amount_1),
                                ),
                                creator_fee_payout_type,
                            },
                            &attached_assets,
                        )?],
                    })
                    .into(),
                    deposit: storage_near,
                    gas: CREATE_POOL_GAS,
                });
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "pool creation",
                        success_message: format!("Created a {pair} pool on {dex_id}"),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        dex_events: DexEvents::Amp,
                        deployment: None,
                    },
                    deploy_code: None,
                })
            },
        )))
    }
}

impl From<PoolFeesContext> for ActionContext {
    fn from(item: PoolFeesContext) -> Self {
        item.0
    }
}
