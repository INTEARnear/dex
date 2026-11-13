use near_cli_rs::commands::ActionContext;
use near_cli_rs::commands::account::export_account::{
    get_account_key_pair_from_keychain, get_account_key_pair_from_legacy_keychain,
};
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::gas::Gas;
use near_primitives::types::{AccountId, Finality};
use near_sdk::{NearToken, PublicKeyHandle};
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use crate::chain::asset_metadata::AssetMetadata;
use crate::chain::engine;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;
use crate::inputs::oracle_key::OracleKeyArg;
use crate::outcome::{DexEvents, ExpectedOutcome};
use crate::planning::{self, FunctionCall, Preflight, TransactionPlan, format_near};

/// Each call takes a few Tgas, and removal 5 more for its callback. What isn't used is refunded.
const ORACLE_KEY_CALL_GAS: Gas = Gas::from_teragas(30);
/// Removing a gas key burns its balance, and the protocol refuses to burn more than this
const MAX_REMOVED_GAS_BALANCE: NearToken = NearToken::from_near(1);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
pub struct OracleCommands {
    #[interactive_clap(subcommand)]
    action: OracleAction,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = crate::GlobalContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do with oracle keys?
pub enum OracleAction {
    #[strum_discriminants(strum(
        message = "list        - The keys that oracles respond with, and their gas balances"
    ))]
    /// The keys that oracles respond with, and their gas balances
    List(List),
    #[strum_discriminants(strum(
        message = "create-key  - Add a key for an oracle to respond with, paying for its storage and gas"
    ))]
    /// Add a key for an oracle to respond with, paying for its storage and gas
    CreateKey(CreateKey),
    #[strum_discriminants(strum(
        message = "top-up      - Add NEAR to the gas balance of an oracle key"
    ))]
    /// Add NEAR to the gas balance of an oracle key
    TopUp(TopUp),
    #[strum_discriminants(strum(
        message = "remove-key  - Remove an oracle key, signed with the key itself, and get back what its storage took"
    ))]
    /// Remove an oracle key, signed with the key itself, and get back what its storage took
    RemoveKey(RemoveKey),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = ListContext)]
pub struct List {
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

#[derive(Clone)]
pub struct ListContext(ArgsForViewContext);

impl ListContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        _scope: &<List as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let output_format = previous_context.output_format;
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block_reference = engine::engine_view_block(network_config, block_reference)?;
                let oracle_keys = engine::oracle_keys(network_config, &block_reference)?;
                let near = AssetMetadata::near();
                match output_format {
                    OutputFormat::Table if oracle_keys.is_empty() => {
                        println!("{ENGINE_ACCOUNT_ID} has no oracle keys");
                    }
                    OutputFormat::Table => {
                        let mut table = display::table(&["Oracle key", "Gas balance", "Nonces"]);
                        for oracle_key in &oracle_keys {
                            table.add_row(prettytable::row![
                                oracle_key.public_key,
                                display::format_amount(
                                    oracle_key.gas_balance.as_yoctonear(),
                                    Some(&near),
                                ),
                                oracle_key.nonces,
                            ]);
                        }
                        print!("{table}");
                    }
                    OutputFormat::Json => {
                        display::print_json(&json!(
                            oracle_keys
                                .iter()
                                .map(|oracle_key| json!({
                                    "oracle_key": oracle_key.public_key,
                                    "gas_balance": display::amount_json(oracle_key.gas_balance.as_yoctonear(), Some(&near)),
                                    "nonces": oracle_key.nonces,
                                }))
                                .collect::<Vec<_>>()
                        ))?;
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

impl From<ListContext> for ArgsForViewContext {
    fn from(item: ListContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = CreateKeyContext)]
pub struct CreateKey {
    /// How much NEAR? It pays for the key's storage, and the rest is its gas balance, e.g. '0.5 NEAR'
    deposit: near_cli_rs::types::near_token::NearToken,
    #[interactive_clap(subcommand)]
    oracle_key: OracleKeySource,
}

#[derive(Clone)]
pub struct CreateKeyContext(OracleKeyCallContext);

impl CreateKeyContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<CreateKey as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let near_cli_rs::types::near_token::NearToken(deposit) = scope.deposit;
        Ok(Self(OracleKeyCallContext {
            global_context: previous_context,
            call: OracleKeyCall::Create,
            deposit,
        }))
    }
}

impl From<CreateKeyContext> for OracleKeyCallContext {
    fn from(item: CreateKeyContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = TopUpContext)]
pub struct TopUp {
    /// How much NEAR to add to its gas balance? e.g. '0.5 NEAR'
    amount: near_cli_rs::types::near_token::NearToken,
    #[interactive_clap(subcommand)]
    oracle_key: OracleKeySource,
}

#[derive(Clone)]
pub struct TopUpContext(OracleKeyCallContext);

impl TopUpContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<TopUp as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let near_cli_rs::types::near_token::NearToken(amount) = scope.amount;
        Ok(Self(OracleKeyCallContext {
            global_context: previous_context,
            call: OracleKeyCall::TopUp,
            deposit: amount,
        }))
    }
}

impl From<TopUpContext> for OracleKeyCallContext {
    fn from(item: TopUpContext) -> Self {
        item.0
    }
}

#[derive(Clone, Copy)]
enum OracleKeyCall {
    Create,
    TopUp,
}

#[derive(Clone)]
pub struct OracleKeyCallContext {
    global_context: crate::GlobalContext,
    call: OracleKeyCall,
    deposit: NearToken,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = OracleKeyCallContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// Which key?
pub enum OracleKeySource {
    #[strum_discriminants(strum(
        message = "public-key    - A public key that you give, e.g. of a key the oracle generated"
    ))]
    /// A public key that you give, e.g. of a key the oracle generated
    PublicKey(GivenPublicKey),
    #[strum_discriminants(strum(
        message = "from-account  - The key that near-cli has of the account that pays, which the oracle then runs with"
    ))]
    /// The key that near-cli has of the account that pays, which the oracle then runs with
    FromAccount(KeyOfAccount),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = OracleKeyCallContext)]
#[interactive_clap(output_context = GivenPublicKeyContext)]
pub struct GivenPublicKey {
    #[interactive_clap(skip_default_input_arg)]
    /// The public key, e.g. 'ed25519:…'
    oracle_key: OracleKeyArg,
    #[interactive_clap(skip_default_input_arg)]
    /// Which account pays?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl GivenPublicKey {
    fn input_oracle_key(
        _context: &OracleKeyCallContext,
    ) -> color_eyre::eyre::Result<Option<OracleKeyArg>> {
        crate::inputs::prompt("The public key? e.g. ed25519:…")
    }

    fn input_signer_account_id(
        context: &OracleKeyCallContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account pays?",
        )
    }
}

#[derive(Clone)]
pub struct GivenPublicKeyContext(ActionContext);

impl GivenPublicKeyContext {
    pub fn from_previous_context(
        previous_context: OracleKeyCallContext,
        scope: &<GivenPublicKey as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let OracleKeyArg(oracle_key) = scope.oracle_key.clone();
        Ok(Self(oracle_key_call_action_context(
            previous_context,
            scope.signer_account_id.clone().into(),
            move |_| Ok(oracle_key.clone()),
        )))
    }
}

impl From<GivenPublicKeyContext> for ActionContext {
    fn from(item: GivenPublicKeyContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = OracleKeyCallContext)]
#[interactive_clap(output_context = KeyOfAccountContext)]
pub struct KeyOfAccount {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account pays, with its key that near-cli has?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl KeyOfAccount {
    fn input_signer_account_id(
        context: &OracleKeyCallContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.global_context.config.credentials_home_dir,
            "Which account pays, with its key that near-cli has?",
        )
    }
}

#[derive(Clone)]
pub struct KeyOfAccountContext(ActionContext);

impl KeyOfAccountContext {
    pub fn from_previous_context(
        previous_context: OracleKeyCallContext,
        scope: &<KeyOfAccount as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let credentials_home_dir = previous_context
            .global_context
            .config
            .credentials_home_dir
            .clone();
        Ok(Self(oracle_key_call_action_context(
            previous_context,
            scope.signer_account_id.clone().into(),
            move |preflight| {
                // Where near-cli looks for it to sign with: its keychain, then the key files of
                // its legacy keychain
                let key_pair = get_account_key_pair_from_keychain(
                    &preflight.network_config,
                    &preflight.signer_id,
                )
                .or_else(|_| {
                    get_account_key_pair_from_legacy_keychain(
                        &preflight.network_config,
                        &preflight.signer_id,
                        &credentials_home_dir,
                    )
                })
                .map_err(|_| {
                    PreflightError::NoKeyInNearCli {
                        account_id: preflight.signer_id.clone(),
                        network_name: preflight.network_config.network_name.clone(),
                    }
                    .into_report()
                })?;
                Ok(key_pair.public_key.to_string().parse()?)
            },
        )))
    }
}

impl From<KeyOfAccountContext> for ActionContext {
    fn from(item: KeyOfAccountContext) -> Self {
        item.0
    }
}

fn oracle_key_call_action_context(
    OracleKeyCallContext {
        global_context,
        call,
        deposit,
    }: OracleKeyCallContext,
    signer_id: AccountId,
    oracle_key_of: impl Fn(&Preflight) -> color_eyre::eyre::Result<PublicKeyHandle> + 'static,
) -> ActionContext {
    planning::write_action_context(&global_context, signer_id, move |preflight| {
        preflight.ensure_engine_not_paused()?;
        if deposit.is_zero() {
            return Err(PreflightError::ZeroAmount.into_report());
        }
        let oracle_key = oracle_key_of(preflight)?;
        let is_oracle_key =
            engine::oracle_keys(&preflight.network_config, &preflight.block_reference)?
                .iter()
                .any(|existing| existing.public_key.to_string() == oracle_key.to_string());
        let (method_name, step, action_name, success_message) = match call {
            OracleKeyCall::Create => {
                if is_oracle_key {
                    return Err(PreflightError::OracleKeyExists {
                        oracle_key: oracle_key.to_string(),
                    }
                    .into_report());
                }
                (
                    "create_oracle_key",
                    format!(
                        "Add {oracle_key} as an oracle key of {ENGINE_ACCOUNT_ID} that amp pools can ask for prices, with {} for its storage and gas",
                        format_near(deposit)
                    ),
                    "oracle key creation",
                    format!("Added the oracle key {oracle_key} to {ENGINE_ACCOUNT_ID}"),
                )
            }
            OracleKeyCall::TopUp => {
                if !is_oracle_key {
                    return Err(PreflightError::NotAnOracleKey {
                        oracle_key: oracle_key.to_string(),
                    }
                    .into_report());
                }
                (
                    "top_up_oracle_key",
                    format!(
                        "Add {} to the gas balance of the oracle key {oracle_key}",
                        format_near(deposit)
                    ),
                    "oracle key top-up",
                    format!(
                        "Added {} to the gas balance of the oracle key {oracle_key}",
                        format_near(deposit)
                    ),
                )
            }
        };
        Ok(TransactionPlan {
            receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
            steps: vec![step],
            function_calls: vec![FunctionCall {
                method_name,
                args: json!({ "oracle_key": oracle_key }).into(),
                deposit,
                gas: ORACLE_KEY_CALL_GAS,
            }],
            expected_outcome: ExpectedOutcome {
                action_name,
                success_message,
                transfer_call: None,
                dex_id: None,
                dex_events: DexEvents::NotRead,
                deployment: None,
            },
            deploy_code: None,
        })
    })
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = RemoveKeyContext)]
pub struct RemoveKey {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account gets back the NEAR that paid for the key's storage?
    receiver_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl RemoveKey {
    fn input_receiver_account_id(
        context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_non_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "Which account gets back the NEAR that paid for the key's storage?",
        )
    }
}

#[derive(Clone)]
pub struct RemoveKeyContext(ActionContext);

impl RemoveKeyContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<RemoveKey as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let receiver_id: AccountId = scope.receiver_account_id.clone().into();
        // The engine removes the key that signs, which is a key of the engine's own account
        let mut action_context = planning::write_action_context(
            &previous_context,
            ENGINE_ACCOUNT_ID.to_owned(),
            move |preflight| {
                preflight.ensure_account_exists(&receiver_id)?;
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps: vec![format!(
                        "Remove the oracle key that signs this transaction as {ENGINE_ACCOUNT_ID}, burning its gas balance, which can't be more than {}, and send the NEAR that paid for its storage to {receiver_id}",
                        format_near(MAX_REMOVED_GAS_BALANCE)
                    )],
                    function_calls: vec![FunctionCall {
                        method_name: "remove_oracle_key",
                        args: json!({ "receiver_id": receiver_id }).into(),
                        deposit: NearToken::ZERO,
                        gas: ORACLE_KEY_CALL_GAS,
                    }],
                    expected_outcome: ExpectedOutcome {
                        action_name: "oracle key removal",
                        success_message: format!(
                            "Removed the oracle key, and sent the NEAR that paid for its storage to {receiver_id}"
                        ),
                        transfer_call: None,
                        dex_id: None,
                        dex_events: DexEvents::NotRead,
                        deployment: None,
                    },
                    deploy_code: None,
                })
            },
        );
        // Each way near-cli signs here calls this with the key it's about to sign with, except
        // sign-later, which only prints the transaction for whoever signs it
        action_context.on_before_signing_callback =
            std::sync::Arc::new(|unsigned_transaction, network_config| {
                let block_reference =
                    engine::engine_view_block(network_config, &Finality::Final.into())?;
                ensure_signer_is_oracle_key(
                    &unsigned_transaction.public_key,
                    &engine::oracle_keys(network_config, &block_reference)?,
                )
            });
        Ok(Self(action_context))
    }
}

/// `remove_oracle_key` deletes whichever key signs, so signing it with any
/// other key of the engine's account, such as a full-access one, deletes that key
fn ensure_signer_is_oracle_key(
    signer_public_key: &near_crypto::PublicKey,
    oracle_keys: &[engine::OracleKey],
) -> color_eyre::eyre::Result<()> {
    let signer_public_key = signer_public_key.to_string();
    if oracle_keys
        .iter()
        .any(|oracle_key| oracle_key.public_key.to_string() == signer_public_key)
    {
        Ok(())
    } else {
        Err(PreflightError::SignerNotAnOracleKey { signer_public_key }.into_report())
    }
}

impl From<RemoveKeyContext> for ActionContext {
    fn from(item: RemoveKeyContext) -> Self {
        item.0
    }
}

#[cfg(test)]
mod tests {
    use near_crypto::{KeyType, SecretKey};

    use super::*;

    fn oracle_key_of(secret_key: &SecretKey) -> engine::OracleKey {
        engine::OracleKey {
            public_key: secret_key.public_key().into(),
            gas_balance: NearToken::from_near(1),
            nonces: 4,
        }
    }

    #[test]
    fn only_an_oracle_key_may_sign_its_removal() {
        let oracle_secret_key = SecretKey::from_random(KeyType::ED25519);
        let full_access_secret_key = SecretKey::from_random(KeyType::ED25519);
        let oracle_keys = [oracle_key_of(&oracle_secret_key)];

        ensure_signer_is_oracle_key(&oracle_secret_key.public_key(), &oracle_keys).unwrap();
        let refusal =
            ensure_signer_is_oracle_key(&full_access_secret_key.public_key(), &oracle_keys)
                .unwrap_err();
        assert!(
            refusal
                .to_string()
                .contains(&full_access_secret_key.public_key().to_string()),
            "{refusal}"
        );
        assert!(ensure_signer_is_oracle_key(&oracle_secret_key.public_key(), &[]).is_err());
    }
}
