//! The whole migration of the engine to the layout that keeps balances by
//! owner: pause, deploy the new code with `migrate`, move every balance with
//! `migrate_balances`, `finish_migration` with the sums of storage balances,
//! unpause. It takes transactions from two accounts, so it signs with keys
//! from the keychain instead of asking how to sign each one. Running it again
//! after a failure continues where it stopped.

use std::sync::Arc;
use std::time::Duration;

use color_eyre::Section;
use color_eyre::eyre::{bail, eyre};
use intear_dex_types::{AccountOrDexId, CAN_PAUSE};
use near_cli_rs::commands::{PrepopulatedTransaction, TransactionContext};
use near_cli_rs::common::JsonRpcClientExt;
use near_cli_rs::config::NetworkConfig;
use near_cli_rs::transaction_signature_options::{
    SignedTransactionOrSignedDelegateAction, SubmitContext, send, sign_with_keychain,
    sign_with_legacy_keychain,
};
use near_jsonrpc_client::methods::block::RpcBlockRequest;
use near_primitives::gas::Gas;
use near_primitives::hash::CryptoHash;
use near_primitives::transaction::{Action, DeployContractAction, FunctionCallAction};
use near_primitives::types::{AccountId, BlockId, BlockReference, Finality};
use near_primitives::views::{FinalExecutionOutcomeView, TxExecutionStatus};
use near_sdk::NearToken;
use near_sdk::json_types::U128;
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};

use crate::chain::asset_metadata::AssetMetadata;
use crate::chain::engine_state::{self, FlatBalances, StorageBalanceSum};
use crate::chain::{account, engine, protocol};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;
use crate::outcome::{self, ExpectedOutcome, SentTransaction};
use crate::planning::{ONE_YOCTO_NEAR, format_near, storage};

const PAUSE_GAS: Gas = Gas::from_teragas(10);
/// For `migrate`, which runs after the deploy in the same receipt
const MIGRATE_GAS: Gas = Gas::from_teragas(100);
/// Moving a balance takes about a teragas, so a batch leaves room in the
/// most gas a transaction can have
const BALANCES_PER_BATCH: usize = 100;
const FINISH_MIGRATION_GAS: Gas = Gas::from_teragas(30);
/// A withdrawal that started before the pause refunds a failed transfer a
/// few blocks later, and moving that balance while the refund is in flight
/// would lose it
const BLOCKS_FOR_WITHDRAWALS_IN_FLIGHT: u64 = 20;
const FINAL_BLOCK_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// The engine's own record grows by the fields of the new layout
const ENGINE_STATE_GROWTH_BYTES: u64 = 1_000;
const WASM_MAGIC_BYTES: &[u8] = b"\0asm";

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = MigrateContext)]
pub struct Migrate {
    #[interactive_clap(skip_default_input_arg)]
    /// The account that pauses dex.intear.near for the migration and unpauses it after
    pauser_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The account that deploys dex code after the migration
    trusted_code_deployer: near_cli_rs::types::account_id::AccountId,
    /// The wasm file of the engine to migrate to
    code: near_cli_rs::types::path_buf::PathBuf,
    #[interactive_clap(subcommand)]
    signing: MigrationSigning,
}

impl Migrate {
    fn input_pauser_account_id(
        context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "Which account pauses dex.intear.near for the migration and unpauses it after?",
        )
    }

    fn input_trusted_code_deployer(
        context: &crate::GlobalContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        near_cli_rs::common::input_non_signer_account_id_from_used_account_list(
            &context.config.credentials_home_dir,
            "Which account deploys dex code after the migration?",
        )
    }
}

#[derive(Clone)]
pub struct MigrateContext {
    global_context: crate::GlobalContext,
    pauser_id: AccountId,
    trusted_code_deployer: AccountId,
    code: Vec<u8>,
}

impl MigrateContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<Migrate as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let pauser_id: AccountId = scope.pauser_account_id.clone().into();
        if !CAN_PAUSE.contains(&pauser_id.as_str()) {
            return Err(PreflightError::NotAllowedToPause {
                signer_id: pauser_id,
            }
            .into_report());
        }
        let code = scope.code.read_bytes()?;
        if !code.starts_with(WASM_MAGIC_BYTES) {
            return Err(eyre!("{} isn't a WebAssembly module", scope.code));
        }
        Ok(Self {
            global_context: previous_context,
            pauser_id,
            trusted_code_deployer: scope.trusted_code_deployer.clone().into(),
            code,
        })
    }
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = MigrateContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// Where are the keys of the pauser and of dex.intear.near?
pub enum MigrationSigning {
    #[strum_discriminants(strum(
        message = "sign-with-keychain         - Keys saved in the secure keychain, or else in the legacy keychain"
    ))]
    /// Keys saved in the secure keychain, or else in the legacy keychain
    SignWithKeychain(SignWithKeychain),
    #[strum_discriminants(strum(
        message = "sign-with-legacy-keychain  - Keys saved in the legacy keychain (compatible with the old near CLI)"
    ))]
    /// Keys saved in the legacy keychain (compatible with the old near CLI)
    SignWithLegacyKeychain(SignWithLegacyKeychain),
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = MigrateContext)]
#[interactive_clap(output_context = SignWithKeychainContext)]
pub struct SignWithKeychain {
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network::Network,
}

#[derive(Clone)]
pub struct SignWithKeychainContext(near_cli_rs::network::NetworkContext);

impl SignWithKeychainContext {
    pub fn from_previous_context(
        previous_context: MigrateContext,
        _scope: &<SignWithKeychain as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self(network_context(previous_context, Keychain::Secure)))
    }
}

impl From<SignWithKeychainContext> for near_cli_rs::network::NetworkContext {
    fn from(item: SignWithKeychainContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = MigrateContext)]
#[interactive_clap(output_context = SignWithLegacyKeychainContext)]
pub struct SignWithLegacyKeychain {
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network::Network,
}

#[derive(Clone)]
pub struct SignWithLegacyKeychainContext(near_cli_rs::network::NetworkContext);

impl SignWithLegacyKeychainContext {
    pub fn from_previous_context(
        previous_context: MigrateContext,
        _scope: &<SignWithLegacyKeychain as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self(network_context(previous_context, Keychain::Legacy)))
    }
}

impl From<SignWithLegacyKeychainContext> for near_cli_rs::network::NetworkContext {
    fn from(item: SignWithLegacyKeychainContext) -> Self {
        item.0
    }
}

fn network_context(
    migrate_context: MigrateContext,
    keychain: Keychain,
) -> near_cli_rs::network::NetworkContext {
    near_cli_rs::network::NetworkContext {
        config: migrate_context.global_context.config.clone(),
        interacting_with_account_ids: vec![
            ENGINE_ACCOUNT_ID.to_owned(),
            migrate_context.pauser_id.clone(),
        ],
        on_after_getting_network_callback: Arc::new(move |network_config| {
            migrate_engine(&migrate_context, network_config, keychain)
        }),
    }
}

#[derive(Clone, Copy)]
enum Keychain {
    Secure,
    Legacy,
}

/// Signs transactions to the engine with keys from the keychain, the way
/// near-cli-rs's sign-with-keychain and sign-with-legacy-keychain do, and
/// sends them one by one
struct Signer<'a> {
    global_context: &'a crate::GlobalContext,
    network_config: &'a NetworkConfig,
    connection_name: String,
    keychain: Keychain,
}

impl Signer<'_> {
    fn sign(
        &self,
        signer_id: &AccountId,
        actions: Vec<Action>,
    ) -> color_eyre::eyre::Result<near_primitives::transaction::SignedTransaction> {
        let transaction_context = TransactionContext {
            global_context: near_cli_rs::GlobalContext {
                config: self.global_context.config.clone(),
                offline: false,
                verbosity: self.global_context.verbosity,
            },
            network_config: self.network_config.clone(),
            prepopulated_transaction: PrepopulatedTransaction {
                signer_id: signer_id.clone(),
                receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                actions,
            },
            on_before_signing_callback: Arc::new(|_, _| Ok(())),
            on_after_signing_callback: Arc::new(|_, _| Ok(())),
            on_before_sending_transaction_callback: Arc::new(|_, _| Ok(String::new())),
            on_after_sending_transaction_callback: Arc::new(|_, _| Ok(())),
            on_sending_delegate_action_callback: None,
            sign_as_delegate_action: false,
        };
        let submit_context: SubmitContext = match self.keychain {
            Keychain::Secure => sign_with_keychain::SignKeychainContext::from_previous_context(
                transaction_context,
                &sign_with_keychain::InteractiveClapContextScopeForSignKeychain {
                    signer_public_key: None,
                    nonce: None,
                    block_hash: None,
                    block_height: None,
                    nonce_index: None,
                    meta_transaction_valid_for: None,
                },
            )?
            .into(),
            Keychain::Legacy => {
                sign_with_legacy_keychain::SignLegacyKeychainContext::from_previous_context(
                    transaction_context,
                    &sign_with_legacy_keychain::InteractiveClapContextScopeForSignLegacyKeychain {
                        signer_public_key: None,
                        nonce: None,
                        block_hash: None,
                        block_height: None,
                        nonce_index: None,
                        meta_transaction_valid_for: None,
                    },
                )?
                .into()
            }
        };
        match submit_context.signed_transaction_or_signed_delegate_action {
            SignedTransactionOrSignedDelegateAction::SignedTransaction(signed_transaction) => {
                Ok(signed_transaction)
            }
            SignedTransactionOrSignedDelegateAction::SignedDelegateAction(_) => {
                bail!("The keychain signed a delegate action instead of a transaction")
            }
        }
    }

    /// Signs one transaction to the engine with a key of `signer_id`, sends
    /// it and waits until it's final
    fn send(
        &self,
        signer_id: &AccountId,
        actions: Vec<Action>,
        expected_outcome: ExpectedOutcome,
    ) -> color_eyre::eyre::Result<FinalExecutionOutcomeView> {
        let signed_transaction = self.sign(signer_id, actions)?;
        let sent_transaction = SentTransaction {
            signer_id: signer_id.clone(),
            receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
            connection_name: self.connection_name.clone(),
            action_name: expected_outcome.action_name,
            dex_id: None,
            url: format!(
                "{}{}",
                self.network_config.explorer_transaction_url,
                signed_transaction.get_hash()
            ),
        };
        outcome::record_sent_transaction(sent_transaction.clone())?;
        tracing::info!("Transaction: {}", sent_transaction.url);
        let final_outcome = send::sending_signed_transaction(
            self.network_config,
            &signed_transaction,
            TxExecutionStatus::Final,
        )?
        .ok_or_else(|| {
            eyre!(
                "RPC {} took the transaction without waiting for its outcome",
                self.network_config.rpc_url
            )
        })?;
        outcome::report_outcome(
            &final_outcome,
            self.network_config,
            &expected_outcome,
            &sent_transaction,
            OutputFormat::Table,
            self.global_context.verbosity,
        )?;
        Ok(final_outcome)
    }
}

fn function_call(
    method_name: &str,
    args: serde_json::Value,
    deposit: NearToken,
    gas: Gas,
) -> color_eyre::eyre::Result<Action> {
    Ok(Action::FunctionCall(Box::new(FunctionCallAction {
        method_name: method_name.to_string(),
        args: serde_json::to_vec(&args)?,
        gas,
        deposit,
    })))
}

fn expected_outcome(action_name: &'static str, success_message: String) -> ExpectedOutcome {
    ExpectedOutcome {
        action_name,
        success_message,
        transfer_call: None,
        dex_id: None,
        deployment: None,
    }
}

fn final_block(network_config: &NetworkConfig) -> color_eyre::eyre::Result<BlockReference> {
    engine::engine_view_block(network_config, &Finality::Final.into())
}

fn migrate_engine(
    context: &MigrateContext,
    network_config: &NetworkConfig,
    keychain: Keychain,
) -> color_eyre::eyre::Result<()> {
    let engine_id = ENGINE_ACCOUNT_ID.to_owned();
    let connection_name = context
        .global_context
        .config
        .network_connection
        .iter()
        .find(|(_, connection)| {
            connection.rpc_url == network_config.rpc_url
                && connection.network_name == network_config.network_name
        })
        .map(|(connection_name, _)| connection_name.clone())
        .ok_or_else(|| eyre!("The chosen network connection isn't in the near-cli config"))?;
    let signer = Signer {
        global_context: &context.global_context,
        network_config,
        connection_name,
        keychain,
    };
    let code_hash = near_primitives::hash::hash(&context.code);
    let block_reference = final_block(network_config)?;
    for account_id in [&context.pauser_id, &context.trusted_code_deployer] {
        if account::view_account(network_config, &block_reference, account_id)?.is_none() {
            return Err(PreflightError::AccountMissing {
                account_id: account_id.clone(),
                network_name: network_config.network_name.clone(),
            }
            .into_report());
        }
    }
    let engine_account = account::view_account(network_config, &block_reference, &engine_id)?
        .ok_or_else(|| {
            eyre!(
                "{engine_id} doesn't exist on network {}",
                network_config.network_name
            )
        })?;
    let network = if network_config.network_name == "mainnet" {
        "MAINNET".to_string()
    } else {
        network_config.network_name.clone()
    };
    tracing::info!(
        "{network} · migrate {engine_id} to code {code_hash} ({} bytes)",
        context.code.len()
    );

    if engine_account.code_hash != code_hash {
        deploy_with_migration(context, &signer, &block_reference, code_hash)?;
    } else if engine::migration_progress(network_config, &block_reference)?.is_none() {
        tracing::info!("{engine_id} already runs this code, and its balances are migrated");
        if engine::is_paused(network_config, &block_reference)? {
            return Err(eyre!("{engine_id} is migrated, but it's still paused").suggestion(
                format!(
                    "Unpause it with `engine unpause`, signed by {}, once nothing else needs it paused",
                    CAN_PAUSE.join(" or ")
                ),
            ));
        }
        return Ok(());
    } else {
        tracing::info!(
            "{engine_id} already runs this code; continuing the migration of its balances"
        );
    }

    let moved_balances = move_balances(&signer)?;
    let (user_storage_balance_sum, dex_storage_balance_sum) = finish_migration(&signer)?;
    signer.send(
        &context.pauser_id,
        vec![function_call(
            "unpause",
            json!({}),
            ONE_YOCTO_NEAR,
            PAUSE_GAS,
        )?],
        expected_outcome("unpause", format!("Unpaused {engine_id}")),
    )?;

    let block_reference = final_block(network_config)?;
    let untracked_near = engine::untracked_near(network_config, &block_reference)?;
    let paused = engine::is_paused(network_config, &block_reference)?;
    match context.global_context.output_format {
        OutputFormat::Table => {
            let near = AssetMetadata::near();
            let near_amount = |yocto_near: u128| display::format_amount(yocto_near, Some(&near));
            let storage_balances = |sum: StorageBalanceSum| {
                format!(
                    "{} total, {} used",
                    near_amount(sum.total),
                    near_amount(sum.used)
                )
            };
            print!(
                "{}",
                display::key_value_table(vec![
                    ("Code", code_hash.to_string()),
                    (
                        "Trusted code deployer",
                        engine::trusted_code_deployer(network_config, &block_reference)?
                            .to_string()
                    ),
                    (
                        "Balances moved",
                        format!(
                            "{} of accounts, {} of dexes",
                            moved_balances.users, moved_balances.dexes
                        )
                    ),
                    (
                        "User storage balances",
                        storage_balances(user_storage_balance_sum)
                    ),
                    (
                        "Dex storage balances",
                        storage_balances(dex_storage_balance_sum)
                    ),
                    ("Untracked NEAR", format_near(untracked_near)),
                    ("Paused", if paused { "yes" } else { "no" }.to_string()),
                ])
            );
        }
        OutputFormat::Json => {
            display::print_json(&json!({
                "code_hash": code_hash.to_string(),
                "trusted_code_deployer": engine::trusted_code_deployer(network_config, &block_reference)?,
                "user_balances_moved": moved_balances.users,
                "dex_balances_moved": moved_balances.dexes,
                "user_storage_balances": {
                    "total": NearToken::from_yoctonear(user_storage_balance_sum.total),
                    "used": NearToken::from_yoctonear(user_storage_balance_sum.used),
                },
                "dex_storage_balances": {
                    "total": NearToken::from_yoctonear(dex_storage_balance_sum.total),
                    "used": NearToken::from_yoctonear(dex_storage_balance_sum.used),
                },
                "untracked_near": untracked_near,
                "paused": paused,
            }))?;
        }
    }
    Ok(())
}

/// Pauses the engine, waits for withdrawals in flight, and deploys the new
/// code with `migrate`, which counts the balances that it has to move
fn deploy_with_migration(
    context: &MigrateContext,
    signer: &Signer,
    block_reference: &BlockReference,
    code_hash: CryptoHash,
) -> color_eyre::eyre::Result<()> {
    let network_config = signer.network_config;
    let engine_id = ENGINE_ACCOUNT_ID.to_owned();

    // Only code that keeps balances by owner has migration_progress, and
    // `migrate` can't read its state, so it would fail after the pause
    let deployed_code_hash = account::view_account(network_config, block_reference, &engine_id)?
        .ok_or_else(|| eyre!("{engine_id} doesn't exist"))?
        .code_hash;
    match crate::chain::view_function_result(
        network_config,
        block_reference,
        &engine_id,
        "migration_progress",
        b"{}".to_vec(),
    )? {
        None => {}
        Some(progress) if progress.as_slice() == b"null" => bail!(
            "{engine_id} runs code {deployed_code_hash}, which already keeps balances by owner, so there's nothing to migrate"
        ),
        Some(_) => {
            return Err(eyre!(
                "{engine_id} is migrating with code {deployed_code_hash}, not this code"
            )
            .suggestion("Continue the migration with the wasm file of that code"));
        }
    }

    // Both keys must be there before anything is signed, so that a missing
    // key of the engine doesn't leave it paused
    tracing::info!(
        "Checking the keychain by signing, without sending, a transaction of {} and one of {engine_id}",
        context.pauser_id
    );
    signer.sign(&context.pauser_id, Vec::new())?;
    signer.sign(&engine_id, Vec::new())?;

    let protocol_limits = protocol::protocol_limits(network_config, block_reference)?;
    let flat_balances = engine_state::flat_balances(network_config, block_reference)?;
    let needed = storage::storage_cost(
        migration_growth_bytes(
            &flat_balances,
            context.code.len(),
            account::contract_code_bytes(network_config, block_reference, &engine_id)?,
            protocol_limits.extra_bytes_per_record,
        )?,
        protocol_limits.storage_byte_cost,
    )?;
    let untracked = untracked_near_of_old_engine(
        network_config,
        block_reference,
        protocol_limits.storage_byte_cost,
    )?;
    if untracked < i128::try_from(needed.as_yoctonear())? {
        let shortfall = i128::try_from(needed.as_yoctonear())?
            .checked_sub(untracked)
            .ok_or_else(|| eyre!("NEAR overflow"))?;
        return Err(PreflightError::NotEnoughNearForMigration {
            needed: format_near(needed),
            shortfall: format_near(NearToken::from_yoctonear(u128::try_from(shortfall)?)),
        }
        .into_report());
    }

    let pause_outcome = signer.send(
        &context.pauser_id,
        vec![function_call(
            "pause",
            json!({}),
            ONE_YOCTO_NEAR,
            PAUSE_GAS,
        )?],
        expected_outcome(
            "pause",
            format!("Paused {engine_id}, so that no balance changes while it's migrated"),
        ),
    )?;
    wait_for_withdrawals_in_flight(network_config, &pause_outcome)?;

    // No balance can be registered while the engine is paused, so these are
    // all the balances that the migration moves
    let block_reference = final_block(network_config)?;
    let flat_balances = engine_state::flat_balances(network_config, &block_reference)?;
    signer.send(
        &engine_id,
        vec![
            Action::DeployContract(DeployContractAction {
                code: context.code.clone(),
            }),
            function_call(
                "migrate",
                json!({
                    "trusted_code_deployer": context.trusted_code_deployer,
                    "user_balance_count": u32::try_from(flat_balances.users.len())?,
                    "dex_balance_count": u32::try_from(flat_balances.dexes.len())?,
                }),
                NearToken::from_yoctonear(0),
                MIGRATE_GAS,
            )?,
        ],
        expected_outcome(
            "migration",
            format!(
                "Deployed code {code_hash} to {engine_id}, which has {} balances of accounts and {} of dexes to move",
                flat_balances.users.len(),
                flat_balances.dexes.len()
            ),
        ),
    )?;
    let deployed_code_hash =
        account::view_account(network_config, &final_block(network_config)?, &engine_id)?
            .ok_or_else(|| eyre!("{engine_id} is gone"))?
            .code_hash;
    if deployed_code_hash != code_hash {
        bail!("{engine_id} runs code {deployed_code_hash} after the deploy, not {code_hash}");
    }
    Ok(())
}

/// Waits until the engine's state can't change anymore after the pause,
/// some blocks after the one where the pause took effect
fn wait_for_withdrawals_in_flight(
    network_config: &NetworkConfig,
    pause_outcome: &FinalExecutionOutcomeView,
) -> color_eyre::eyre::Result<()> {
    let mut paused_at_height = 0;
    for receipt_outcome in &pause_outcome.receipts_outcome {
        let block = network_config
            .json_rpc_client()
            .blocking_call(RpcBlockRequest {
                block_reference: BlockReference::BlockId(BlockId::Hash(receipt_outcome.block_hash)),
            })
            .map_err(color_eyre::eyre::Report::new)?;
        paused_at_height = paused_at_height.max(block.header.height);
    }
    let waited_for_height = paused_at_height
        .checked_add(BLOCKS_FOR_WITHDRAWALS_IN_FLIGHT)
        .ok_or_else(|| eyre!("Block height overflow"))?;
    tracing::info!(
        "Waiting until block {waited_for_height} is final, {BLOCKS_FOR_WITHDRAWALS_IN_FLIGHT} blocks after the pause, so that withdrawals in flight finish"
    );
    loop {
        let final_height = network_config
            .json_rpc_client()
            .blocking_call(RpcBlockRequest {
                block_reference: Finality::Final.into(),
            })
            .map_err(color_eyre::eyre::Report::new)?
            .header
            .height;
        if final_height >= waited_for_height {
            return Ok(());
        }
        std::thread::sleep(FINAL_BLOCK_POLL_INTERVAL);
    }
}

struct MovedBalances {
    users: usize,
    dexes: usize,
}

/// Moves every balance that's left in the old layout, in batches, and checks
/// that none is left
fn move_balances(signer: &Signer) -> color_eyre::eyre::Result<MovedBalances> {
    let network_config = signer.network_config;
    let engine_id = ENGINE_ACCOUNT_ID.to_owned();
    let block_reference = final_block(network_config)?;
    let progress = engine::migration_progress(network_config, &block_reference)?
        .ok_or_else(|| eyre!("{engine_id} isn't migrating"))?;
    let FlatBalances { users, dexes } =
        engine_state::flat_balances(network_config, &block_reference)?;
    if usize::try_from(progress.user_balances_left)? != users.len()
        || usize::try_from(progress.dex_balances_left)? != dexes.len()
    {
        bail!(
            "{engine_id} counts {} balances of accounts and {} of dexes left to move, but its state has {} and {} in the old layout",
            progress.user_balances_left,
            progress.dex_balances_left,
            users.len(),
            dexes.len()
        );
    }
    let protocol_limits = protocol::protocol_limits(network_config, &block_reference)?;
    let owners_and_assets = users
        .iter()
        .map(|(account_id, asset_id)| (AccountOrDexId::Account(account_id.clone()), asset_id))
        .chain(
            dexes
                .iter()
                .map(|(dex_id, asset_id)| (AccountOrDexId::Dex(dex_id.clone()), asset_id)),
        )
        .collect::<Vec<_>>();
    let balance_count = owners_and_assets.len();
    let mut moved_count: usize = 0;
    for batch in owners_and_assets.chunks(BALANCES_PER_BATCH) {
        let mut user_balances = Vec::new();
        let mut dex_balances = Vec::new();
        for (owner, asset_id) in batch {
            match owner {
                AccountOrDexId::Account(account_id) => {
                    user_balances.push(json!([account_id, asset_id]))
                }
                AccountOrDexId::Dex(dex_id) => dex_balances.push(json!([dex_id, asset_id])),
            }
        }
        let first = moved_count.saturating_add(1);
        moved_count = moved_count.saturating_add(batch.len());
        signer.send(
            &engine_id,
            vec![function_call(
                "migrate_balances",
                json!({ "user_balances": user_balances, "dex_balances": dex_balances }),
                NearToken::from_yoctonear(0),
                protocol_limits.max_prepaid_gas,
            )?],
            expected_outcome(
                "migration of balances",
                format!("Moved balances {first} to {moved_count} of {balance_count}"),
            ),
        )?;
    }

    let block_reference = final_block(network_config)?;
    let progress = engine::migration_progress(network_config, &block_reference)?
        .ok_or_else(|| eyre!("{engine_id} stopped migrating before finish_migration"))?;
    let flat_balances = engine_state::flat_balances(network_config, &block_reference)?;
    if progress.user_balances_left != 0
        || progress.dex_balances_left != 0
        || !flat_balances.is_empty()
    {
        bail!(
            "{} balances of accounts and {} of dexes are still in the old layout of {engine_id}",
            flat_balances.users.len(),
            flat_balances.dexes.len()
        );
    }
    tracing::info!("No balance is left in the old layout");
    Ok(MovedBalances {
        users: users.len(),
        dexes: dexes.len(),
    })
}

/// Ends the migration with the sums of storage balances, which can't change
/// while the new code is paused
fn finish_migration(
    signer: &Signer,
) -> color_eyre::eyre::Result<(StorageBalanceSum, StorageBalanceSum)> {
    let network_config = signer.network_config;
    let engine_id = ENGINE_ACCOUNT_ID.to_owned();
    let block_reference = final_block(network_config)?;
    let user_storage_balance_sum =
        engine_state::user_storage_balance_sum(network_config, &block_reference)?;
    let dex_storage_balance_sum =
        engine_state::dex_storage_balance_sum(network_config, &block_reference)?;
    signer.send(
        &engine_id,
        vec![function_call(
            "finish_migration",
            json!({
                "dex_storage_balances_total": NearToken::from_yoctonear(dex_storage_balance_sum.total),
                "dex_storage_balances_used": NearToken::from_yoctonear(dex_storage_balance_sum.used),
                "user_storage_balances_total": NearToken::from_yoctonear(user_storage_balance_sum.total),
                "user_storage_balances_used": NearToken::from_yoctonear(user_storage_balance_sum.used),
            }),
            NearToken::from_yoctonear(0),
            FINISH_MIGRATION_GAS,
        )?],
        expected_outcome(
            "end of the migration",
            format!("Finished the migration of {engine_id}"),
        ),
    )?;
    Ok((user_storage_balance_sum, dex_storage_balance_sum))
}

/// What the migration adds to the engine's storage: two records for each
/// balance instead of one, a record for each owner, and the new code
fn migration_growth_bytes(
    flat_balances: &FlatBalances,
    new_code_bytes: usize,
    old_code_bytes: usize,
    extra_bytes_per_record: u64,
) -> color_eyre::eyre::Result<u64> {
    let mut new_bytes = Vec::new();
    let mut old_bytes = Vec::new();
    let mut last_owner: Option<AccountOrDexId> = None;
    let owners_and_assets = flat_balances
        .users
        .iter()
        .map(|(account_id, asset_id)| (AccountOrDexId::Account(account_id.clone()), asset_id))
        .chain(
            flat_balances
                .dexes
                .iter()
                .map(|(dex_id, asset_id)| (AccountOrDexId::Dex(dex_id.clone()), asset_id)),
        );
    for (owner, asset_id) in owners_and_assets {
        new_bytes.push(storage::balance_record_bytes(
            asset_id,
            extra_bytes_per_record,
        )?);
        old_bytes.push(storage::flat_balance_record_bytes(
            &owner,
            asset_id,
            extra_bytes_per_record,
        )?);
        // Balances are in order, so an owner's are next to each other
        if last_owner.as_ref() != Some(&owner) {
            new_bytes.push(storage::owner_balances_record_bytes(
                &owner,
                extra_bytes_per_record,
            )?);
            last_owner = Some(owner);
        }
    }
    new_bytes.push(ENGINE_STATE_GROWTH_BYTES);
    new_bytes.push(u64::try_from(
        new_code_bytes.saturating_sub(old_code_bytes),
    )?);
    let sum = |bytes: Vec<u64>| {
        bytes
            .into_iter()
            .try_fold(0u64, |sum, bytes| sum.checked_add(bytes))
            .ok_or_else(|| eyre!("Storage size overflow"))
    };
    Ok(sum(new_bytes)?.saturating_sub(sum(old_bytes)?))
}

/// The engine's NEAR beyond what it owes, counted from the old code's state
/// like the new code's `untracked_near` counts it. Negative when it owes
/// more than it has.
fn untracked_near_of_old_engine(
    network_config: &NetworkConfig,
    block_reference: &BlockReference,
    storage_byte_cost: NearToken,
) -> color_eyre::eyre::Result<i128> {
    let engine_id = ENGINE_ACCOUNT_ID.to_owned();
    let engine_account = account::view_account(network_config, block_reference, &engine_id)?
        .ok_or_else(|| eyre!("{engine_id} doesn't exist"))?;
    let near_in_custody = engine::total_in_custody(
        network_config,
        block_reference,
        &intear_dex_types::AssetId::Near,
    )?
    .map_or(0, |U128(in_custody)| in_custody);
    let users = engine_state::user_storage_balance_sum(network_config, block_reference)?;
    let dexes = engine_state::dex_storage_balance_sum(network_config, block_reference)?;
    let overflow = || eyre!("NEAR overflow");
    let storage_locked = storage::storage_cost(engine_account.storage_usage, storage_byte_cost)?;
    let storage_paid_by_storage_balances =
        users.used.checked_add(dexes.used).ok_or_else(overflow)?;
    let owed = near_in_custody
        .checked_add(users.total)
        .and_then(|owed| owed.checked_add(dexes.total))
        .and_then(|owed| {
            owed.checked_add(
                storage_locked
                    .as_yoctonear()
                    .saturating_sub(storage_paid_by_storage_balances),
            )
        })
        .ok_or_else(overflow)?;
    i128::try_from(engine_account.amount.as_yoctonear())?
        .checked_sub(i128::try_from(owed)?)
        .ok_or_else(overflow)
}
