use amp_dex_types::{FULL_FEE_FRACTION, FeeConfiguration, FeeFraction, PoolView, TokenAmount};
use color_eyre::eyre::bail;
use intear_dex_common::{AssetId, DexId};
use near_primitives::types::AccountId;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};
use tracing_indicatif::span_ext::IndicatifSpanExt;

use crate::chain::asset_metadata::{AssetMetadata, AssetMetadataCache};
use crate::commands::xyk::{FeesContext, FeesDex};
use crate::deployment::{DEFAULT_AMP_DEX_DEPLOYER, DEFAULT_AMP_DEX_NAME};
use crate::errors::PreflightError;
use crate::inputs::amount::AmountArg;
use crate::inputs::dex_id::DexIdArg;
use crate::inputs::pool_id::PoolIdArg;
use crate::planning::Preflight;
use xyk_dex_types::PoolId;

pub mod create_pool;
pub mod deploy;
pub mod init;
pub mod liquidity;
pub mod pool;
pub mod pools;
pub mod presentation;

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = crate::GlobalContext)]
#[interactive_clap(output_context = AmpContext)]
pub struct AmpCommands {
    #[interactive_clap(long)]
    #[interactive_clap(skip_interactive_input)]
    /// The amp dex as <deployer>/<name> (default: slimedragon.near/amp)
    dex: Option<DexIdArg>,
    #[interactive_clap(subcommand)]
    action: AmpAction,
}

#[derive(Clone)]
pub struct AmpContext {
    pub global_context: crate::GlobalContext,
    pub dex_id: DexId,
    /// Whether --dex chose the dex, which deploy doesn't take
    pub is_dex_chosen: bool,
}

impl AmpContext {
    pub fn from_previous_context(
        previous_context: crate::GlobalContext,
        scope: &<AmpCommands as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let dex_id = match &scope.dex {
            Some(DexIdArg(dex_id)) => dex_id.clone(),
            None => DexId {
                deployer: DEFAULT_AMP_DEX_DEPLOYER.to_owned(),
                id: DEFAULT_AMP_DEX_NAME.to_string(),
            },
        };
        Ok(Self {
            global_context: previous_context,
            dex_id,
            is_dex_chosen: scope.dex.is_some(),
        })
    }
}

impl From<AmpContext> for FeesContext {
    fn from(amp_context: AmpContext) -> Self {
        Self {
            global_context: amp_context.global_context,
            dex_id: amp_context.dex_id,
            dex: FeesDex::Amp,
        }
    }
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = AmpContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// What do you want to do?
pub enum AmpAction {
    #[strum_discriminants(strum(message = "pools           - List or count pools"))]
    /// List or count pools
    Pools(self::pools::PoolsCommands),
    #[strum_discriminants(strum(
        message = "pool            - One pool in detail, or change it: set-amplification, set-oracle-key, set-fees"
    ))]
    /// One pool in detail, or change it: set-amplification, set-oracle-key, set-fees
    Pool(self::pool::PoolCommands),
    #[strum_discriminants(strum(message = "liquidity       - Show, add or remove liquidity"))]
    /// Show, add or remove liquidity
    Liquidity(self::liquidity::LiquidityCommands),
    #[strum_discriminants(strum(
        message = "fees            - Fees an account collected: pending or withdraw"
    ))]
    /// Fees an account collected: pending or withdraw
    Fees(crate::commands::xyk::fees::FeesCommands),
    #[strum_discriminants(strum(
        message = "fee-discounts   - Who signs fee discounts: show, set-signer, disable"
    ))]
    /// Who signs fee discounts: show, set-signer, disable
    FeeDiscounts(crate::commands::xyk::fee_discounts::FeeDiscountsCommands),
    #[strum_discriminants(strum(
        message = "create-pool     - Create a pool rated at an oracle's prices, as the owner of the dex"
    ))]
    /// Create a pool rated at an oracle's prices, as the owner of the dex
    CreatePool(self::create_pool::CreatePool),
    #[strum_discriminants(strum(
        message = "deploy          - Deploy amp code as <signer>/<name>; --migrate converts the state too"
    ))]
    /// Deploy amp code as <signer>/<name>; --migrate converts the state too
    Deploy(self::deploy::Deploy),
    #[strum_discriminants(strum(message = "init            - Initialize a newly deployed dex"))]
    /// Initialize a newly deployed dex
    Init(self::init::Init),
}

struct PoolChoice {
    pool_id: PoolId,
    label: String,
}

impl std::fmt::Display for PoolChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[tracing::instrument(name = "Getting the pools of", skip_all)]
fn pool_choices(context: &AmpContext) -> color_eyre::eyre::Result<Vec<PoolChoice>> {
    tracing::Span::current().pb_set_message(&context.dex_id.to_string());
    let pools_by_network = crate::chain::networks::read_on_every_network(
        &context.global_context.config,
        |network_config, block_reference| {
            let block_reference =
                crate::chain::engine::engine_view_block(network_config, block_reference)?;
            let metadata_cache = AssetMetadataCache::new(network_config, block_reference.clone());
            let mut choices = Vec::new();
            for (pool_id, pool) in
                crate::chain::amp::all_pools(network_config, &block_reference, &context.dex_id)?
            {
                choices.push(PoolChoice {
                    pool_id,
                    label: presentation::pool_label(pool_id, &pool, &metadata_cache)?,
                });
            }
            Ok(choices)
        },
    )?;
    let is_single_network = pools_by_network.len() == 1;
    Ok(pools_by_network
        .into_iter()
        .flat_map(|(network_name, choices)| {
            choices.into_iter().map(move |choice| PoolChoice {
                pool_id: choice.pool_id,
                label: if is_single_network {
                    choice.label
                } else {
                    format!("[{network_name}] {}", choice.label)
                },
            })
        })
        .collect())
}

pub fn input_pool_id(context: &AmpContext) -> color_eyre::eyre::Result<Option<PoolIdArg>> {
    let choices = pool_choices(context)?;
    if choices.is_empty() {
        bail!("{} has no pools", context.dex_id);
    }
    match inquire::Select::new("Which pool? (type to filter)", choices)
        .with_page_size(15)
        .prompt()
    {
        Ok(choice) => Ok(Some(PoolIdArg(choice.pool_id))),
        Err(
            inquire::error::InquireError::OperationCanceled
            | inquire::error::InquireError::OperationInterrupted,
        ) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The tool takes pool ids as xyk's, and amp's are the same numbers
fn amp_pool_id(pool_id: PoolId) -> amp_dex_types::PoolId {
    amp_dex_types::PoolId::new(pool_id.get())
}

/// Asks for an account that signs as the owner of the dex, its deployer
fn input_dex_owner(
    context: &AmpContext,
    question: &str,
) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
    near_cli_rs::common::input_signer_account_id_from_used_account_list(
        &context.global_context.config.credentials_home_dir,
        &format!("{question} It has to be {}", context.dex_id.deployer),
    )
}

/// Only the deployer of an amp dex creates its pools and changes their
/// amplification and oracle
fn ensure_dex_owner(
    preflight: &Preflight,
    dex_id: &DexId,
    action: &'static str,
) -> color_eyre::eyre::Result<()> {
    if preflight.signer_id != dex_id.deployer {
        return Err(PreflightError::NotDexOwner {
            dex_id: dex_id.clone(),
            signer_id: preflight.signer_id.clone(),
            action,
        }
        .into_report());
    }
    Ok(())
}

/// The fees of a pool of `assets` with a fee of `fee_millionths`, and oracle fees for
/// `oracle_fee_receiver` that name their assets in either order, which can be zero
fn fee_configuration(
    fee_millionths: u32,
    oracle_fees: &(AmountArg, AmountArg),
    oracle_fee_receiver: AccountId,
    assets: &[(AssetId, Option<AssetMetadata>); 2],
    pair_label: &str,
) -> color_eyre::eyre::Result<FeeConfiguration> {
    let fraction = FeeFraction::new(fee_millionths);
    if fraction >= FULL_FEE_FRACTION {
        return Err(PreflightError::FeesNotAllowed {
            problem: "the fee must be less than 100%".to_string(),
        }
        .into_report());
    }
    let (first_asset_id, first_fixed_fee) = oracle_fees.0.resolve(assets)?;
    let (second_asset_id, second_fixed_fee) = oracle_fees.1.resolve(assets)?;
    if first_asset_id == second_asset_id {
        return Err(PreflightError::AmountsOfOneAsset {
            pair: pair_label.to_string(),
        }
        .into_report());
    }
    let (fixed_fee_0, fixed_fee_1) = if first_asset_id == assets[0].0 {
        (first_fixed_fee, second_fixed_fee)
    } else {
        (second_fixed_fee, first_fixed_fee)
    };
    Ok(FeeConfiguration {
        fraction,
        oracle_fee_per_swap: (TokenAmount::new(fixed_fee_0), TokenAmount::new(fixed_fee_1)),
        oracle_fee_receiver,
    })
}

/// A pool that a write changes, with the metadata of its two assets
pub struct PoolToWrite {
    pub pool: PoolView,
    pub assets: [(AssetId, Option<AssetMetadata>); 2],
}

impl PoolToWrite {
    pub fn read(
        preflight: &Preflight,
        dex_id: &DexId,
        pool_id: PoolId,
    ) -> color_eyre::eyre::Result<Self> {
        let pool = crate::chain::amp::pool(
            &preflight.network_config,
            &preflight.block_reference,
            dex_id,
            pool_id,
        )?;
        let metadata_cache =
            AssetMetadataCache::new(&preflight.network_config, preflight.block_reference.clone());
        let [(asset_id_0, _), (asset_id_1, _)] = presentation::pool_assets(&pool);
        let metadata_0 = metadata_cache.get(&asset_id_0)?;
        let metadata_1 = metadata_cache.get(&asset_id_1)?;
        Ok(Self {
            pool,
            assets: [(asset_id_0, metadata_0), (asset_id_1, metadata_1)],
        })
    }

    pub fn pair_label(&self) -> String {
        let [(asset_id_0, metadata_0), (asset_id_1, metadata_1)] = &self.assets;
        format!(
            "{} / {}",
            crate::display::asset_label(asset_id_0, metadata_0.as_ref()),
            crate::display::asset_label(asset_id_1, metadata_1.as_ref())
        )
    }

    pub fn format_amount(&self, side: usize, raw_amount: u128) -> String {
        let (_, metadata) = &self.assets[side];
        crate::display::format_amount(raw_amount, metadata.as_ref())
    }
}
