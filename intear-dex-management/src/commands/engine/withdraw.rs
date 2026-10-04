use intear_dex_types::{AccountOrDexId, Operation, WithdrawAmount};
use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::json_types::U128;
use serde_json::json;

use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::engine;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display;
use crate::errors::PreflightError;
use crate::inputs::amount::AmountOrAllArg;
use crate::inputs::asset_ids::AssetIdArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan};

const WITHDRAW_GAS: Gas = Gas::from_teragas(50);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = WithdrawContext)]
pub struct Withdraw {
    #[interactive_clap(skip_default_input_arg)]
    /// Whose balance on dex.intear.near?
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
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Withdraw {
    fn input_signer_account_id(
        context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "Whose balance on dex.intear.near?",
        )
    }

    fn input_asset(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<AssetIdArg>> {
        crate::inputs::prompt("Which asset? e.g. near or nep141:usdt.tether-token.near")
    }

    fn input_amount(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<AmountOrAllArg>> {
        crate::inputs::prompt(
            "How much? With the asset's symbol, e.g. '10 NEAR' or '25.5 USDT', or all",
        )
    }
}

#[derive(Clone)]
pub struct WithdrawContext(ActionContext);

impl WithdrawContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Withdraw as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let signer_id: AccountId = scope.signer_account_id.clone().into();
        let AssetIdArg(asset_id) = scope.asset.clone();
        let amount = scope.amount.clone();
        let to: Option<AccountId> = scope.to.clone().map(Into::into);
        Ok(Self(planning::write_action_context(
            &previous_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                let signer_id = preflight.signer_id.clone();
                let metadata = AssetMetadataCache::new(
                    &preflight.network_config,
                    preflight.block_reference.clone(),
                )
                .get(&asset_id)?;
                let asset_label = display::asset_label(&asset_id, metadata.as_ref());
                let no_balance = || {
                    PreflightError::NoBalance {
                        owner: signer_id.to_string(),
                        asset_label: asset_label.clone(),
                    }
                    .into_report()
                };
                let U128(balance) = engine::asset_balance_of(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &AccountOrDexId::Account(signer_id.clone()),
                    &asset_id,
                )?
                .ok_or_else(no_balance)?;
                let receiver_id = to.clone().unwrap_or_else(|| signer_id.clone());
                if receiver_id != signer_id {
                    preflight.ensure_account_exists(&receiver_id)?;
                }
                let (withdraw_amount, amount_label) = match &amount {
                    AmountOrAllArg::All => {
                        if balance == 0 {
                            return Err(no_balance());
                        }
                        (
                            WithdrawAmount::Full { at_least: None },
                            format!(
                                "all {asset_label} ({})",
                                display::format_amount(balance, metadata.as_ref())
                            ),
                        )
                    }
                    AmountOrAllArg::Amount(amount) => {
                        let (_, raw_amount) =
                            amount.resolve(&[(asset_id.clone(), metadata.clone())])?;
                        if raw_amount == 0 {
                            return Err(PreflightError::ZeroAmount.into_report());
                        }
                        if raw_amount > balance {
                            return Err(PreflightError::InsufficientEngineBalance {
                                owner: signer_id.to_string(),
                                available: display::format_amount(balance, metadata.as_ref()),
                                needed: display::format_amount(raw_amount, metadata.as_ref()),
                            }
                            .into_report());
                        }
                        (
                            WithdrawAmount::Exact(U128(raw_amount)),
                            display::format_amount(raw_amount, metadata.as_ref()),
                        )
                    }
                };
                let description = format!(
                    "{amount_label} from {signer_id}'s balance on {ENGINE_ACCOUNT_ID} to {receiver_id}"
                );
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps: vec![format!("Withdraw {description}")],
                    function_calls: vec![FunctionCall {
                        method_name: "execute_operations",
                        args: json!({
                            "operations": [Operation::Withdraw {
                                asset_id: asset_id.clone(),
                                amount: withdraw_amount,
                                to: to.clone(),
                                rescue_address: None,
                            }],
                        }),
                        deposit: ONE_YOCTO_NEAR,
                        gas: WITHDRAW_GAS,
                    }],
                    expected_outcome: ExpectedOutcome {
                        action_name: "withdrawal",
                        success_message: format!("Withdrew {description}"),
                        transfer_call: None,
                        dex_id: None,
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
