use intear_dex_types::{AccountOrDexId, AssetId};
use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::NearToken;
use near_sdk::json_types::U128;
use serde_json::json;

use crate::chain::asset_metadata::AssetMetadataCache;
use crate::chain::{engine, fungible_token};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display;
use crate::errors::PreflightError;
use crate::inputs::amount::AmountArg;
use crate::inputs::asset_ids::AssetIdArg;
use crate::outcome::{ExpectedOutcome, ExpectedTransferCall};
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan, format_near};

pub const DEPOSIT_NEAR_GAS: Gas = Gas::from_teragas(10);
const TOKEN_STORAGE_DEPOSIT_GAS: Gas = Gas::from_teragas(10);
const FT_TRANSFER_CALL_GAS: Gas = Gas::from_teragas(50);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = DepositContext)]
pub struct Deposit {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account deposits from its wallet?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The asset: near or nep141:<token contract>
    asset: AssetIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How much, e.g. '10 NEAR', '25.5 USDT' or '1000000 raw'
    amount: AmountArg,
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
            "Which account deposits from its wallet?",
        )
    }

    fn input_asset(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<AssetIdArg>> {
        crate::inputs::prompt("Which asset? near or nep141:<token contract>")
    }

    fn input_amount(
        _context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<AmountArg>> {
        crate::inputs::prompt(
            "How much? With the asset's symbol, e.g. '10 NEAR' or '25.5 USDT', or '1000000 raw'",
        )
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
        let AssetIdArg(asset_id) = scope.asset.clone();
        let amount = scope.amount.clone();
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
                let (_, raw_amount) = amount.resolve(&[(asset_id.clone(), metadata.clone())])?;
                if raw_amount == 0 {
                    return Err(PreflightError::ZeroAmount.into_report());
                }
                let amount_label = display::format_amount(raw_amount, metadata.as_ref());
                let registered_for_signer = engine::asset_balance_of(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &AccountOrDexId::Account(signer_id.clone()),
                    &asset_id,
                )?
                .is_some();
                let success_message = format!(
                    "Deposited {amount_label} into {signer_id}'s balance on {ENGINE_ACCOUNT_ID}"
                );
                match &asset_id {
                    AssetId::Near => {
                        let mut steps = Vec::new();
                        let mut function_calls = Vec::new();
                        // Same contract as deposit_near, so the registration fits in this transaction
                        if !registered_for_signer {
                            let registration_storage = preflight.storage_for_registrations(&[(
                                AccountOrDexId::Account(signer_id.clone()),
                                AssetId::Near,
                            )])?;
                            if let Some(storage_top_up) = &registration_storage.top_up {
                                steps.push(storage_top_up.step(&signer_id));
                                function_calls.push(storage_top_up.function_call());
                            }
                            steps.push(format!(
                                "Register near for {signer_id}, which takes about {} of its storage balance",
                                format_near(registration_storage.needed)
                            ));
                            function_calls.push(FunctionCall {
                                method_name: "register_assets",
                                args: json!({ "asset_ids": [AssetId::Near] }),
                                deposit: ONE_YOCTO_NEAR,
                                gas: planning::register_assets_gas(1)?,
                            });
                        }
                        steps.push(format!(
                            "Deposit {amount_label} into {signer_id}'s balance on {ENGINE_ACCOUNT_ID}"
                        ));
                        function_calls.push(FunctionCall {
                            method_name: "deposit_near",
                            args: json!({}),
                            deposit: NearToken::from_yoctonear(raw_amount),
                            gas: DEPOSIT_NEAR_GAS,
                        });
                        Ok(TransactionPlan {
                            receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                            steps,
                            function_calls,
                            expected_outcome: ExpectedOutcome {
                                action_name: "deposit",
                                success_message,
                                transfer_call: None,
                                dex_id: None,
                                deployment: None,
                            },
                        })
                    }
                    AssetId::Nep141(token_id) => {
                        // ft_transfer_call goes to the token contract, so a
                        // missing registration on the engine can't be added
                        if !registered_for_signer {
                            return Err(PreflightError::AssetNotRegisteredForSigner {
                                signer_id: signer_id.clone(),
                                asset_label: display::asset_label(&asset_id, metadata.as_ref()),
                                register_command: shell_words::join([
                                    "near",
                                    "intear-dex-management",
                                    "engine",
                                    "assets",
                                    "register",
                                    signer_id.as_str(),
                                    &asset_id.to_string(),
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
                        if wallet_balance < raw_amount {
                            return Err(PreflightError::InsufficientTokenBalance {
                                account_id: signer_id.clone(),
                                available: display::format_amount(
                                    wallet_balance,
                                    metadata.as_ref(),
                                ),
                                needed: amount_label,
                            }
                            .into_report());
                        }
                        let mut steps = Vec::new();
                        let mut function_calls = Vec::new();
                        if !fungible_token::is_registered(
                            &preflight.network_config,
                            &preflight.block_reference,
                            token_id,
                            &ENGINE_ACCOUNT_ID.to_owned(),
                        )? {
                            let minimum_deposit = fungible_token::minimum_storage_deposit(
                                &preflight.network_config,
                                &preflight.block_reference,
                                token_id,
                            )?;
                            steps.push(format!(
                                "Pay {} for {ENGINE_ACCOUNT_ID}'s storage on {token_id}, which it needs to receive the tokens",
                                format_near(minimum_deposit)
                            ));
                            function_calls.push(FunctionCall {
                                method_name: "storage_deposit",
                                args: json!({ "account_id": ENGINE_ACCOUNT_ID, "registration_only": true }),
                                deposit: minimum_deposit,
                                gas: TOKEN_STORAGE_DEPOSIT_GAS,
                            });
                        }
                        steps.push(format!(
                            "Send {amount_label} to {ENGINE_ACCOUNT_ID} with ft_transfer_call, into {signer_id}'s balance there"
                        ));
                        function_calls.push(FunctionCall {
                            method_name: "ft_transfer_call",
                            args: json!({
                                "receiver_id": ENGINE_ACCOUNT_ID,
                                "amount": U128(raw_amount),
                                "msg": "",
                            }),
                            deposit: ONE_YOCTO_NEAR,
                            gas: FT_TRANSFER_CALL_GAS,
                        });
                        Ok(TransactionPlan {
                            receiver_id: token_id.clone(),
                            steps,
                            function_calls,
                            expected_outcome: ExpectedOutcome {
                                action_name: "deposit",
                                success_message,
                                transfer_call: Some(ExpectedTransferCall {
                                    token_id: token_id.clone(),
                                    sender_id: signer_id,
                                    amount: raw_amount,
                                    metadata,
                                }),
                                dex_id: None,
                                deployment: None,
                            },
                        })
                    }
                    AssetId::Nep171(_, _) | AssetId::Nep245(_, _) => {
                        Err(PreflightError::UnsupportedDeposit {
                            asset_id: asset_id.to_string(),
                        }
                        .into_report())
                    }
                }
            },
        )))
    }
}

impl From<DepositContext> for ActionContext {
    fn from(item: DepositContext) -> Self {
        item.0
    }
}
