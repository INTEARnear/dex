use color_eyre::Section;
use color_eyre::eyre::{Context, eyre};
use intear_dex_types::{AssetId, DexId};
use near_cli_rs::network_view_at_block::{
    ArgsForViewContext, OnAfterGettingBlockReferenceCallback,
};
use near_primitives::types::{AccountId, BlockId, BlockReference};
use near_sdk::NearToken;
use serde_json::json;

use crate::chain::asset_metadata::AssetMetadata;
use crate::chain::{account, engine};
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display::{self, OutputFormat};
use crate::errors::PreflightError;

/// Prefixes of the engine's collections in its state, in the order of its
/// `StorageKey`
const DEX_STORAGE_BALANCES_PREFIX: u8 = 3;
const USER_BALANCES_PREFIX: u8 = 4;
const USER_STORAGE_BALANCES_PREFIX: u8 = 5;
/// As many as the mainnet migration test sends per call, which fit in 300 Tgas
const BACKFILL_ENTRIES_PER_BATCH: usize = 200;
const MIGRATE_ARGS_FILE_NAME: &str = "migrate-args.json";

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = MigrationArgsContext)]
pub struct MigrationArgs {
    #[interactive_clap(skip_default_input_arg)]
    /// The account that deploys dex code after the migration
    trusted_code_deployer: near_cli_rs::types::account_id::AccountId,
    /// An empty or new directory for migrate-args.json and the backfill batches
    output_directory: near_cli_rs::types::path_buf::PathBuf,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_view_at_block::NetworkViewAtBlockArgs,
}

impl MigrationArgs {
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
pub struct MigrationArgsContext(ArgsForViewContext);

impl MigrationArgsContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<MigrationArgs as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let output_format = previous_context.output_format;
        let trusted_code_deployer: AccountId = scope.trusted_code_deployer.clone().into();
        let output_directory = scope.output_directory.0.clone();
        let on_after_getting_block_reference_callback: OnAfterGettingBlockReferenceCallback =
            std::sync::Arc::new(move |network_config, block_reference| {
                let block = engine::engine_block(network_config, block_reference)?;
                let block_reference = BlockReference::BlockId(BlockId::Hash(block.header.hash));
                if account::view_account(network_config, &block_reference, &trusted_code_deployer)?
                    .is_none()
                {
                    return Err(PreflightError::AccountMissing {
                        account_id: trusted_code_deployer.clone(),
                        network_name: network_config.network_name.clone(),
                    }
                    .into_report());
                }
                std::fs::create_dir_all(&output_directory)
                    .wrap_err_with(|| format!("Couldn't create {}", output_directory.display()))?;
                let directory_is_empty = std::fs::read_dir(&output_directory)
                    .wrap_err_with(|| format!("Couldn't read {}", output_directory.display()))?
                    .next()
                    .is_none();
                if !directory_is_empty {
                    return Err(eyre!("{} isn't empty", output_directory.display()).suggestion(
                        "Choose a new directory, so that no file of an earlier run is sent with these",
                    ));
                }
                let paused = engine::is_paused(network_config, &block_reference)?;
                let (dex_storage_total, dex_storage_used) =
                    sum_storage_balances::<DexId>(engine::state_records_with_prefix(
                        network_config,
                        &block_reference,
                        DEX_STORAGE_BALANCES_PREFIX,
                    )?)?;
                let (user_storage_total, user_storage_used) =
                    sum_storage_balances::<AccountId>(engine::state_records_with_prefix(
                        network_config,
                        &block_reference,
                        USER_STORAGE_BALANCES_PREFIX,
                    )?)?;
                let mut backfill_entries = Vec::new();
                for (key, _balance) in engine::state_records_with_prefix(
                    network_config,
                    &block_reference,
                    USER_BALANCES_PREFIX,
                )? {
                    if let Some(entry) = parse_state_key::<(AccountId, AssetId)>(&key)? {
                        backfill_entries.push(entry);
                    }
                }
                backfill_entries.sort();

                let migrate_args = json!({
                    "trusted_code_deployer": trusted_code_deployer,
                    "dex_storage_balances_total": NearToken::from_yoctonear(dex_storage_total),
                    "dex_storage_balances_used": NearToken::from_yoctonear(dex_storage_used),
                    "user_storage_balances_total": NearToken::from_yoctonear(user_storage_total),
                    "user_storage_balances_used": NearToken::from_yoctonear(user_storage_used),
                });
                let mut written_file_names = vec![MIGRATE_ARGS_FILE_NAME.to_string()];
                std::fs::write(
                    output_directory.join(MIGRATE_ARGS_FILE_NAME),
                    serde_json::to_vec(&migrate_args)?,
                )?;
                for (batch_index, batch) in backfill_entries
                    .chunks(BACKFILL_ENTRIES_PER_BATCH)
                    .enumerate()
                {
                    let batch_file_name =
                        format!("backfill-{:03}.json", batch_index.saturating_add(1));
                    std::fs::write(
                        output_directory.join(&batch_file_name),
                        serde_json::to_vec(&json!({ "entries": batch }))?,
                    )?;
                    written_file_names.push(batch_file_name);
                }
                let batch_count = written_file_names.len().saturating_sub(1);

                let near = AssetMetadata::near();
                match output_format {
                    OutputFormat::Table => {
                        let near_amount =
                            |yocto_near: u128| display::format_amount(yocto_near, Some(&near));
                        let table = display::key_value_table(vec![
                            ("Block", block.header.height.to_string()),
                            (
                                "Paused",
                                if paused {
                                    "yes".to_string()
                                } else {
                                    "no, so storage balances can still change: write the files again once it's paused".to_string()
                                },
                            ),
                            ("Trusted code deployer", trusted_code_deployer.to_string()),
                            (
                                "Dex storage balances",
                                format!(
                                    "{} total, {} used",
                                    near_amount(dex_storage_total),
                                    near_amount(dex_storage_used)
                                ),
                            ),
                            (
                                "User storage balances",
                                format!(
                                    "{} total, {} used",
                                    near_amount(user_storage_total),
                                    near_amount(user_storage_used)
                                ),
                            ),
                            (
                                "Registered assets to list",
                                format!(
                                    "{} in {} of up to {BACKFILL_ENTRIES_PER_BATCH}",
                                    backfill_entries.len(),
                                    match batch_count {
                                        1 => "1 batch".to_string(),
                                        batch_count => format!("{batch_count} batches"),
                                    }
                                ),
                            ),
                            ("Directory", output_directory.display().to_string()),
                            (
                                "Files",
                                match written_file_names.as_slice() {
                                    [migrate_args_file, first_batch_file, .., last_batch_file] => {
                                        format!(
                                            "{migrate_args_file}, {first_batch_file} to {last_batch_file}"
                                        )
                                    }
                                    file_names => file_names.join(", "),
                                },
                            ),
                        ]);
                        print!("{table}");
                    }
                    OutputFormat::Json => {
                        display::print_json(&json!({
                            "block_height": block.header.height,
                            "paused": paused,
                            "migrate_args": migrate_args,
                            "backfill_entries": backfill_entries.len(),
                            "backfill_batches": batch_count,
                            "files": written_file_names
                                .iter()
                                .map(|file_name| output_directory.join(file_name).display().to_string())
                                .collect::<Vec<_>>(),
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

impl From<MigrationArgsContext> for ArgsForViewContext {
    fn from(item: MigrationArgsContext) -> Self {
        item.0
    }
}

/// A state key without its collection prefix. `total_in_custody` keeps its
/// values under 32-byte hashes, which can start with any prefix, so those
/// don't have to parse.
fn parse_state_key<Key: near_sdk::borsh::BorshDeserialize>(
    key: &[u8],
) -> color_eyre::eyre::Result<Option<Key>> {
    const HASHED_KEY_LENGTH: usize = 32;
    let (_prefix, key_without_prefix) = key
        .split_first()
        .ok_or_else(|| eyre!("{ENGINE_ACCOUNT_ID} has an empty key in its state"))?;
    match near_sdk::borsh::from_slice::<Key>(key_without_prefix) {
        Ok(parsed_key) => Ok(Some(parsed_key)),
        Err(_) if key.len() == HASHED_KEY_LENGTH => Ok(None),
        Err(error) => Err(error).wrap_err_with(|| {
            format!(
                "Unexpected key {} in the state of {ENGINE_ACCOUNT_ID}",
                near_primitives::serialize::to_base64(key)
            )
        }),
    }
}

/// Storage balances keep how much NEAR an account or a dex deposited and how
/// much of it pays for storage
fn sum_storage_balances<Owner: near_sdk::borsh::BorshDeserialize>(
    records: Vec<(Vec<u8>, Vec<u8>)>,
) -> color_eyre::eyre::Result<(u128, u128)> {
    let mut total_sum: u128 = 0;
    let mut used_sum: u128 = 0;
    for (key, value) in records {
        if parse_state_key::<Owner>(&key)?.is_none() {
            continue;
        }
        let (total, used): (u128, u128) = near_sdk::borsh::from_slice(&value)
            .wrap_err("Unexpected storage balance in the state of the engine")?;
        total_sum = total_sum
            .checked_add(total)
            .ok_or_else(|| eyre!("Storage balances overflow"))?;
        used_sum = used_sum
            .checked_add(used)
            .ok_or_else(|| eyre!("Storage balances overflow"))?;
    }
    Ok((total_sum, used_sum))
}
