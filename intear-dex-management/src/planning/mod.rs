pub mod storage;
pub mod xyk_math;
pub mod xyk_storage;

use std::sync::{Arc, Mutex};

use color_eyre::eyre::eyre;
use intear_dex_types::{AccountOrDexId, AssetId, DexId, Operation};
use near_cli_rs::commands::{ActionContext, PrepopulatedTransaction};
use near_cli_rs::config::{Config, NetworkConfig};
use near_cli_rs::transaction_signature_options::SignedTransactionOrSignedDelegateAction;
use near_primitives::gas::Gas;
use near_primitives::transaction::{Action, FunctionCallAction};
use near_primitives::types::{AccountId, BlockId, BlockReference, Finality};
use near_sdk::NearToken;
use near_sdk::json_types::{Base64VecU8, U128};

use crate::chain::asset_metadata::AssetMetadata;
use crate::chain::protocol::ProtocolLimits;
use crate::chain::{account, engine, protocol};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display;
use crate::errors::PreflightError;
use crate::inputs::account_or_dex_id::AccountOrDexIdArg;
use crate::outcome::{self, ExpectedOutcome, SentTransaction};

pub const ONE_YOCTO_NEAR: NearToken = NearToken::from_yoctonear(1);
pub const ENGINE_STORAGE_DEPOSIT_GAS: Gas = Gas::from_teragas(10);
const REGISTER_ASSETS_BASE_GAS: Gas = Gas::from_teragas(5);
const REGISTER_ASSETS_GAS_PER_ASSET: Gas = Gas::from_teragas(3);

pub fn register_assets_gas(asset_count: usize) -> color_eyre::eyre::Result<Gas> {
    REGISTER_ASSETS_GAS_PER_ASSET
        .checked_mul(u64::try_from(asset_count)?)
        .and_then(|assets_gas| assets_gas.checked_add(REGISTER_ASSETS_BASE_GAS))
        .ok_or_else(|| eyre!("Gas overflow"))
}

pub struct FunctionCall {
    pub method_name: &'static str,
    pub args: serde_json::Value,
    pub deposit: NearToken,
    pub gas: Gas,
}

/// One transaction to one contract
pub struct TransactionPlan {
    pub receiver_id: AccountId,
    /// What the transaction does, one plain sentence per step, shown before
    /// signing
    pub steps: Vec<String>,
    pub function_calls: Vec<FunctionCall>,
    pub expected_outcome: ExpectedOutcome,
}

/// Chain state that every write reads before signing, at one final block
pub struct Preflight {
    pub network_config: NetworkConfig,
    /// The near-cli connection the user chose, for suggested commands
    pub connection_name: String,
    pub block_reference: BlockReference,
    /// When that block was made, in nanoseconds, which `now` in fee
    /// schedules means
    pub block_timestamp_nanoseconds: u64,
    pub gas_price: NearToken,
    pub protocol_limits: ProtocolLimits,
    pub engine_paused: bool,
    pub signer_id: AccountId,
    /// The signer's NEAR, minus what its own storage locks
    pub signer_spendable_balance: NearToken,
}

impl Preflight {
    fn read(
        config: &Config,
        network_config: &NetworkConfig,
        signer_id: &AccountId,
    ) -> color_eyre::eyre::Result<Self> {
        let block = engine::engine_block(network_config, &Finality::Final.into())?;
        let block_reference = BlockReference::BlockId(BlockId::Hash(block.header.hash));
        let protocol_limits = protocol::protocol_limits(network_config, &block_reference)?;
        let signer_account = account::view_account(network_config, &block_reference, signer_id)?
            .ok_or_else(|| {
                PreflightError::AccountMissing {
                    account_id: signer_id.clone(),
                    network_name: network_config.network_name.clone(),
                }
                .into_report()
            })?;
        let storage_stake = storage::storage_cost(
            signer_account.storage_usage,
            protocol_limits.storage_byte_cost,
        )?;
        let connection_name = config
            .network_connection
            .iter()
            .find(|(_, connection)| {
                connection.rpc_url == network_config.rpc_url
                    && connection.network_name == network_config.network_name
            })
            .map(|(connection_name, _)| connection_name.clone())
            .ok_or_else(|| eyre!("The chosen network connection isn't in the near-cli config"))?;
        Ok(Self {
            network_config: network_config.clone(),
            connection_name,
            engine_paused: engine::is_paused(network_config, &block_reference)?,
            block_reference,
            block_timestamp_nanoseconds: block.header.timestamp_nanosec,
            gas_price: block.header.gas_price,
            protocol_limits,
            signer_id: signer_id.clone(),
            signer_spendable_balance: signer_account
                .amount
                .saturating_sub(storage_stake.saturating_sub(signer_account.locked)),
        })
    }

    pub fn ensure_engine_not_paused(&self) -> color_eyre::eyre::Result<()> {
        if self.engine_paused {
            return Err(PreflightError::EnginePaused.into_report());
        }
        Ok(())
    }

    pub fn ensure_account_exists(&self, account_id: &AccountId) -> color_eyre::eyre::Result<()> {
        if account::view_account(&self.network_config, &self.block_reference, account_id)?.is_none()
        {
            return Err(PreflightError::AccountMissing {
                account_id: account_id.clone(),
                network_name: self.network_config.network_name.clone(),
            }
            .into_report());
        }
        Ok(())
    }

    /// The signer's balance of an asset on the engine, which has to cover
    /// `needed`
    pub fn ensure_engine_balance(
        &self,
        asset_id: &AssetId,
        metadata: Option<&AssetMetadata>,
        needed: u128,
    ) -> color_eyre::eyre::Result<u128> {
        let U128(balance) = engine::asset_balance_of(
            &self.network_config,
            &self.block_reference,
            &AccountOrDexId::Account(self.signer_id.clone()),
            asset_id,
        )?
        .ok_or_else(|| {
            PreflightError::NoBalance {
                owner: self.signer_id.to_string(),
                asset_label: display::asset_label(asset_id, metadata),
            }
            .into_report()
        })?;
        if needed > balance {
            return Err(PreflightError::InsufficientEngineBalance {
                owner: self.signer_id.to_string(),
                available: display::format_amount(balance, metadata),
                needed: display::format_amount(needed, metadata),
            }
            .into_report());
        }
        Ok(balance)
    }

    pub fn ensure_dex_exists(&self, dex_id: &DexId) -> color_eyre::eyre::Result<()> {
        if !engine::dex_exists(&self.network_config, &self.block_reference, dex_id)? {
            return Err(PreflightError::DexMissing {
                dex_id: dex_id.clone(),
                network_name: self.network_config.network_name.clone(),
            }
            .into_report());
        }
        Ok(())
    }
}

/// A storage deposit that has to go before registrations, because the
/// signer's storage balance on the engine can't pay for them
pub struct StorageTopUp {
    pub deposit: NearToken,
}

/// What registrations take from the signer's storage balance on the engine
pub struct RegistrationStorage {
    pub needed: NearToken,
    pub top_up: Option<StorageTopUp>,
}

impl StorageTopUp {
    pub fn step(&self, signer_id: &AccountId) -> String {
        format!(
            "Deposit {} into {signer_id}'s storage balance on {ENGINE_ACCOUNT_ID}",
            format_near(self.deposit)
        )
    }

    pub fn function_call(&self) -> FunctionCall {
        FunctionCall {
            method_name: "storage_deposit",
            args: serde_json::json!({}),
            deposit: self.deposit,
            gas: ENGINE_STORAGE_DEPOSIT_GAS,
        }
    }
}

impl Preflight {
    /// Registrations are charged to the signer's storage balance on the
    /// engine
    pub fn storage_for_registrations(
        &self,
        registrations: &[(AccountOrDexId, AssetId)],
    ) -> color_eyre::eyre::Result<RegistrationStorage> {
        let mut bytes = Vec::new();
        let mut owners_with_registered_assets: Vec<&AccountOrDexId> = Vec::new();
        let mut assets_in_custody: Vec<AssetId> = Vec::new();
        for (owner, asset_id) in registrations {
            let owner_has_registered_assets = owners_with_registered_assets.contains(&owner)
                || engine::has_registered_assets(
                    &self.network_config,
                    &self.block_reference,
                    owner,
                )?;
            let asset_is_in_custody = assets_in_custody.contains(asset_id)
                || engine::total_in_custody(&self.network_config, &self.block_reference, asset_id)?
                    .is_some();
            bytes.push(storage::asset_registration_bytes(
                owner,
                asset_id,
                owner_has_registered_assets,
                asset_is_in_custody,
                self.protocol_limits.extra_bytes_per_record,
            )?);
            owners_with_registered_assets.push(owner);
            assets_in_custody.push(asset_id.clone());
        }
        let storage_balance = engine::storage_balance_of(
            &self.network_config,
            &self.block_reference,
            &self.signer_id,
        )?;
        if storage_balance.is_none() {
            bytes.push(storage::storage_balance_record_bytes(
                &self.signer_id,
                self.protocol_limits.extra_bytes_per_record,
            )?);
        }
        let total_bytes = bytes
            .into_iter()
            .try_fold(0u64, |total, bytes| total.checked_add(bytes))
            .ok_or_else(|| eyre!("Storage size overflow"))?;
        let needed = storage::storage_cost(total_bytes, self.protocol_limits.storage_byte_cost)?;
        let available = storage_balance.map_or(NearToken::from_yoctonear(0), |storage_balance| {
            storage_balance.available
        });
        let minimum_deposit =
            engine::minimum_storage_deposit(&self.network_config, &self.block_reference)?;
        Ok(RegistrationStorage {
            needed,
            top_up: storage::storage_deposit_for(needed, available, minimum_deposit)
                .map(|deposit| StorageTopUp { deposit }),
        })
    }
}

/// A deposit into a dex's storage balance that has to go before calls that
/// store data for the dex
pub struct DexStorageTopUp {
    pub dex_id: DexId,
    pub deposit: NearToken,
}

impl DexStorageTopUp {
    pub fn step(&self, purpose: &str) -> String {
        format!(
            "Deposit {} into {}'s storage balance on {ENGINE_ACCOUNT_ID}, which then covers {purpose}",
            format_near(self.deposit),
            self.dex_id
        )
    }

    pub fn function_call(&self) -> FunctionCall {
        FunctionCall {
            method_name: "dex_storage_deposit",
            args: serde_json::json!({ "dex_id": self.dex_id }),
            deposit: self.deposit,
            gas: ENGINE_STORAGE_DEPOSIT_GAS,
        }
    }
}

impl Preflight {
    /// The top-up a dex's storage balance needs before `bytes` more are
    /// stored for it, if what's available doesn't cover them
    pub fn dex_storage_top_up(
        &self,
        dex_id: &DexId,
        bytes: u64,
    ) -> color_eyre::eyre::Result<Option<DexStorageTopUp>> {
        let storage_balance =
            engine::dex_storage_balance_of(&self.network_config, &self.block_reference, dex_id)?;
        // A first deposit pays for the storage balance's own record
        let storage_balance_record_bytes = match &storage_balance {
            Some(_) => 0,
            None => storage::storage_balance_record_bytes(
                dex_id,
                self.protocol_limits.extra_bytes_per_record,
            )?,
        };
        let needed = storage::storage_cost(
            bytes
                .checked_add(storage_balance_record_bytes)
                .ok_or_else(|| eyre!("Storage size overflow"))?,
            self.protocol_limits.storage_byte_cost,
        )?;
        let available = storage_balance.map_or(NearToken::from_yoctonear(0), |storage_balance| {
            storage_balance.available
        });
        let minimum_deposit =
            engine::minimum_dex_storage_deposit(&self.network_config, &self.block_reference)?;
        Ok(
            storage::storage_deposit_for(needed, available, minimum_deposit).map(|deposit| {
                DexStorageTopUp {
                    dex_id: dex_id.clone(),
                    deposit,
                }
            }),
        )
    }
}

/// What a transaction does first so that assets are registered on the
/// engine: a storage top-up when the signer's storage balance can't pay, then
/// one register_assets call per owner
pub struct RegistrationCalls {
    pub steps: Vec<String>,
    pub function_calls: Vec<FunctionCall>,
}

impl Preflight {
    /// The registrations among `registrations` that the engine doesn't have
    /// yet, as calls that go before the transaction's own calls
    pub fn registration_calls(
        &self,
        registrations: &[(AccountOrDexId, AssetId)],
    ) -> color_eyre::eyre::Result<RegistrationCalls> {
        let mut missing_registrations: Vec<(AccountOrDexId, AssetId)> = Vec::new();
        for registration in registrations {
            let (owner, asset_id) = registration;
            if !missing_registrations.contains(registration)
                && engine::asset_balance_of(
                    &self.network_config,
                    &self.block_reference,
                    owner,
                    asset_id,
                )?
                .is_none()
            {
                missing_registrations.push(registration.clone());
            }
        }
        let mut steps = Vec::new();
        let mut function_calls = Vec::new();
        if missing_registrations.is_empty() {
            return Ok(RegistrationCalls {
                steps,
                function_calls,
            });
        }
        let registration_storage = self.storage_for_registrations(&missing_registrations)?;
        if let Some(storage_top_up) = &registration_storage.top_up {
            steps.push(storage_top_up.step(&self.signer_id));
            function_calls.push(storage_top_up.function_call());
        }
        let mut owners: Vec<&AccountOrDexId> = Vec::new();
        for (owner, _) in &missing_registrations {
            if !owners.contains(&owner) {
                owners.push(owner);
            }
        }
        let mut registration_descriptions = Vec::new();
        for owner in owners {
            let asset_ids = missing_registrations
                .iter()
                .filter(|(registration_owner, _)| registration_owner == owner)
                .map(|(_, asset_id)| asset_id.clone())
                .collect::<Vec<_>>();
            registration_descriptions.push(format!(
                "{} for {}",
                asset_ids
                    .iter()
                    .map(AssetId::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                AccountOrDexIdArg(owner.clone())
            ));
            let register_for = match owner {
                AccountOrDexId::Account(account_id) if *account_id == self.signer_id => None,
                _ => Some(owner.clone()),
            };
            function_calls.push(FunctionCall {
                method_name: "register_assets",
                args: serde_json::json!({ "asset_ids": asset_ids, "for": register_for }),
                deposit: ONE_YOCTO_NEAR,
                gas: register_assets_gas(asset_ids.len())?,
            });
        }
        steps.push(format!(
            "Register {}, which takes about {} of {}'s storage balance",
            registration_descriptions.join(" and "),
            format_near(registration_storage.needed),
            self.signer_id
        ));
        Ok(RegistrationCalls {
            steps,
            function_calls,
        })
    }
}

/// A call of `method` on a dex hosted by the engine, with `args` in borsh as
/// dexes take them; the attached assets move from the signer's balance to the
/// dex. Operation keeps attached assets in a HashMap, whose order changes
/// from run to run, so they're put in order of asset id.
pub fn dex_call_operation(
    dex_id: &DexId,
    method: &str,
    args: &impl near_sdk::borsh::BorshSerialize,
    attached_assets: &[(AssetId, u128)],
) -> color_eyre::eyre::Result<serde_json::Value> {
    let mut operation = serde_json::to_value(Operation::DexCall {
        dex_id: dex_id.clone(),
        method: method.to_string(),
        args: Base64VecU8(near_sdk::borsh::to_vec(args)?),
        attached_assets: std::collections::HashMap::new(),
    })?;
    operation["DexCall"]["attached_assets"] = serde_json::to_value(
        attached_assets
            .iter()
            .map(|(asset_id, amount)| (asset_id, U128(*amount)))
            .collect::<std::collections::BTreeMap<_, _>>(),
    )?;
    Ok(operation)
}

pub fn format_near(amount: NearToken) -> String {
    display::format_amount(amount.as_yoctonear(), Some(&AssetMetadata::near()))
}

fn prepopulated_transaction(
    plan: &TransactionPlan,
    preflight: &Preflight,
) -> color_eyre::eyre::Result<PrepopulatedTransaction> {
    let total_gas = plan
        .function_calls
        .iter()
        .try_fold(Gas::from_gas(0), |total, call| total.checked_add(call.gas))
        .ok_or_else(|| eyre!("Gas overflow"))?;
    if total_gas > preflight.protocol_limits.max_prepaid_gas {
        return Err(PreflightError::TooMuchGas {
            needed: format!("{} Tgas", total_gas.as_teragas()),
            limit: format!(
                "{} Tgas",
                preflight.protocol_limits.max_prepaid_gas.as_teragas()
            ),
        }
        .into_report());
    }
    let total_deposit = plan
        .function_calls
        .iter()
        .try_fold(NearToken::from_yoctonear(0), |total, call| {
            total.checked_add(call.deposit)
        })
        .ok_or_else(|| eyre!("Deposit overflow"))?;
    let most_gas_can_cost = preflight
        .gas_price
        .checked_mul(u128::from(total_gas.as_gas()))
        .ok_or_else(|| eyre!("Gas cost overflow"))?;
    let most_transaction_can_cost = total_deposit
        .checked_add(most_gas_can_cost)
        .ok_or_else(|| eyre!("Cost overflow"))?;
    if most_transaction_can_cost > preflight.signer_spendable_balance {
        return Err(PreflightError::InsufficientWalletBalance {
            account_id: preflight.signer_id.clone(),
            available: format_near(preflight.signer_spendable_balance),
            needed: format_near(most_transaction_can_cost),
        }
        .into_report());
    }
    let actions = plan
        .function_calls
        .iter()
        .map(|call| {
            Ok(Action::FunctionCall(Box::new(FunctionCallAction {
                method_name: call.method_name.to_string(),
                args: serde_json::to_vec(&call.args)?,
                gas: call.gas,
                deposit: call.deposit,
            })))
        })
        .collect::<color_eyre::eyre::Result<Vec<_>>>()?;
    Ok(PrepopulatedTransaction {
        signer_id: preflight.signer_id.clone(),
        receiver_id: plan.receiver_id.clone(),
        actions,
    })
}

fn print_summary(plan: &TransactionPlan, preflight: &Preflight) {
    let network = if preflight.network_config.network_name == "mainnet" {
        "MAINNET".to_string()
    } else {
        preflight.network_config.network_name.clone()
    };
    let mut lines = vec![format!(
        "{network} · signer {} · {}",
        preflight.signer_id, plan.receiver_id
    )];
    lines.extend(plan.steps.iter().cloned());
    let actions = plan
        .function_calls
        .iter()
        .map(|call| {
            let deposit = if call.deposit == ONE_YOCTO_NEAR {
                "1 yoctoNEAR".to_string()
            } else {
                format_near(call.deposit)
            };
            format!(
                "{} ({deposit}, {} Tgas)",
                call.method_name,
                call.gas.as_teragas()
            )
        })
        .collect::<Vec<_>>();
    lines.push(format!("Actions: {}", actions.join(", ")));
    tracing::info!("{}", lines.join("\n│  "));
}

/// near-cli-rs prints the transaction it is about to sign with every
/// argument in full, which for a deploy is all of the code as base64. This
/// prints the same with long strings shortened, and main hides near-cli-rs's
/// copy.
fn print_unsigned_transaction(
    plan: &TransactionPlan,
    preflight: &Preflight,
) -> color_eyre::eyre::Result<()> {
    const LONGEST_SHOWN_STRING: usize = 1_000;
    fn shorten_long_strings(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(string) if string.len() > LONGEST_SHOWN_STRING => {
                *value = serde_json::Value::String(format!("<{} characters>", string.len()));
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(shorten_long_strings),
            serde_json::Value::Object(fields) => fields.values_mut().for_each(shorten_long_strings),
            _ => {}
        }
    }
    let actions = plan
        .function_calls
        .iter()
        .map(|call| {
            let mut shown_args = call.args.clone();
            shorten_long_strings(&mut shown_args);
            Ok(Action::FunctionCall(Box::new(FunctionCallAction {
                method_name: call.method_name.to_string(),
                args: serde_json::to_vec(&shown_args)?,
                gas: call.gas,
                deposit: call.deposit,
            })))
        })
        .collect::<color_eyre::eyre::Result<Vec<_>>>()?;
    let shown_transaction = PrepopulatedTransaction {
        signer_id: preflight.signer_id.clone(),
        receiver_id: plan.receiver_id.clone(),
        actions,
    };
    // near-cli-rs signs as a delegate action when the connection has a relayer
    let what_is_signed = if preflight
        .network_config
        .meta_transaction_relayer_url
        .is_some()
    {
        "Unsigned delegate action:"
    } else {
        "Unsigned transaction:"
    };
    tracing::info!(
        "{what_is_signed}{}",
        near_cli_rs::common::indent_payload(&near_cli_rs::common::print_unsigned_transaction(
            &shown_transaction
        ))
    );
    Ok(())
}

/// near-cli-rs makes the plan after the network step and reports the outcome
/// after sending; both need what the plan expected
struct PlannedTransaction {
    expected_outcome: ExpectedOutcome,
    sent_transaction: SentTransaction,
}

fn planned_transaction(
    planned: &Mutex<Option<PlannedTransaction>>,
) -> color_eyre::eyre::Result<std::sync::MutexGuard<'_, Option<PlannedTransaction>>> {
    planned
        .lock()
        .map_err(|_| eyre!("The planned transaction is unusable"))
}

/// The near-cli-rs context of a write command: `plan_transaction` reads chain
/// state through the preflight and decides what to sign
pub fn write_action_context(
    global_context: &crate::GlobalContext,
    signer_id: AccountId,
    plan_transaction: impl Fn(&Preflight) -> color_eyre::eyre::Result<TransactionPlan> + 'static,
) -> ActionContext {
    let planned: Arc<Mutex<Option<PlannedTransaction>>> = Arc::new(Mutex::new(None));
    let output_format = global_context.output_format;
    let verbosity = global_context.verbosity;

    let get_prepopulated_transaction_after_getting_network_callback = {
        let planned = planned.clone();
        let config = global_context.config.clone();
        let signer_id = signer_id.clone();
        Arc::new(move |network_config: &NetworkConfig| {
            let preflight = Preflight::read(&config, network_config, &signer_id)?;
            let plan = plan_transaction(&preflight)?;
            let prepopulated_transaction = prepopulated_transaction(&plan, &preflight)?;
            print_summary(&plan, &preflight);
            print_unsigned_transaction(&plan, &preflight)?;
            *planned_transaction(&planned)? = Some(PlannedTransaction {
                sent_transaction: SentTransaction {
                    signer_id: signer_id.clone(),
                    receiver_id: plan.receiver_id,
                    connection_name: preflight.connection_name,
                    action_name: plan.expected_outcome.action_name,
                    dex_id: plan.expected_outcome.dex_id.clone(),
                    url: String::new(),
                },
                expected_outcome: plan.expected_outcome,
            });
            Ok(prepopulated_transaction)
        })
    };

    let on_before_sending_transaction_callback = {
        let planned = planned.clone();
        Arc::new(
            move |signed_transaction: &SignedTransactionOrSignedDelegateAction,
                  network_config: &NetworkConfig| {
                if let SignedTransactionOrSignedDelegateAction::SignedTransaction(
                    signed_transaction,
                ) = signed_transaction
                {
                    let mut planned = planned_transaction(&planned)?;
                    let planned = planned
                        .as_mut()
                        .ok_or_else(|| eyre!("A transaction is being sent without a plan"))?;
                    planned.sent_transaction.url = format!(
                        "{}{}",
                        network_config.explorer_transaction_url,
                        signed_transaction.get_hash()
                    );
                    outcome::record_sent_transaction(planned.sent_transaction.clone())?;
                }
                Ok(String::new())
            },
        )
    };

    let on_after_sending_transaction_callback = Arc::new(
        move |outcome_view: &near_primitives::views::FinalExecutionOutcomeView,
              network_config: &NetworkConfig| {
            let planned = planned_transaction(&planned)?;
            let planned = planned
                .as_ref()
                .ok_or_else(|| eyre!("A transaction was sent without a plan"))?;
            outcome::report_outcome(
                outcome_view,
                network_config,
                &planned.expected_outcome,
                &planned.sent_transaction,
                output_format,
                verbosity,
            )
        },
    );

    ActionContext {
        global_context: near_cli_rs::GlobalContext {
            config: global_context.config.clone(),
            offline: false,
            verbosity,
        },
        interacting_with_account_ids: vec![ENGINE_ACCOUNT_ID.to_owned(), signer_id],
        get_prepopulated_transaction_after_getting_network_callback,
        on_before_signing_callback: Arc::new(|_, _| Ok(())),
        on_before_sending_transaction_callback,
        on_after_sending_transaction_callback,
        on_sending_delegate_action_callback: None,
        sign_as_delegate_action: false,
    }
}
