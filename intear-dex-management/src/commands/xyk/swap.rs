use intear_dex_types::{
    AccountOrDexId, AssetId, DepositMessage, DexId, Operation, SwapOperationAmount, SwapRequest,
    SwapRequestAmount, WithdrawAmount,
};
use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::NearToken;
use near_sdk::json_types::{Base64VecU8, U128};
use serde_json::json;
use xyk_dex_types::{PoolId, PoolView, SwapArgs, asset_account_ids};

use super::presentation::pool_assets;
use super::{PoolToWrite, XykContext};
use crate::chain::{engine, fungible_token, xyk};
use crate::commands::engine::deposit::DEPOSIT_NEAR_GAS;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display;
use crate::errors::PreflightError;
use crate::inputs::amount::AmountArg;
use crate::inputs::funds::{FundsDestinationArg, FundsSourceArg, TradeDirectionArg};
use crate::inputs::percent::PercentArg;
use crate::inputs::pool_id::PoolIdArg;
use crate::outcome::{ExpectedOutcome, ExpectedTransferCall};
use crate::planning::xyk_math::{at_least_with_slippage, at_most_with_slippage};
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, Preflight, TransactionPlan};

const SWAP_GAS: Gas = Gas::from_teragas(100);
/// The token contract keeps some of it to resolve the transfer
const SWAP_FT_TRANSFER_CALL_GAS: Gas = Gas::from_teragas(150);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = SwapContext)]
pub struct Swap {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account swaps?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The pool id
    pool_id: PoolIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// sell an exact amount, or buy an exact amount
    direction: TradeDirectionArg,
    #[interactive_clap(skip_default_input_arg)]
    /// The amount to sell or buy, e.g. '10 NEAR'
    amount: AmountArg,
    #[interactive_clap(named_arg)]
    /// How far the price may move against you before the swap
    max_slippage: SwapMaxSlippage,
}

impl Swap {
    fn input_signer_account_id(
        context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account swaps?",
        )
    }

    fn input_pool_id(context: &XykContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
        super::input_pool_id(context)
    }

    fn input_direction(
        _context: &XykContext,
    ) -> color_eyre::eyre::Result<Option<TradeDirectionArg>> {
        crate::inputs::select(
            "Sell an exact amount, or buy an exact amount?",
            vec![TradeDirectionArg::Sell, TradeDirectionArg::Buy],
        )
    }

    fn input_amount(_context: &XykContext) -> color_eyre::eyre::Result<Option<AmountArg>> {
        crate::inputs::prompt(
            "How much? e.g. '10 NEAR', '25.5 USDT' or '1000000 raw nep141:usdt.tether-token.near'",
        )
    }
}

#[derive(Clone)]
pub struct SwapContext {
    xyk_context: XykContext,
    signer_id: AccountId,
    pool_id: PoolId,
    direction: TradeDirectionArg,
    amount: AmountArg,
}

impl SwapContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<Swap as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self {
            xyk_context: previous_context,
            signer_id: scope.signer_account_id.clone().into(),
            pool_id: scope.pool_id.0,
            direction: scope.direction,
            amount: scope.amount.clone(),
        })
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = SwapContext)]
#[interactive_clap(output_context = SwapMaxSlippageContext)]
pub struct SwapMaxSlippage {
    #[interactive_clap(skip_default_input_arg)]
    /// The most the price may move against you, e.g. 0.5%
    percent: PercentArg,
    #[interactive_clap(skip_default_input_arg)]
    /// from-wallet, or from-balance on dex.intear.near
    source: FundsSourceArg,
    #[interactive_clap(skip_default_input_arg)]
    /// to-wallet, or to-balance on dex.intear.near
    destination: FundsDestinationArg,
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// An account that takes a referral fee, if it set one up on the dex
    referrer: Option<near_cli_rs::types::account_id::AccountId>,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl SwapMaxSlippage {
    fn input_percent(_context: &SwapContext) -> color_eyre::eyre::Result<Option<PercentArg>> {
        crate::inputs::prompt("Max slippage? e.g. 0.5%")
    }

    fn input_source(_context: &SwapContext) -> color_eyre::eyre::Result<Option<FundsSourceArg>> {
        crate::inputs::select(
            "Spend from the wallet, or from the balance on dex.intear.near?",
            vec![FundsSourceArg::FromWallet, FundsSourceArg::FromBalance],
        )
    }

    fn input_destination(
        _context: &SwapContext,
    ) -> color_eyre::eyre::Result<Option<FundsDestinationArg>> {
        crate::inputs::select(
            "Receive into the wallet, or into the balance on dex.intear.near?",
            vec![
                FundsDestinationArg::ToWallet,
                FundsDestinationArg::ToBalance,
            ],
        )
    }
}

/// Whether the referrer takes a fee from this swap, which needs referral
/// settings on the dex and fees collected in the asset the fee is paid in
fn referral_step(
    preflight: &Preflight,
    dex_id: &DexId,
    referrer_id: &AccountId,
    pool: &PoolView,
    (asset_in, asset_out): (&AssetId, &AssetId),
) -> color_eyre::eyre::Result<String> {
    let Some(referral_settings) = xyk::referral_settings(
        &preflight.network_config,
        &preflight.block_reference,
        dex_id,
        referrer_id,
    )?
    else {
        return Ok(format!(
            "{referrer_id} is the referrer, but takes no fee: it has no referral settings on {dex_id}"
        ));
    };
    let referral_fee = referral_settings.fee_fraction(&asset_account_ids([asset_in, asset_out]));
    if referral_fee == 0 {
        return Ok(format!(
            "{referrer_id} is the referrer, but its referral fee for this pair is 0%"
        ));
    }
    // Launch pools pay the fees of sales for NEAR in NEAR
    let fee_asset_id = match pool {
        PoolView::Launch { .. } if *asset_out == AssetId::Near => AssetId::Near,
        PoolView::Launch { .. } | PoolView::Private { .. } | PoolView::Public { .. } => {
            asset_in.clone()
        }
    };
    let collects_fee_asset = xyk::pending_fees(
        &preflight.network_config,
        &preflight.block_reference,
        dex_id,
        referrer_id,
        vec![fee_asset_id.clone()],
    )?
    .contains_key(&fee_asset_id);
    if !collects_fee_asset {
        return Ok(format!(
            "{referrer_id} is the referrer, but takes no fee: it doesn't collect fees in {fee_asset_id} on {dex_id}"
        ));
    }
    Ok(format!(
        "{referrer_id} takes {} of the swap as its referral fee",
        display::format_fee(referral_fee)
    ))
}

#[derive(Clone)]
pub struct SwapMaxSlippageContext(ActionContext);

impl SwapMaxSlippageContext {
    pub fn from_previous_context(
        previous_context: SwapContext,
        scope: &<SwapMaxSlippage as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let SwapContext {
            xyk_context,
            signer_id,
            pool_id,
            direction,
            amount,
        } = previous_context;
        let dex_id = xyk_context.dex_id;
        let PercentArg(max_slippage) = scope.percent;
        let source = scope.source;
        let destination = scope.destination;
        let referrer_id: Option<AccountId> = scope.referrer.clone().map(Into::into);
        Ok(Self(planning::write_action_context(
            &xyk_context.global_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                let signer_id = preflight.signer_id.clone();
                let pool = PoolToWrite::read(preflight, &dex_id, pool_id)?;
                let (amount_asset_id, raw_amount) = amount.resolve(&pool.assets)?;
                if raw_amount == 0 {
                    return Err(PreflightError::ZeroAmount.into_report());
                }
                let [(_, reserve_0), (_, reserve_1)] = pool_assets(&pool.pool);
                if reserve_0 == 0 || reserve_1 == 0 {
                    return Err(PreflightError::EmptyPool {
                        dex_id: dex_id.clone(),
                        pool_id,
                    }
                    .into_report());
                }
                let [first_asset, second_asset] = &pool.assets;
                let (amount_asset, other_asset) = if amount_asset_id == first_asset.0 {
                    (first_asset, second_asset)
                } else {
                    (second_asset, first_asset)
                };
                let ((asset_in, metadata_in), (asset_out, metadata_out)) = match direction {
                    TradeDirectionArg::Sell => (amount_asset, other_asset),
                    TradeDirectionArg::Buy => (other_asset, amount_asset),
                };
                let request_amount = match direction {
                    TradeDirectionArg::Sell => SwapRequestAmount::ExactIn(U128(raw_amount)),
                    TradeDirectionArg::Buy => SwapRequestAmount::ExactOut(U128(raw_amount)),
                };
                if let Some(referrer_id) = &referrer_id {
                    preflight.ensure_account_exists(referrer_id)?;
                }
                let swap_message = Base64VecU8(near_sdk::borsh::to_vec(&SwapArgs { pool_id })?);
                let (U128(quoted_amount_in), U128(quoted_amount_out)) =
                    engine::simulate_swap_simple(
                        &preflight.network_config,
                        &preflight.block_reference,
                        &dex_id,
                        &SwapRequest {
                            message: swap_message.clone(),
                            asset_in: asset_in.clone(),
                            asset_out: asset_out.clone(),
                            amount: request_amount,
                            referrer: referrer_id.clone(),
                        },
                        &signer_id,
                    )?;
                let format_in =
                    |raw_amount| display::format_amount(raw_amount, metadata_in.as_ref());
                let format_out =
                    |raw_amount| display::format_amount(raw_amount, metadata_out.as_ref());
                let label_in = display::asset_label(asset_in, metadata_in.as_ref());
                let label_out = display::asset_label(asset_out, metadata_out.as_ref());
                let pool_label = format!("pool #{pool_id} ({}) of {dex_id}", pool.pair_label());
                let wallet_label = format!("{signer_id}'s wallet");
                let balance_label = format!("{signer_id}'s balance on {ENGINE_ACCOUNT_ID}");
                // NEAR from the wallet goes into the balance first when the
                // output stays there too, so the swap spends from the balance
                let spends_from_balance = match (source, destination) {
                    (FundsSourceArg::FromBalance, _) => true,
                    (FundsSourceArg::FromWallet, FundsDestinationArg::ToBalance) => {
                        *asset_in == AssetId::Near
                    }
                    (FundsSourceArg::FromWallet, FundsDestinationArg::ToWallet) => false,
                };
                let source_label = if spends_from_balance {
                    &balance_label
                } else {
                    &wallet_label
                };
                let destination_label = match destination {
                    FundsDestinationArg::ToWallet => &wallet_label,
                    FundsDestinationArg::ToBalance => &balance_label,
                };
                // What the signer sends at most, and what the swap may not
                // fall short of: the least output of a sale, or the most
                // input of a purchase
                let (most_spent, constraint, swap_step) = match direction {
                    TradeDirectionArg::Sell => {
                        if quoted_amount_out == 0 {
                            return Err(PreflightError::SwapGivesNothing {
                                amount: format_in(raw_amount),
                                dex_id: dex_id.clone(),
                                pool_id,
                            }
                            .into_report());
                        }
                        let min_amount_out =
                            at_least_with_slippage(quoted_amount_out, max_slippage)?;
                        (
                            raw_amount,
                            min_amount_out,
                            format!(
                                "Sell {} on {pool_label} for at least {}, {} at the current price",
                                format_in(raw_amount),
                                format_out(min_amount_out),
                                format_out(quoted_amount_out)
                            ),
                        )
                    }
                    TradeDirectionArg::Buy => {
                        let max_amount_in = at_most_with_slippage(quoted_amount_in, max_slippage)?;
                        (
                            max_amount_in,
                            max_amount_in,
                            format!(
                                "Buy {} on {pool_label} for at most {}, {} at the current price",
                                format_out(raw_amount),
                                format_in(max_amount_in),
                                format_in(quoted_amount_in)
                            ),
                        )
                    }
                };
                let source_step = format!("The {label_in} comes from {source_label}");
                if let (FundsDestinationArg::ToWallet, AssetId::Nep141(token_id)) =
                    (destination, asset_out)
                    && !fungible_token::is_registered(
                        &preflight.network_config,
                        &preflight.block_reference,
                        token_id,
                        &signer_id,
                    )?
                {
                    return Err(PreflightError::NotRegisteredWithToken {
                        account_id: signer_id.clone(),
                        token_id: token_id.clone(),
                    }
                    .into_report());
                }
                let swap_operation = Operation::SwapSimple {
                    dex_id: dex_id.clone(),
                    message: swap_message,
                    asset_in: asset_in.clone(),
                    asset_out: asset_out.clone(),
                    amount: SwapOperationAmount::Amount(request_amount),
                    constraint: Some(U128(constraint)),
                };
                let withdraw_output = Operation::Withdraw {
                    asset_id: asset_out.clone(),
                    amount: WithdrawAmount::PreviousSwapOutput,
                    to: None,
                    rescue_address: None,
                };
                let withdraw_unspent_input = Operation::Withdraw {
                    asset_id: asset_in.clone(),
                    amount: WithdrawAmount::Full { at_least: None },
                    to: None,
                    rescue_address: None,
                };
                let signer = AccountOrDexId::Account(signer_id.clone());
                let mut steps = Vec::new();
                let mut function_calls = Vec::new();
                let mut transfer_call = None;
                let receiver_id = match (source, destination, asset_in) {
                    (FundsSourceArg::FromBalance, _, _) => {
                        preflight.ensure_engine_balance(
                            asset_in,
                            metadata_in.as_ref(),
                            most_spent,
                        )?;
                        let registration_calls =
                            preflight.registration_calls(&[(signer.clone(), asset_out.clone())])?;
                        steps.extend(registration_calls.steps);
                        function_calls.extend(registration_calls.function_calls);
                        steps.push(swap_step);
                        steps.push(source_step);
                        let mut operations = vec![swap_operation];
                        if let FundsDestinationArg::ToWallet = destination {
                            operations.push(withdraw_output);
                        }
                        function_calls.push(FunctionCall {
                            method_name: "execute_operations",
                            args: json!({ "operations": operations, "referrer": referrer_id }),
                            deposit: ONE_YOCTO_NEAR,
                            gas: SWAP_GAS,
                        });
                        ENGINE_ACCOUNT_ID.to_owned()
                    }
                    (FundsSourceArg::FromWallet, FundsDestinationArg::ToBalance, AssetId::Near) => {
                        let registration_calls = preflight.registration_calls(&[
                            (signer.clone(), AssetId::Near),
                            (signer.clone(), asset_out.clone()),
                        ])?;
                        steps.extend(registration_calls.steps);
                        function_calls.extend(registration_calls.function_calls);
                        steps.push(format!(
                            "Deposit {} from {wallet_label} into {balance_label}",
                            format_in(most_spent)
                        ));
                        steps.push(swap_step);
                        steps.push(source_step);
                        if let TradeDirectionArg::Buy = direction {
                            steps.push(
                                "What the swap doesn't use of the deposit stays in that balance"
                                    .to_string(),
                            );
                        }
                        function_calls.push(FunctionCall {
                            method_name: "deposit_near",
                            args: json!({}),
                            deposit: NearToken::from_yoctonear(most_spent),
                            gas: DEPOSIT_NEAR_GAS,
                        });
                        function_calls.push(FunctionCall {
                            method_name: "execute_operations",
                            args: json!({ "operations": [swap_operation], "referrer": referrer_id }),
                            deposit: ONE_YOCTO_NEAR,
                            gas: SWAP_GAS,
                        });
                        ENGINE_ACCOUNT_ID.to_owned()
                    }
                    (FundsSourceArg::FromWallet, FundsDestinationArg::ToBalance, _) => {
                        let deposit_amount = match metadata_in {
                            Some(metadata) => format!(
                                "{} {}",
                                display::decimal_amount(most_spent, metadata.decimals),
                                metadata.symbol
                            ),
                            None => format!("{most_spent} raw"),
                        };
                        return Err(PreflightError::WalletToBalanceSwapOfToken {
                            deposit_command: shell_words::join([
                                "near",
                                "intear-dex-management",
                                "engine",
                                "deposit",
                                signer_id.as_str(),
                                &asset_in.to_string(),
                                &deposit_amount,
                                "network-config",
                                &preflight.connection_name,
                            ]),
                        }
                        .into_report());
                    }
                    // The engine swaps what comes from the wallet apart from
                    // the signer's balance and withdraws all it ends with; a
                    // withdrawal that fails goes into the signer's balance,
                    // so the assets have to be registered there
                    (FundsSourceArg::FromWallet, FundsDestinationArg::ToWallet, _) => {
                        let mut operations = vec![swap_operation, withdraw_output];
                        let mut registrations = vec![(signer.clone(), asset_out.clone())];
                        if let TradeDirectionArg::Buy = direction {
                            operations.push(withdraw_unspent_input);
                            registrations.push((signer.clone(), asset_in.clone()));
                        }
                        let deposit_message = DepositMessage::Advanced {
                            operations,
                            referrer: referrer_id.clone(),
                        };
                        match asset_in {
                            AssetId::Near => {
                                let registration_calls =
                                    preflight.registration_calls(&registrations)?;
                                steps.extend(registration_calls.steps);
                                function_calls.extend(registration_calls.function_calls);
                                steps.push(swap_step);
                                steps.push(source_step);
                                function_calls.push(FunctionCall {
                                    method_name: "deposit_near",
                                    args: json!({ "operations": deposit_message }),
                                    deposit: NearToken::from_yoctonear(most_spent),
                                    gas: SWAP_GAS,
                                });
                                ENGINE_ACCOUNT_ID.to_owned()
                            }
                            AssetId::Nep141(token_id) => {
                                // ft_transfer_call goes to the token contract,
                                // so missing registrations can't be added
                                let mut missing_asset_ids = Vec::new();
                                for (owner, asset_id) in &registrations {
                                    if engine::asset_balance_of(
                                        &preflight.network_config,
                                        &preflight.block_reference,
                                        owner,
                                        asset_id,
                                    )?
                                    .is_none()
                                    {
                                        missing_asset_ids.push(asset_id.clone());
                                    }
                                }
                                if !missing_asset_ids.is_empty() {
                                    let asset_labels = missing_asset_ids
                                        .iter()
                                        .map(|asset_id| {
                                            let metadata = if asset_id == asset_in {
                                                metadata_in
                                            } else {
                                                metadata_out
                                            };
                                            display::asset_label(asset_id, metadata.as_ref())
                                        })
                                        .collect();
                                    let asset_ids = missing_asset_ids
                                        .iter()
                                        .map(AssetId::to_string)
                                        .collect::<Vec<_>>()
                                        .join(",");
                                    return Err(PreflightError::AssetNotRegisteredForSigner {
                                        signer_id: signer_id.clone(),
                                        asset_labels,
                                        register_command: shell_words::join([
                                            "near",
                                            "intear-dex-management",
                                            "engine",
                                            "assets",
                                            "register",
                                            signer_id.as_str(),
                                            &asset_ids,
                                            "network-config",
                                            &preflight.connection_name,
                                        ]),
                                    }
                                    .into_report());
                                }
                                let wallet_balance = fungible_token::ft_balance_of(
                                    &preflight.network_config,
                                    &preflight.block_reference,
                                    token_id,
                                    &signer_id,
                                )?;
                                if wallet_balance < most_spent {
                                    return Err(PreflightError::InsufficientTokenBalance {
                                        account_id: signer_id.clone(),
                                        available: format_in(wallet_balance),
                                        needed: format_in(most_spent),
                                    }
                                    .into_report());
                                }
                                steps.push(swap_step);
                                steps.push(source_step);
                                function_calls.push(FunctionCall {
                                    method_name: "ft_transfer_call",
                                    args: json!({
                                        "receiver_id": ENGINE_ACCOUNT_ID,
                                        "amount": U128(most_spent),
                                        "msg": serde_json::to_string(&deposit_message)?,
                                    }),
                                    deposit: ONE_YOCTO_NEAR,
                                    gas: SWAP_FT_TRANSFER_CALL_GAS,
                                });
                                transfer_call = Some(ExpectedTransferCall {
                                    token_id: token_id.clone(),
                                    sender_id: signer_id.clone(),
                                    amount: most_spent,
                                    metadata: metadata_in.clone(),
                                });
                                token_id.clone()
                            }
                            AssetId::Nep171(_, _) | AssetId::Nep245(_, _) => {
                                return Err(PreflightError::UnsupportedDeposit {
                                    asset_id: asset_in.to_string(),
                                }
                                .into_report());
                            }
                        }
                    }
                };
                if let (
                    FundsSourceArg::FromWallet,
                    FundsDestinationArg::ToWallet,
                    TradeDirectionArg::Buy,
                ) = (source, destination, direction)
                {
                    steps.push(format!(
                        "What the swap doesn't use of the {} goes back to {wallet_label}",
                        format_in(most_spent)
                    ));
                }
                steps.push(format!("The {label_out} goes to {destination_label}"));
                if let Some(referrer_id) = &referrer_id {
                    steps.push(referral_step(
                        preflight,
                        &dex_id,
                        referrer_id,
                        &pool.pool,
                        (asset_in, asset_out),
                    )?);
                }
                Ok(TransactionPlan {
                    receiver_id,
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "swap",
                        success_message: format!(
                            "Swapped on {pool_label}; the {label_out} went to {destination_label}"
                        ),
                        transfer_call,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<SwapMaxSlippageContext> for ActionContext {
    fn from(item: SwapMaxSlippageContext) -> Self {
        item.0
    }
}
