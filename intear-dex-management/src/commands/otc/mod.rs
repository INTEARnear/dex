use color_eyre::eyre::eyre;
use intear_dex_types::{AccountOrDexId, AssetId, DexId};
use near_primitives::gas::Gas;
use near_sdk::NearToken;
use otc_dex_types::StorageDepositArgs;
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use crate::deployment::{DEFAULT_OTC_DEX_DEPLOYER, DEFAULT_OTC_DEX_NAME};
use crate::inputs::dex_id::DexIdArg;
use crate::planning::{
    self, FunctionCall, ONE_YOCTO_NEAR, Preflight, format_near, otc_storage, storage,
};

pub mod account;
pub mod authorized_key;
pub mod deploy;
pub mod deposit;
pub mod nonce;
pub mod register;
pub mod storage_deposit;
pub mod withdraw;

/// A call of an OTC method, which runs in the engine's interpreter
const OTC_CALL_GAS: Gas = Gas::from_teragas(50);
const OTC_STORAGE_DEPOSIT_GAS: Gas = Gas::from_teragas(30);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = OtcContext)]
pub struct OtcCommands {
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// The OTC dex as <deployer>/<name> (default: slimedragon.near/otc)
    dex: Option<DexIdArg>,
    #[interactive_clap(subcommand)]
    action: OtcAction,
}

#[derive(Clone)]
pub struct OtcContext {
    pub global_context: crate::GlobalContext,
    pub dex_id: DexId,
    /// Whether --dex chose the dex, which deploy doesn't take
    pub is_dex_chosen: bool,
}

impl OtcContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<OtcCommands as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = match &scope.dex {
            Some(DexIdArg(dex_id)) => dex_id.clone(),
            None => DexId {
                deployer: DEFAULT_OTC_DEX_DEPLOYER.to_owned(),
                id: DEFAULT_OTC_DEX_NAME.to_string(),
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
#[interactive_clap(context = OtcContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do?
pub enum OtcAction {
    #[strum_discriminants(strum(
        message = "account             - An account on the dex: storage balance, authorized key and balances"
    ))]
    /// An account on the dex: storage balance, authorized key and balances
    Account(self::account::Account),
    #[strum_discriminants(strum(
        message = "nonce-used          - Whether an account's signed intents already used a nonce"
    ))]
    /// Whether an account's signed intents already used a nonce
    NonceUsed(self::nonce::NonceUsed),
    #[strum_discriminants(strum(
        message = "storage-deposit     - Pay for the signer's records on the dex, from its wallet"
    ))]
    /// Pay for the signer's records on the dex, from its wallet
    StorageDeposit(self::storage_deposit::StorageDeposit),
    #[strum_discriminants(strum(
        message = "set-authorized-key  - Set the key whose signatures authorize the signer's trade intents"
    ))]
    /// Set the key whose signatures authorize the signer's trade intents
    SetAuthorizedKey(self::authorized_key::SetAuthorizedKey),
    #[strum_discriminants(strum(
        message = "register-assets     - Register assets for the signer, so that trades can pay it in them"
    ))]
    /// Register assets for the signer, so that trades can pay it in them
    RegisterAssets(self::register::RegisterAssets),
    #[strum_discriminants(strum(
        message = "deposit             - Move assets from the signer's balance on dex.intear.near to the dex"
    ))]
    /// Move assets from the signer's balance on dex.intear.near to the dex
    Deposit(self::deposit::Deposit),
    #[strum_discriminants(strum(
        message = "withdraw            - Withdraw an asset from the dex to a wallet or a balance on dex.intear.near"
    ))]
    /// Withdraw an asset from the dex to a wallet or a balance on dex.intear.near
    Withdraw(self::withdraw::Withdraw),
    #[strum_discriminants(strum(
        message = "deploy              - Deploy otc code as <signer>/<name>"
    ))]
    /// Deploy otc code as <signer>/<name>
    Deploy(self::deploy::Deploy),
}

fn storage_deposit_operation(
    dex_id: &DexId,
    deposit: NearToken,
) -> color_eyre::eyre::Result<serde_json::Value> {
    planning::dex_call_operation(
        dex_id,
        "storage_deposit",
        &StorageDepositArgs,
        &[(AssetId::Near, deposit.as_yoctonear())],
    )
}

/// The steps and calls that deposit `deposit` from the signer's wallet into
/// its storage balance on the dex, then make `operations` after
/// `registrations`. The NEAR goes with the transaction into the signer's
/// balance on the engine, and from there to the dex, which turns it into its
/// own storage balance on the engine.
fn with_storage_deposit(
    preflight: &Preflight,
    dex_id: &DexId,
    deposit: NearToken,
    purpose: &str,
    registrations: &[(AccountOrDexId, AssetId)],
    operations: Vec<serde_json::Value>,
    gas: Gas,
) -> color_eyre::eyre::Result<(Vec<String>, Vec<FunctionCall>)> {
    let mut all_registrations =
        planning::storage_near_registrations(&preflight.signer_id, dex_id).to_vec();
    all_registrations.extend_from_slice(registrations);
    let registration_calls = preflight.registration_calls(&all_registrations)?;
    let mut steps = registration_calls.steps;
    let mut function_calls = registration_calls.function_calls;
    steps.push(format!(
        "Deposit {} from {}'s wallet into its storage balance on {dex_id}, which then covers {purpose}",
        format_near(deposit),
        preflight.signer_id
    ));
    let mut all_operations = vec![storage_deposit_operation(dex_id, deposit)?];
    all_operations.extend(operations);
    function_calls.push(FunctionCall {
        method_name: "execute_operations",
        args: json!({ "operations": all_operations }),
        deposit,
        gas: gas
            .checked_add(OTC_STORAGE_DEPOSIT_GAS)
            .ok_or_else(|| eyre!("Gas overflow"))?,
    });
    Ok((steps, function_calls))
}

/// The steps and calls of an OTC method that stores `new_bytes` for the
/// signer, after a deposit into the signer's storage balance on the dex when
/// what's available there doesn't cover them. `registrations` are what the
/// method needs registered on the engine.
pub fn otc_call_paying_for_storage(
    preflight: &Preflight,
    dex_id: &DexId,
    action_step: String,
    (method, args): (&str, &impl near_sdk::borsh::BorshSerialize),
    attached_assets: &[(AssetId, u128)],
    registrations: &[(AccountOrDexId, AssetId)],
    new_bytes: u64,
) -> color_eyre::eyre::Result<(Vec<String>, Vec<FunctionCall>)> {
    let operation = planning::dex_call_operation(dex_id, method, args, attached_assets)?;
    let storage_balance = crate::chain::otc::storage_balance_of(
        &preflight.network_config,
        &preflight.block_reference,
        dex_id,
        &preflight.signer_id,
    )?;
    // A first deposit pays for the storage balance's own record
    let storage_balance_record_bytes = match &storage_balance {
        Some(_) => 0,
        None => otc_storage::storage_balance_bytes(
            dex_id,
            &preflight.signer_id,
            preflight.protocol_limits.extra_bytes_per_record,
        )?,
    };
    let needed = storage::storage_cost(
        new_bytes
            .checked_add(storage_balance_record_bytes)
            .ok_or_else(|| eyre!("Storage size overflow"))?,
        preflight.protocol_limits.storage_byte_cost,
    )?;
    let available = storage_balance.map_or(NearToken::ZERO, |storage_balance| {
        storage_balance.total.saturating_sub(storage_balance.used)
    });
    match storage::storage_deposit_for(needed, available, NearToken::ZERO) {
        Some(deposit) => {
            let (mut steps, function_calls) = with_storage_deposit(
                preflight,
                dex_id,
                deposit,
                "what this stores",
                registrations,
                vec![operation],
                OTC_CALL_GAS,
            )?;
            steps.push(action_step);
            Ok((steps, function_calls))
        }
        None => {
            let registration_calls = preflight.registration_calls(registrations)?;
            let mut steps = registration_calls.steps;
            let mut function_calls = registration_calls.function_calls;
            steps.push(action_step);
            function_calls.push(FunctionCall {
                method_name: "execute_operations",
                args: json!({ "operations": [operation] }),
                deposit: ONE_YOCTO_NEAR,
                gas: OTC_CALL_GAS,
            });
            Ok((steps, function_calls))
        }
    }
}
