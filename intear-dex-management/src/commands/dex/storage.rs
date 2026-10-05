use color_eyre::eyre::eyre;
use near_cli_rs::commands::ActionContext;
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use crate::chain::asset_metadata::AssetMetadata;
use crate::chain::engine;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;
use crate::inputs::dex_id::DexIdArg;
use crate::inputs::near_amount::NearAmountOrAllArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{
    self, ENGINE_STORAGE_DEPOSIT_GAS, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan, format_near,
};

const DEX_STORAGE_WITHDRAW_GAS: Gas = Gas::from_teragas(10);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
pub struct StorageCommands {
    #[interactive_clap(subcommand)]
    action: StorageAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do with the dex's storage balance?
pub enum StorageAction {
    #[strum_discriminants(strum(
        message = "show      - Storage balance of a dex, and whether its code is deployed"
    ))]
    /// Storage balance of a dex, and whether its code is deployed
    Show(Show),
    #[strum_discriminants(strum(
        message = "deposit   - Add NEAR to a dex's storage balance, which pays for its code and state"
    ))]
    /// Add NEAR to a dex's storage balance, which pays for its code and state
    Deposit(Deposit),
    #[strum_discriminants(strum(
        message = "withdraw  - Take NEAR that a dex doesn't use back to its deployer's wallet"
    ))]
    /// Take NEAR that a dex doesn't use back to its deployer's wallet
    Withdraw(Withdraw),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = ShowContext)]
pub struct Show {
    #[interactive_clap(skip_default_input_arg)]
    /// The dex as <deployer>/<name>
    dex: DexIdArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl Show {
    fn input_dex(_context: &crate::GlobalContext) -> color_eyre::eyre::Result<Option<DexIdArg>> {
        super::input_dex_id()
    }
}

#[derive(Clone)]
pub struct ShowContext(ArgsForViewContext);

impl ShowContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Show as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let DexIdArg(dex_id) = scope.dex.clone();
        let output_format = previous_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let storage_balance =
                    engine::dex_storage_balance_of(network_config, &block_reference, &dex_id)?;
                let is_deployed = engine::dex_exists(network_config, &block_reference, &dex_id)?;
                let near = AssetMetadata::near();
                match (output_format, storage_balance) {
                    (OutputFormat::Table, None) => {
                        println!(
                            "{dex_id} has no storage balance on {ENGINE_ACCOUNT_ID}, and {}",
                            if is_deployed {
                                "its code is deployed"
                            } else {
                                "no code"
                            }
                        );
                    }
                    (OutputFormat::Table, Some(storage_balance)) => {
                        let used = storage_balance
                            .total
                            .checked_sub(storage_balance.available)
                            .ok_or_else(|| eyre!("{ENGINE_ACCOUNT_ID} reports more storage available than in total for {dex_id}"))?;
                        print!(
                            "{}",
                            display::key_value_table(vec![
                                (
                                    "Code",
                                    if is_deployed { "deployed" } else { "none" }.to_string()
                                ),
                                (
                                    "Total",
                                    display::format_amount(
                                        storage_balance.total.as_yoctonear(),
                                        Some(&near)
                                    ),
                                ),
                                (
                                    "Used",
                                    display::format_amount(used.as_yoctonear(), Some(&near))
                                ),
                                (
                                    "Available",
                                    display::format_amount(
                                        storage_balance.available.as_yoctonear(),
                                        Some(&near),
                                    ),
                                ),
                            ])
                        );
                    }
                    (OutputFormat::Json, storage_balance) => {
                        display::print_json(&json!({
                            "dex_id": dex_id,
                            "deployed": is_deployed,
                            "registered": storage_balance.is_some(),
                            "total": storage_balance.as_ref().map(|storage_balance| display::amount_json(storage_balance.total.as_yoctonear(), Some(&near))),
                            "available": storage_balance.as_ref().map(|storage_balance| display::amount_json(storage_balance.available.as_yoctonear(), Some(&near))),
                        }))?;
                    }
                }
                Ok(())
            });
        Ok(Self(ArgsForViewContext {
            config: previous_context.config,
            interacting_with_account_ids: vec![ENGINE_ACCOUNT_ID.to_owned()],
            on_after_getting_block_reference_callback,
        }))
    }
}

impl From<ShowContext> for ArgsForViewContext {
    fn from(item: ShowContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = DepositContext)]
pub struct Deposit {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account pays?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The dex as <deployer>/<name>
    dex: DexIdArg,
    /// How much NEAR? e.g. '1 NEAR'
    amount: near_cli_rs::types::near_token::NearToken,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Deposit {
    fn input_signer_account_id(
        context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "Which account pays for the dex's storage?",
        )
    }

    fn input_dex(_context: &crate::GlobalContext) -> color_eyre::eyre::Result<Option<DexIdArg>> {
        super::input_dex_id()
    }
}

#[derive(Clone)]
pub struct DepositContext(ActionContext);

impl DepositContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Deposit as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let signer_id: AccountId = scope.signer_account_id.clone().into();
        let DexIdArg(dex_id) = scope.dex.clone();
        let near_cli_rs::types::near_token::NearToken(amount) = scope.amount;
        Ok(Self(planning::write_action_context(
            &previous_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                if amount.is_zero() {
                    return Err(PreflightError::ZeroAmount.into_report());
                }
                let minimum_deposit = engine::minimum_dex_storage_deposit(
                    &preflight.network_config,
                    &preflight.block_reference,
                )?;
                if amount < minimum_deposit {
                    return Err(PreflightError::StorageDepositTooSmall {
                        minimum: format_near(minimum_deposit),
                    }
                    .into_report());
                }
                // Only the deployer in the dex id can withdraw this NEAR, so
                // a mistyped id of a dex that doesn't exist yet would give it away
                if dex_id.deployer != preflight.signer_id {
                    preflight.ensure_dex_exists(&dex_id)?;
                }
                let description = format!(
                    "{} into {dex_id}'s storage balance on {ENGINE_ACCOUNT_ID}",
                    format_near(amount)
                );
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps: vec![format!("Deposit {description}")],
                    function_calls: vec![FunctionCall {
                        method_name: "dex_storage_deposit",
                        args: json!({ "dex_id": dex_id }),
                        deposit: amount,
                        gas: ENGINE_STORAGE_DEPOSIT_GAS,
                    }],
                    expected_outcome: ExpectedOutcome {
                        action_name: "dex storage deposit",
                        success_message: format!("Deposited {description}"),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<DepositContext> for ActionContext {
    fn from(item: DepositContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = WithdrawContext)]
pub struct Withdraw {
    #[interactive_clap(skip_default_input_arg)]
    /// The deployer of the dex
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The dex as <deployer>/<name>
    dex: DexIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How much NEAR, or all that's available
    amount: NearAmountOrAllArg,
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
            "Which account deployed the dex?",
        )
    }

    fn input_dex(_context: &crate::GlobalContext) -> color_eyre::eyre::Result<Option<DexIdArg>> {
        super::input_dex_id()
    }

    fn input_amount(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<NearAmountOrAllArg>> {
        crate::inputs::prompt("How much NEAR? e.g. '0.5 NEAR', or all that's available")
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
        let DexIdArg(dex_id) = scope.dex.clone();
        let requested_amount = scope.amount;
        Ok(Self(planning::write_action_context(
            &previous_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                if dex_id.deployer != preflight.signer_id {
                    return Err(PreflightError::NotDexDeployer {
                        dex_id: dex_id.clone(),
                        deployer: dex_id.deployer.clone(),
                    }
                    .into_report());
                }
                let storage_balance = engine::dex_storage_balance_of(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &dex_id,
                )?
                .ok_or_else(|| {
                    PreflightError::NoStorageBalance {
                        owner: dex_id.to_string(),
                    }
                    .into_report()
                })?;
                let (amount, amount_arg) = match requested_amount {
                    NearAmountOrAllArg::All => {
                        if storage_balance.available.is_zero() {
                            return Err(PreflightError::NoStorageAvailable {
                                owner: dex_id.to_string(),
                            }
                            .into_report());
                        }
                        (storage_balance.available, None)
                    }
                    NearAmountOrAllArg::Amount(amount) => {
                        if amount.is_zero() {
                            return Err(PreflightError::ZeroAmount.into_report());
                        }
                        if amount > storage_balance.available {
                            return Err(PreflightError::StorageWithdrawalTooLarge {
                                owner: dex_id.to_string(),
                                available: format_near(storage_balance.available),
                                requested: format_near(amount),
                            }
                            .into_report());
                        }
                        (amount, Some(amount))
                    }
                };
                let description = format!(
                    "{} of {dex_id}'s storage balance on {ENGINE_ACCOUNT_ID} to {}'s wallet",
                    format_near(amount),
                    preflight.signer_id
                );
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps: vec![format!("Withdraw {description}")],
                    function_calls: vec![FunctionCall {
                        method_name: "dex_storage_withdraw",
                        args: json!({ "dex_id": dex_id, "amount": amount_arg }),
                        deposit: ONE_YOCTO_NEAR,
                        gas: DEX_STORAGE_WITHDRAW_GAS,
                    }],
                    expected_outcome: ExpectedOutcome {
                        action_name: "dex storage withdrawal",
                        success_message: format!("Withdrew {description}"),
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
