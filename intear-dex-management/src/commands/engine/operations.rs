use std::io::Read;

use color_eyre::Section;
use color_eyre::eyre::{Context, eyre};
use intear_dex_types::{
    AccountOrDexId, AssetId, DexId, Operation, SwapOperationAmount, SwapRequestAmount,
    WithdrawAmount,
};
use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::json_types::U128;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::engine;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display;
use crate::errors::PreflightError;
use crate::inputs::account_or_dex_id::AccountOrDexIdArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan, format_near};

/// What operations need can't be known ahead, so batches get a generous
/// default that `--prepaid-gas` overrides
const DEFAULT_OPERATIONS_GAS: Gas = Gas::from_teragas(100);
const STANDARD_INPUT_FILE_NAME: &str = "-";
const EXAMPLE_OPERATIONS: &str = r#"{"operations": [{"Withdraw": {"asset_id": "near", "amount": {"Exact": "1000000000000000000000000"}, "to": null, "rescue_address": null}}], "referrer": null}"#;

/// The arguments of the engine's execute_operations
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecuteOperationsArgs {
    operations: Vec<Operation>,
    #[serde(default)]
    referrer: Option<AccountId>,
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
pub struct OperationsCommands {
    #[interactive_clap(subcommand)]
    action: OperationsAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do with operations?
pub enum OperationsAction {
    #[strum_discriminants(strum(
        message = "run       - Run a batch of operations from a JSON file in one transaction"
    ))]
    /// Run a batch of operations from a JSON file in one transaction
    Run(Run),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = RunContext)]
pub struct Run {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account runs the operations?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    /// A JSON file with the arguments of execute_operations, or - for standard input
    file: near_cli_rs::types::path_buf::PathBuf,
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// NEAR to attach, which goes to the signer's balance on dex.intear.near before the operations run (default 1 yoctoNEAR)
    deposit: Option<near_cli_rs::types::near_token::NearToken>,
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// Gas for the batch, e.g. '150 Tgas' (default 100 Tgas)
    prepaid_gas: Option<near_cli_rs::common::NearGas>,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Run {
    fn input_signer_account_id(
        context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "Which account runs the operations?",
        )
    }
}

#[derive(Clone)]
pub struct RunContext(ActionContext);

impl RunContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Run as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let signer_id: AccountId = scope.signer_account_id.clone().into();
        let file_bytes = if scope.file.0.as_os_str() == STANDARD_INPUT_FILE_NAME {
            let mut standard_input = Vec::new();
            std::io::stdin()
                .read_to_end(&mut standard_input)
                .wrap_err("Couldn't read the operations from standard input")?;
            standard_input
        } else {
            scope.file.read_bytes()?
        };
        // The file is sent as written, and parsed only to check and describe it
        let ExecuteOperationsArgs {
            operations,
            referrer,
        } = serde_json::from_slice(&file_bytes)
            .map_err(|error| {
                eyre!(
                    "{} isn't valid arguments of execute_operations: {error}",
                    scope.file
                )
            })
            .with_suggestion(|| {
                format!("The arguments look like this:\n    {EXAMPLE_OPERATIONS}")
            })?;
        let execute_operations_args: serde_json::Value = serde_json::from_slice(&file_bytes)?;
        if operations.is_empty() {
            return Err(eyre!("{} has no operations", scope.file));
        }
        let deposit = scope.deposit.map_or(
            ONE_YOCTO_NEAR,
            |near_cli_rs::types::near_token::NearToken(deposit)| deposit,
        );
        if deposit.is_zero() {
            return Err(eyre!(
                "execute_operations needs a deposit of at least 1 yoctoNEAR"
            ));
        }
        let gas = scope
            .prepaid_gas
            .map_or(DEFAULT_OPERATIONS_GAS, |prepaid_gas| {
                Gas::from_gas(prepaid_gas.as_gas())
            });
        Ok(Self(planning::write_action_context(
            &previous_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                let signer_id = preflight.signer_id.clone();
                let mut referenced_accounts: Vec<&AccountId> = referrer.iter().collect();
                let mut referenced_dexes: Vec<&DexId> = Vec::new();
                for operation in &operations {
                    match operation {
                        Operation::RegisterAssets {
                            r#for: Some(AccountOrDexId::Dex(dex_id)),
                            ..
                        }
                        | Operation::StorageDeposit {
                            r#for: Some(AccountOrDexId::Dex(dex_id)),
                            ..
                        }
                        | Operation::TransferAsset {
                            to: AccountOrDexId::Dex(dex_id),
                            ..
                        }
                        | Operation::SwapSimple { dex_id, .. } => referenced_dexes.push(dex_id),
                        Operation::DexCall { dex_id, method, .. } => {
                            if method == "swap" {
                                return Err(PreflightError::ReservedDexMethod.into_report());
                            }
                            referenced_dexes.push(dex_id);
                        }
                        Operation::RegisterAssets {
                            r#for: Some(AccountOrDexId::Account(account_id)),
                            ..
                        }
                        | Operation::StorageDeposit {
                            r#for: Some(AccountOrDexId::Account(account_id)),
                            ..
                        }
                        | Operation::TransferAsset {
                            to: AccountOrDexId::Account(account_id),
                            ..
                        } => referenced_accounts.push(account_id),
                        Operation::Withdraw {
                            to, rescue_address, ..
                        } => referenced_accounts.extend(to.iter().chain(rescue_address)),
                        Operation::DeployDexCode { .. } => {
                            let trusted_code_deployer = engine::trusted_code_deployer(
                                &preflight.network_config,
                                &preflight.block_reference,
                            )?;
                            if signer_id != trusted_code_deployer {
                                return Err(PreflightError::NotTrustedCodeDeployer {
                                    signer_id,
                                    trusted_code_deployer,
                                }
                                .into_report());
                            }
                        }
                        Operation::RegisterAssets { r#for: None, .. }
                        | Operation::StorageDeposit { r#for: None, .. } => {}
                    }
                }
                referenced_accounts.sort();
                referenced_accounts.dedup();
                referenced_dexes.sort();
                referenced_dexes.dedup();
                for account_id in &referenced_accounts {
                    preflight.ensure_account_exists(account_id)?;
                }
                for dex_id in &referenced_dexes {
                    preflight.ensure_dex_exists(dex_id)?;
                }

                let metadata_cache = AssetMetadataCache::new(
                    &preflight.network_config,
                    preflight.block_reference.clone(),
                );
                let mut steps = Vec::new();
                let mut function_calls = Vec::new();
                if deposit > ONE_YOCTO_NEAR {
                    // The engine adds the deposit to the signer's NEAR balance,
                    // which needs NEAR registered
                    let registration_calls = preflight.registration_calls(&[(
                        AccountOrDexId::Account(signer_id.clone()),
                        AssetId::Near,
                    )])?;
                    steps.extend(registration_calls.steps);
                    function_calls.extend(registration_calls.function_calls);
                    steps.push(format!(
                        "Deposit {} into {signer_id}'s balance on {ENGINE_ACCOUNT_ID} before the operations",
                        format_near(deposit)
                    ));
                }
                for operation in &operations {
                    steps.push(operation_step(operation, &signer_id, &metadata_cache)?);
                }
                if let Some(referrer) = &referrer {
                    steps.push(format!("Name {referrer} as the referrer of the swaps"));
                }
                function_calls.push(FunctionCall {
                    method_name: "execute_operations",
                    args: execute_operations_args.clone(),
                    deposit,
                    gas,
                });
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "operations",
                        success_message: match operations.len() {
                            1 => "Ran 1 operation".to_string(),
                            operation_count => format!("Ran {operation_count} operations"),
                        },
                        transfer_call: None,
                        dex_id: match referenced_dexes.as_slice() {
                            [only_referenced_dex] => Some((*only_referenced_dex).clone()),
                            _ => None,
                        },
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<RunContext> for ActionContext {
    fn from(item: RunContext) -> Self {
        item.0
    }
}

/// What an operation does, in a plain sentence with amounts in the assets'
/// units
fn operation_step(
    operation: &Operation,
    signer_id: &AccountId,
    metadata_cache: &AssetMetadataCache,
) -> color_eyre::eyre::Result<String> {
    let amount_label = |asset_id: &AssetId, raw_amount: u128| {
        metadata_cache
            .get(asset_id)
            .map(|metadata| display::format_amount(raw_amount, metadata.as_ref()))
    };
    let asset_label = |asset_id: &AssetId| {
        metadata_cache
            .get(asset_id)
            .map(|metadata| display::asset_label(asset_id, metadata.as_ref()))
    };
    let owner_label = |owner: &Option<AccountOrDexId>| match owner {
        Some(owner) => AccountOrDexIdArg(owner.clone()).to_string(),
        None => signer_id.to_string(),
    };
    let at_least_out = |asset_out: &AssetId, constraint: &Option<U128>| match constraint {
        Some(U128(minimum_amount_out)) => amount_label(asset_out, *minimum_amount_out)
            .map(|minimum_amount_out| format!(", getting at least {minimum_amount_out}")),
        None => Ok(String::new()),
    };
    Ok(match operation {
        Operation::RegisterAssets { asset_ids, r#for } => format!(
            "Register {} for {}",
            asset_ids
                .iter()
                .map(AssetId::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            owner_label(r#for)
        ),
        Operation::DeployDexCode {
            last_part_of_id,
            code_base64,
        } => format!(
            "Deploy {} bytes of code as dex {signer_id}/{last_part_of_id}",
            code_base64.0.len()
        ),
        Operation::Withdraw {
            asset_id,
            amount,
            to,
            rescue_address,
        } => {
            let withdrawn = match amount {
                WithdrawAmount::Full { at_least: None } => {
                    format!("all {}", asset_label(asset_id)?)
                }
                WithdrawAmount::Full {
                    at_least: Some(U128(at_least)),
                } => format!(
                    "all {}, at least {}",
                    asset_label(asset_id)?,
                    amount_label(asset_id, *at_least)?
                ),
                WithdrawAmount::Exact(U128(exact_amount)) => amount_label(asset_id, *exact_amount)?,
                WithdrawAmount::PreviousSwapOutput => {
                    format!("the {} that the previous swap made", asset_label(asset_id)?)
                }
            };
            let receiver_id = to.as_ref().unwrap_or(signer_id);
            match rescue_address {
                Some(rescue_address) => format!(
                    "Withdraw {withdrawn} from {signer_id}'s balance to {receiver_id}'s wallet, or to {rescue_address} if that fails"
                ),
                None => format!(
                    "Withdraw {withdrawn} from {signer_id}'s balance to {receiver_id}'s wallet"
                ),
            }
        }
        Operation::SwapSimple {
            dex_id,
            message: _,
            asset_in,
            asset_out,
            amount,
            constraint,
        } => match amount {
            SwapOperationAmount::Amount(SwapRequestAmount::ExactIn(U128(amount_in))) => format!(
                "Swap {} for {} on {dex_id}{}",
                amount_label(asset_in, *amount_in)?,
                asset_label(asset_out)?,
                at_least_out(asset_out, constraint)?
            ),
            SwapOperationAmount::Amount(SwapRequestAmount::ExactOut(U128(amount_out))) => {
                let at_most_in = match constraint {
                    Some(U128(maximum_amount_in)) => {
                        format!(
                            ", paying at most {}",
                            amount_label(asset_in, *maximum_amount_in)?
                        )
                    }
                    None => String::new(),
                };
                format!(
                    "Swap {} for {} on {dex_id}{at_most_in}",
                    asset_label(asset_in)?,
                    amount_label(asset_out, *amount_out)?
                )
            }
            SwapOperationAmount::OutputOfLastIn => format!(
                "Swap the {} that the previous swap made for {} on {dex_id}{}",
                asset_label(asset_in)?,
                asset_label(asset_out)?,
                at_least_out(asset_out, constraint)?
            ),
            SwapOperationAmount::EntireBalanceIn => format!(
                "Swap all of {signer_id}'s {} for {} on {dex_id}{}",
                asset_label(asset_in)?,
                asset_label(asset_out)?,
                at_least_out(asset_out, constraint)?
            ),
        },
        Operation::DexCall {
            dex_id,
            method,
            args,
            attached_assets,
        } => {
            let mut attached_amounts = attached_assets
                .iter()
                .map(|(asset_id, U128(amount))| amount_label(asset_id, *amount))
                .collect::<color_eyre::eyre::Result<Vec<_>>>()?;
            // attached_assets is a HashMap, whose order changes from run to run
            attached_amounts.sort();
            let call = format!(
                "Call {method} on {dex_id} with {} bytes of arguments",
                args.0.len()
            );
            if attached_amounts.is_empty() {
                call
            } else {
                format!("{call}, attaching {}", attached_amounts.join(" and "))
            }
        }
        Operation::TransferAsset {
            to,
            asset_id,
            amount: U128(amount),
        } => format!(
            "Transfer {} from {signer_id} to {}",
            amount_label(asset_id, *amount)?,
            AccountOrDexIdArg(to.clone())
        ),
        Operation::StorageDeposit {
            amount: U128(amount),
            r#for,
        } => format!(
            "Move {} from {signer_id}'s NEAR balance to the storage balance of {}",
            amount_label(&AssetId::Near, *amount)?,
            owner_label(r#for)
        ),
    })
}
