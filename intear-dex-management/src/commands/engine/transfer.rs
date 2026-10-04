use intear_dex_types::{AccountOrDexId, AssetId, Operation};
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
use crate::inputs::account_or_dex_id::AccountOrDexIdArg;
use crate::inputs::amount::AmountArg;
use crate::inputs::asset_ids::AssetIdArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan, format_near};

const TRANSFER_GAS: Gas = Gas::from_teragas(15);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = TransferContext)]
pub struct Transfer {
    #[interactive_clap(skip_default_input_arg)]
    /// Whose balance on dex.intear.near?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The asset: near, nep141:<token contract>, …
    asset: AssetIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How much, e.g. '10 NEAR' or '25.5 USDT'
    amount: AmountArg,
    #[interactive_clap(named_arg)]
    /// Who receives it?
    to: TransferReceiver,
}

impl Transfer {
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
    ) -> color_eyre::eyre::Result<Option<AmountArg>> {
        crate::inputs::prompt("How much? With the asset's symbol, e.g. '10 NEAR' or '25.5 USDT'")
    }
}

#[derive(Clone)]
pub struct TransferContext {
    global_context: crate::GlobalContext,
    signer_id: AccountId,
    asset_id: AssetId,
    amount: AmountArg,
}

impl TransferContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Transfer as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let AssetIdArg(asset_id) = scope.asset.clone();
        Ok(Self {
            global_context: previous_context,
            signer_id: scope.signer_account_id.clone().into(),
            asset_id,
            amount: scope.amount.clone(),
        })
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = TransferContext)]
#[interactive_clap(output_context = TransferReceiverContext)]
pub struct TransferReceiver {
    #[interactive_clap(skip_default_input_arg)]
    /// An account (bob.near) or a dex (slimedragon.near/xyk)
    receiver: AccountOrDexIdArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl TransferReceiver {
    fn input_receiver(
        _context: &TransferContext,
    ) -> color_eyre::eyre::Result<Option<AccountOrDexIdArg>> {
        crate::inputs::prompt("To whom? An account (bob.near) or a dex (slimedragon.near/xyk)")
    }
}

#[derive(Clone)]
pub struct TransferReceiverContext(ActionContext);

impl TransferReceiverContext {
    pub fn from_previous_context(
        previous_context: TransferContext,
        scope: &<TransferReceiver as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let TransferContext {
            global_context,
            signer_id,
            asset_id,
            amount,
        } = previous_context;
        let AccountOrDexIdArg(receiver) = scope.receiver.clone();
        Ok(Self(planning::write_action_context(
            &global_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                let signer_id = preflight.signer_id.clone();
                match &receiver {
                    AccountOrDexId::Account(receiver_id) if *receiver_id == signer_id => {
                        return Err(PreflightError::TransferToSelf { signer_id }.into_report());
                    }
                    AccountOrDexId::Account(receiver_id) => {
                        preflight.ensure_account_exists(receiver_id)?;
                    }
                    AccountOrDexId::Dex(dex_id) => preflight.ensure_dex_exists(dex_id)?,
                }
                let metadata = AssetMetadataCache::new(
                    &preflight.network_config,
                    preflight.block_reference.clone(),
                )
                .get(&asset_id)?;
                let (_, raw_amount) = amount.resolve(&[(asset_id.clone(), metadata.clone())])?;
                if raw_amount == 0 {
                    return Err(PreflightError::ZeroAmount.into_report());
                }
                let U128(balance) = engine::asset_balance_of(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &AccountOrDexId::Account(signer_id.clone()),
                    &asset_id,
                )?
                .ok_or_else(|| {
                    PreflightError::NoBalance {
                        owner: signer_id.to_string(),
                        asset_label: display::asset_label(&asset_id, metadata.as_ref()),
                    }
                    .into_report()
                })?;
                let amount_label = display::format_amount(raw_amount, metadata.as_ref());
                if raw_amount > balance {
                    return Err(PreflightError::InsufficientEngineBalance {
                        owner: signer_id.to_string(),
                        available: display::format_amount(balance, metadata.as_ref()),
                        needed: amount_label,
                    }
                    .into_report());
                }
                let receiver_label = AccountOrDexIdArg(receiver.clone()).to_string();
                let receiver_has_asset = engine::asset_balance_of(
                    &preflight.network_config,
                    &preflight.block_reference,
                    &receiver,
                    &asset_id,
                )?
                .is_some();
                let mut steps = Vec::new();
                let mut function_calls = Vec::new();
                let mut operations = Vec::new();
                if !receiver_has_asset {
                    let registration_storage = preflight
                        .storage_for_registrations(&[(receiver.clone(), asset_id.clone())])?;
                    if let Some(storage_top_up) = &registration_storage.top_up {
                        steps.push(storage_top_up.step(&signer_id));
                        function_calls.push(storage_top_up.function_call());
                    }
                    steps.push(format!(
                        "Register {asset_id} for {receiver_label}, which takes about {} of {signer_id}'s storage balance",
                        format_near(registration_storage.needed)
                    ));
                    operations.push(Operation::RegisterAssets {
                        asset_ids: vec![asset_id.clone()],
                        r#for: Some(receiver.clone()),
                    });
                }
                let description = format!(
                    "{amount_label} from {signer_id} to {receiver_label} on {ENGINE_ACCOUNT_ID}"
                );
                steps.push(format!("Transfer {description}"));
                operations.push(Operation::TransferAsset {
                    to: receiver.clone(),
                    asset_id: asset_id.clone(),
                    amount: U128(raw_amount),
                });
                function_calls.push(FunctionCall {
                    method_name: "execute_operations",
                    args: json!({ "operations": operations }),
                    deposit: ONE_YOCTO_NEAR,
                    gas: TRANSFER_GAS,
                });
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "transfer",
                        success_message: format!("Transferred {description}"),
                        transfer_call: None,
                        dex_id: None,
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<TransferReceiverContext> for ActionContext {
    fn from(item: TransferReceiverContext) -> Self {
        item.0
    }
}
