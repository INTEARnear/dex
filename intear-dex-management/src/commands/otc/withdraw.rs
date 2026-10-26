use intear_dex_types::{AccountOrDexId, AssetId};
use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::json_types::U128;
use otc_dex_types::{WithdrawAssetsArgs, WithdrawRequest};
use serde_json::json;

use super::{OTC_CALL_GAS, OtcContext};
use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::{fungible_token, otc};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display;
use crate::errors::PreflightError;
use crate::inputs::amount::AmountOrAllArg;
use crate::inputs::asset_ids::AssetIdArg;
use crate::inputs::funds::FundsDestinationArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan};

/// A withdrawal to the wallet calls the asset's contract, and the engine back
const WITHDRAWAL_TO_WALLET_GAS: Gas = Gas::from_teragas(20);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = OtcContext)]
#[interactive_clap(output_context = WithdrawContext)]
pub struct Withdraw {
    #[interactive_clap(skip_default_input_arg)]
    /// Whose balance on the dex?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The asset: near, nep141:<token contract>, …
    asset: AssetIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How much, e.g. '10 NEAR' or '25.5 USDT', or all
    amount: AmountOrAllArg,
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// The account that receives it, if not the signer
    to: Option<near_cli_rs::types::account_id::AccountId>,
    #[interactive_clap(skip_default_input_arg)]
    /// to-wallet, or to-balance on dex.intear.near
    destination: FundsDestinationArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Withdraw {
    fn input_signer_account_id(
        context: &OtcContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Whose balance on the dex?",
        )
    }

    fn input_asset(_context: &OtcContext) -> color_eyre::eyre::Result<Option<AssetIdArg>> {
        crate::inputs::prompt("Which asset? e.g. near or nep141:usdt.tether-token.near")
    }

    fn input_amount(_context: &OtcContext) -> color_eyre::eyre::Result<Option<AmountOrAllArg>> {
        crate::inputs::prompt(
            "How much? With the asset's symbol, e.g. '10 NEAR' or '25.5 USDT', or all",
        )
    }

    fn input_destination(
        _context: &OtcContext,
    ) -> color_eyre::eyre::Result<Option<FundsDestinationArg>> {
        crate::inputs::select(
            "Withdraw to the wallet, or into the balance on dex.intear.near?",
            vec![
                FundsDestinationArg::ToWallet,
                FundsDestinationArg::ToBalance,
            ],
        )
    }
}

#[derive(Clone)]
pub struct WithdrawContext(ActionContext);

impl WithdrawContext {
    pub fn from_previous_context(
        previous_context: OtcContext,
        scope: &<Withdraw as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let AssetIdArg(asset_id) = scope.asset.clone();
        let amount = scope.amount.clone();
        let to: Option<AccountId> = scope.to.clone().map(Into::into);
        let destination = scope.destination;
        let dex_id = previous_context.dex_id;
        Ok(Self(planning::write_action_context(
            &previous_context.global_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                preflight.ensure_dex_exists(&dex_id)?;
                let signer_id = preflight.signer_id.clone();
                let metadata = AssetMetadataCache::new(
                    &preflight.network_config,
                    preflight.block_reference.clone(),
                )
                .get(&asset_id)?;
                let asset_label = display::asset_label(&asset_id, metadata.as_ref());
                let U128(balance) = otc::balance(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                    &signer_id,
                    &asset_id,
                )?
                .filter(|U128(balance)| *balance > 0)
                .ok_or_else(|| {
                    PreflightError::NoOtcBalance {
                        owner: signer_id.clone(),
                        asset_label: asset_label.clone(),
                        dex_id: dex_id.clone(),
                    }
                    .into_report()
                })?;
                // All of it is what the balance is when the withdrawal runs
                let requested_amount = match &amount {
                    AmountOrAllArg::All => None,
                    AmountOrAllArg::Amount(amount) => {
                        let (_, raw_amount) =
                            amount.resolve(&[(asset_id.clone(), metadata.clone())])?;
                        if raw_amount == 0 {
                            return Err(PreflightError::ZeroAmount.into_report());
                        }
                        if raw_amount > balance {
                            return Err(PreflightError::InsufficientOtcBalance {
                                owner: signer_id.clone(),
                                dex_id: dex_id.clone(),
                                available: display::format_amount(balance, metadata.as_ref()),
                                needed: display::format_amount(raw_amount, metadata.as_ref()),
                            }
                            .into_report());
                        }
                        Some(raw_amount)
                    }
                };
                let amount_label =
                    display::format_amount(requested_amount.unwrap_or(balance), metadata.as_ref());
                let receiver_id = to.clone().unwrap_or_else(|| signer_id.clone());
                if to.is_some() {
                    preflight.ensure_account_exists(&receiver_id)?;
                }
                let mut steps = Vec::new();
                let mut function_calls = Vec::new();
                let (destination_label, gas) = match destination {
                    FundsDestinationArg::ToWallet => {
                        if let AssetId::Nep141(token_id) = &asset_id
                            && !fungible_token::is_registered(
                                &preflight.network_config,
                                &preflight.block_reference,
                                token_id,
                                &receiver_id,
                            )?
                        {
                            return Err(PreflightError::NotRegisteredWithToken {
                                account_id: receiver_id.clone(),
                                token_id: token_id.clone(),
                            }
                            .into_report());
                        }
                        (
                            format!("{receiver_id}'s wallet"),
                            OTC_CALL_GAS
                                .checked_add(WITHDRAWAL_TO_WALLET_GAS)
                                .ok_or_else(|| color_eyre::eyre::eyre!("Gas overflow"))?,
                        )
                    }
                    FundsDestinationArg::ToBalance => {
                        let registration_calls = preflight.registration_calls(&[(
                            AccountOrDexId::Account(receiver_id.clone()),
                            asset_id.clone(),
                        )])?;
                        steps.extend(registration_calls.steps);
                        function_calls.extend(registration_calls.function_calls);
                        (
                            format!("{receiver_id}'s balance on {ENGINE_ACCOUNT_ID}"),
                            OTC_CALL_GAS,
                        )
                    }
                };
                steps.push(format!(
                    "Withdraw {amount_label} from {signer_id}'s balance on {dex_id} to {destination_label}"
                ));
                function_calls.push(FunctionCall {
                    method_name: "execute_operations",
                    args: json!({
                        "operations": [planning::dex_call_operation(
                            &dex_id,
                            "withdraw_assets",
                            &WithdrawAssetsArgs {
                                assets: vec![WithdrawRequest {
                                    asset_id: asset_id.clone(),
                                    amount: requested_amount.map(U128),
                                    to: to.clone(),
                                    to_inner_balance: destination == FundsDestinationArg::ToBalance,
                                }],
                            },
                            &[],
                        )?],
                    }),
                    deposit: ONE_YOCTO_NEAR,
                    gas,
                });
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "withdrawal",
                        success_message: format!(
                            "Withdrew {amount_label} from {dex_id} to {destination_label}"
                        ),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<WithdrawContext> for ActionContext {
    fn from(item: WithdrawContext) -> Self {
        item.0
    }
}
