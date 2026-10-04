use std::collections::BTreeMap;

use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::json_types::{Base64VecU8, U128};
use serde_json::json;

use crate::chain::asset_metadata::AssetMetadataCache;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display;
use crate::errors::PreflightError;
use crate::inputs::asset_amounts::AssetAmountsArg;
use crate::inputs::dex_id::DexIdArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::{self, FunctionCall, ONE_YOCTO_NEAR, TransactionPlan};

/// The code of an arbitrary dex is unknown, so its calls get a generous
/// default that `--prepaid-gas` overrides
const DEFAULT_DEX_CALL_GAS: Gas = Gas::from_teragas(100);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = CallContext)]
pub struct Call {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account calls the dex?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The dex as <deployer>/<name>
    dex: DexIdArg,
    /// The method
    method: String,
    /// The arguments in base64, encoded as the dex reads them (empty for none)
    args: near_cli_rs::types::base64_bytes::Base64Bytes,
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// Assets to attach from the signer's balance on dex.intear.near, e.g. 'near=1 NEAR,nep141:usdt.tether-token.near=25 USDt'
    attach: Option<AssetAmountsArg>,
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// Gas for the call, e.g. '150 Tgas' (default 100 Tgas)
    prepaid_gas: Option<near_cli_rs::common::NearGas>,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl Call {
    fn input_signer_account_id(
        context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "Which account calls the dex?",
        )
    }

    fn input_dex(_context: &crate::GlobalContext) -> color_eyre::eyre::Result<Option<DexIdArg>> {
        super::input_dex_id()
    }
}

#[derive(Clone)]
pub struct CallContext(ActionContext);

impl CallContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Call as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let signer_id: AccountId = scope.signer_account_id.clone().into();
        let DexIdArg(dex_id) = scope.dex.clone();
        let method = scope.method.clone();
        let args = scope.args.clone().into_bytes();
        let attached_amounts = scope
            .attach
            .clone()
            .map_or_else(Vec::new, |AssetAmountsArg(asset_amounts)| asset_amounts);
        let gas = scope
            .prepaid_gas
            .map_or(DEFAULT_DEX_CALL_GAS, |prepaid_gas| {
                Gas::from_gas(prepaid_gas.as_gas())
            });
        Ok(Self(planning::write_action_context(
            &previous_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                if method == "swap" {
                    return Err(PreflightError::ReservedDexMethod.into_report());
                }
                preflight.ensure_dex_exists(&dex_id)?;
                let signer_id = preflight.signer_id.clone();
                let metadata_cache = AssetMetadataCache::new(
                    &preflight.network_config,
                    preflight.block_reference.clone(),
                );
                let mut attached_assets = BTreeMap::new();
                let mut attached_labels = Vec::new();
                for (asset_id, amount) in &attached_amounts {
                    let metadata = metadata_cache.get(asset_id)?;
                    let (_, raw_amount) =
                        amount.resolve(&[(asset_id.clone(), metadata.clone())])?;
                    if raw_amount == 0 {
                        return Err(PreflightError::ZeroAmount.into_report());
                    }
                    preflight.ensure_engine_balance(asset_id, metadata.as_ref(), raw_amount)?;
                    attached_labels.push(display::format_amount(raw_amount, metadata.as_ref()));
                    attached_assets.insert(asset_id.clone(), U128(raw_amount));
                }
                let mut steps = vec![format!(
                    "Call {method} on {dex_id} with {} bytes of arguments",
                    args.len()
                )];
                if !attached_labels.is_empty() {
                    steps.push(format!(
                        "Attach {} from {signer_id}'s balance on {ENGINE_ACCOUNT_ID}",
                        attached_labels.join(" and ")
                    ));
                }
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls: vec![FunctionCall {
                        method_name: "dex_call",
                        args: json!({
                            "dex_id": dex_id,
                            "method": method,
                            "args": Base64VecU8(args.clone()),
                            "attached_assets": attached_assets,
                            "referrer": null,
                        }),
                        deposit: ONE_YOCTO_NEAR,
                        gas,
                    }],
                    expected_outcome: ExpectedOutcome {
                        action_name: "dex call",
                        success_message: format!("Called {method} on {dex_id}"),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<CallContext> for ActionContext {
    fn from(item: CallContext) -> Self {
        item.0
    }
}
