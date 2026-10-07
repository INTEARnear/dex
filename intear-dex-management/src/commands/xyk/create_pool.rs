use intear_dex_types::{AccountOrDexId, AssetId, DexId};
use near_cli_rs::commands::ActionContext;
use near_primitives::gas::Gas;
use near_primitives::types::AccountId;
use near_sdk::NearToken;
use near_sdk::json_types::U128;
use serde_json::json;
use strum::{EnumDiscriminants, EnumIter, EnumMessage};
use xyk_dex_types::{CreatePoolArgs, FeeReceiver, PROTOCOL_FEE_RECEIVER_ID, PoolType};

use super::XykContext;
use super::presentation::format_fee_configuration;
use crate::chain::asset_metadata::AssetMetadataCache;
use crate::deployment::ENGINE_ACCOUNT_ID;
use crate::display;
use crate::errors::{self, PreflightError};
use crate::inputs::amount::AmountArg;
use crate::inputs::asset_ids::AssetIdArg;
use crate::inputs::fees::FeesArg;
use crate::outcome::ExpectedOutcome;
use crate::planning::xyk_storage::{self, NewPoolKind};
use crate::planning::{self, FunctionCall, TransactionPlan, format_near, storage};

const CREATE_POOL_GAS: Gas = Gas::from_teragas(100);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(context = XykContext)]
pub struct CreatePool {
    #[interactive_clap(subcommand)]
    kind: CreatePoolKind,
}

#[derive(Debug, EnumDiscriminants, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = XykContext)]
#[interactive_clap(output_context = CreatePoolKindContext)]
#[strum_discriminants(derive(EnumMessage, EnumIter))]
/// Which kind of pool?
pub enum CreatePoolKind {
    #[strum_discriminants(strum(
        message = "private  - Only its owner adds and removes liquidity and changes its fees"
    ))]
    /// Only its owner adds and removes liquidity and changes its fees
    Private(TwoAssetPool),
    #[strum_discriminants(strum(
        message = "public   - Anyone adds liquidity for shares, and its fees never change"
    ))]
    /// Anyone adds liquidity for shares, and its fees never change
    Public(TwoAssetPool),
    #[strum_discriminants(strum(
        message = "launch   - A token against NEAR, priced from phantom NEAR liquidity; its liquidity stays"
    ))]
    /// A token against NEAR, priced from phantom NEAR liquidity; its liquidity stays
    Launch(LaunchPool),
}

#[derive(Clone)]
pub struct CreatePoolKindContext {
    xyk_context: XykContext,
    kind: CreatePoolKindDiscriminants,
}

impl CreatePoolKindContext {
    pub fn from_previous_context(
        previous_context: XykContext,
        scope: &<CreatePoolKind as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self {
            xyk_context: previous_context,
            kind: *scope,
        })
    }
}

fn input_creator(
    context: &CreatePoolKindContext,
) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
    near_cli_rs::common::input_signer_account_id_from_used_account_list(
        &context
            .xyk_context
            .global_context
            .config
            .credentials_home_dir,
        "Which account creates the pool and pays for its storage?",
    )
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = CreatePoolKindContext)]
#[interactive_clap(output_context = TwoAssetPoolContext)]
pub struct TwoAssetPool {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account creates the pool and pays for its storage?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The first asset: near, nep141:<token contract> or nep245:<contract>:<token>
    asset_0: AssetIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// The second asset
    asset_1: AssetIdArg,
    #[interactive_clap(named_arg)]
    /// The fees of the pool
    fees: PoolFees,
}

impl TwoAssetPool {
    fn input_signer_account_id(
        context: &CreatePoolKindContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        input_creator(context)
    }

    fn input_asset_0(
        _context: &CreatePoolKindContext,
    ) -> color_eyre::eyre::Result<Option<AssetIdArg>> {
        crate::inputs::prompt("The first asset? e.g. near or nep141:usdt.tether-token.near")
    }

    fn input_asset_1(
        _context: &CreatePoolKindContext,
    ) -> color_eyre::eyre::Result<Option<AssetIdArg>> {
        crate::inputs::prompt("The second asset?")
    }
}

#[derive(Clone)]
pub struct TwoAssetPoolContext(NewPoolContext);

impl TwoAssetPoolContext {
    pub fn from_previous_context(
        previous_context: CreatePoolKindContext,
        scope: &<TwoAssetPool as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let AssetIdArg(asset_0) = scope.asset_0.clone();
        let AssetIdArg(asset_1) = scope.asset_1.clone();
        Ok(Self(NewPoolContext {
            global_context: previous_context.xyk_context.global_context,
            dex_id: previous_context.xyk_context.dex_id,
            signer_id: scope.signer_account_id.clone().into(),
            new_pool: NewPool::TwoAssets {
                is_private: matches!(previous_context.kind, CreatePoolKindDiscriminants::Private),
                assets: (asset_0, asset_1),
            },
        }))
    }
}

impl From<TwoAssetPoolContext> for NewPoolContext {
    fn from(item: TwoAssetPoolContext) -> Self {
        item.0
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = CreatePoolKindContext)]
#[interactive_clap(output_context = LaunchPoolContext)]
pub struct LaunchPool {
    #[interactive_clap(skip_default_input_arg)]
    /// Which account creates the pool, puts the token in and pays for its storage?
    signer_account_id: near_cli_rs::types::account_id::AccountId,
    #[interactive_clap(skip_default_input_arg)]
    /// The token to launch: nep141:<token contract> or nep245:<contract>:<token>
    token: AssetIdArg,
    #[interactive_clap(skip_default_input_arg)]
    /// How much of the token goes into the pool, from the signer's balance on dex.intear.near
    token_amount: AmountArg,
    #[interactive_clap(named_arg)]
    /// The NEAR liquidity the token is priced against, which nobody can withdraw
    phantom_liquidity: PhantomLiquidity,
}

impl LaunchPool {
    fn input_signer_account_id(
        context: &CreatePoolKindContext,
    ) -> color_eyre::eyre::Result<Option<near_cli_rs::types::account_id::AccountId>> {
        input_creator(context)
    }

    fn input_token(
        _context: &CreatePoolKindContext,
    ) -> color_eyre::eyre::Result<Option<AssetIdArg>> {
        crate::inputs::prompt("Which token? e.g. nep141:meme.near")
    }

    fn input_token_amount(
        _context: &CreatePoolKindContext,
    ) -> color_eyre::eyre::Result<Option<AmountArg>> {
        crate::inputs::prompt("How much of the token goes into the pool? e.g. '1000000000 MEME'")
    }
}

#[derive(Clone)]
pub struct LaunchPoolContext {
    global_context: crate::GlobalContext,
    dex_id: DexId,
    signer_id: AccountId,
    token: AssetId,
    token_amount: AmountArg,
}

impl LaunchPoolContext {
    pub fn from_previous_context(
        previous_context: CreatePoolKindContext,
        scope: &<LaunchPool as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let AssetIdArg(token) = scope.token.clone();
        Ok(Self {
            global_context: previous_context.xyk_context.global_context,
            dex_id: previous_context.xyk_context.dex_id,
            signer_id: scope.signer_account_id.clone().into(),
            token,
            token_amount: scope.token_amount.clone(),
        })
    }
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = LaunchPoolContext)]
#[interactive_clap(output_context = PhantomLiquidityContext)]
pub struct PhantomLiquidity {
    /// How much NEAR of phantom liquidity, e.g. '100 NEAR'
    near: near_cli_rs::types::near_token::NearToken,
    #[interactive_clap(named_arg)]
    /// The fees of the pool
    fees: PoolFees,
}

#[derive(Clone)]
pub struct PhantomLiquidityContext(NewPoolContext);

impl PhantomLiquidityContext {
    pub fn from_previous_context(
        previous_context: LaunchPoolContext,
        scope: &<PhantomLiquidity as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let near_cli_rs::types::near_token::NearToken(phantom_liquidity) = scope.near;
        Ok(Self(NewPoolContext {
            global_context: previous_context.global_context,
            dex_id: previous_context.dex_id,
            signer_id: previous_context.signer_id,
            new_pool: NewPool::Launch {
                token: previous_context.token,
                token_amount: previous_context.token_amount,
                phantom_liquidity,
            },
        }))
    }
}

impl From<PhantomLiquidityContext> for NewPoolContext {
    fn from(item: PhantomLiquidityContext) -> Self {
        item.0
    }
}

#[derive(Clone)]
enum NewPool {
    TwoAssets {
        is_private: bool,
        assets: (AssetId, AssetId),
    },
    Launch {
        token: AssetId,
        token_amount: AmountArg,
        phantom_liquidity: NearToken,
    },
}

#[derive(Clone)]
pub struct NewPoolContext {
    global_context: crate::GlobalContext,
    dex_id: DexId,
    signer_id: AccountId,
    new_pool: NewPool,
}

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = NewPoolContext)]
#[interactive_clap(output_context = PoolFeesContext)]
pub struct PoolFees {
    #[interactive_clap(skip_default_input_arg)]
    /// The fees, e.g. 'alice.near=0.25%,pool=0.05%', a schedule like 'community:dao.near=1%..0.3%@now..now+7d', or none
    fees: FeesArg,
    #[interactive_clap(named_arg)]
    /// Select network
    network_config: near_cli_rs::network_for_transaction::NetworkForTransactionArgs,
}

impl PoolFees {
    fn input_fees(_context: &NewPoolContext) -> color_eyre::eyre::Result<Option<FeesArg>> {
        crate::inputs::prompt(
            "The fees? e.g. 'alice.near=0.25%,pool=0.05%', 'community:dao.near=1%..0.3%@now..now+7d', or none",
        )
    }
}

fn ensure_poolable(asset_id: &AssetId) -> color_eyre::eyre::Result<()> {
    match asset_id {
        AssetId::Near | AssetId::Nep141(_) | AssetId::Nep245(_, _) => Ok(()),
        AssetId::Nep171(_, _) => Err(PreflightError::UnsupportedPoolAsset {
            asset_id: asset_id.clone(),
        }
        .into_report()),
    }
}

#[derive(Clone)]
pub struct PoolFeesContext(ActionContext);

impl PoolFeesContext {
    pub fn from_previous_context(
        previous_context: NewPoolContext,
        scope: &<PoolFees as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        let NewPoolContext {
            global_context,
            dex_id,
            signer_id,
            new_pool,
        } = previous_context;
        let fees = scope.fees.clone();
        Ok(Self(planning::write_action_context(
            &global_context,
            signer_id,
            move |preflight| {
                preflight.ensure_engine_not_paused()?;
                preflight.ensure_dex_exists(&dex_id)?;
                let signer_id = preflight.signer_id.clone();
                let metadata_cache = AssetMetadataCache::new(
                    &preflight.network_config,
                    preflight.block_reference.clone(),
                );
                let mut attached_assets = Vec::new();
                let mut extra_steps = Vec::new();
                let (assets, pool_type, kind_label, new_pool_kind) = match &new_pool {
                    NewPool::TwoAssets { is_private, assets } => {
                        if assets.0 == assets.1 {
                            return Err(PreflightError::SameAssets {
                                asset_id: assets.0.clone(),
                            }
                            .into_report());
                        }
                        ensure_poolable(&assets.0)?;
                        ensure_poolable(&assets.1)?;
                        if *is_private {
                            (
                                assets.clone(),
                                PoolType::PrivateLatest,
                                "private",
                                NewPoolKind::Private {
                                    owner_id: &signer_id,
                                },
                            )
                        } else {
                            (
                                assets.clone(),
                                PoolType::PublicLatest,
                                "public",
                                NewPoolKind::Public,
                            )
                        }
                    }
                    NewPool::Launch {
                        token,
                        token_amount,
                        phantom_liquidity,
                    } => {
                        if *token == AssetId::Near {
                            return Err(PreflightError::LaunchOfNear.into_report());
                        }
                        ensure_poolable(token)?;
                        if phantom_liquidity.is_zero() {
                            return Err(PreflightError::ZeroAmount.into_report());
                        }
                        let token_metadata = metadata_cache.get(token)?;
                        let (_, raw_token_amount) =
                            token_amount.resolve(&[(token.clone(), token_metadata.clone())])?;
                        if raw_token_amount == 0 {
                            return Err(PreflightError::ZeroAmount.into_report());
                        }
                        let token_amount_label =
                            display::format_amount(raw_token_amount, token_metadata.as_ref());
                        preflight.ensure_engine_balance(
                            token,
                            token_metadata.as_ref(),
                            raw_token_amount,
                        )?;
                        extra_steps.push(format!(
                            "Put {token_amount_label} from {signer_id}'s balance on {ENGINE_ACCOUNT_ID} into the pool, priced against {} of phantom liquidity",
                            format_near(*phantom_liquidity)
                        ));
                        attached_assets.push((token.clone(), raw_token_amount));
                        (
                            (AssetId::Near, token.clone()),
                            PoolType::LaunchLatest {
                                phantom_liquidity: U128(phantom_liquidity.as_yoctonear()),
                            },
                            "launch",
                            NewPoolKind::Launch,
                        )
                    }
                };
                let pair = format!(
                    "{} / {}",
                    display::asset_label(&assets.0, metadata_cache.get(&assets.0)?.as_ref()),
                    display::asset_label(&assets.1, metadata_cache.get(&assets.1)?.as_ref())
                );
                let now = preflight.block_timestamp_nanoseconds;
                let fee_configuration = fees
                    .fee_configuration(now)
                    .map_err(|problem| PreflightError::FeesNotAllowed { problem }.into_report())?;
                fee_configuration
                    .validate(&pool_type, now)
                    .map_err(|error| {
                        PreflightError::FeesNotAllowed {
                            problem: errors::fee_configuration_problem(error),
                        }
                        .into_report()
                    })?;

                let extra_bytes_per_record = preflight.protocol_limits.extra_bytes_per_record;
                let mut storage_bytes = vec![xyk_storage::new_pool_bytes(
                    &dex_id,
                    new_pool_kind,
                    (&assets.0, &assets.1),
                    &fee_configuration,
                    extra_bytes_per_record,
                )?];
                // The pool starts a fee balance in both assets for the
                // protocol and every account receiver that has none yet
                let mut fee_accounts = vec![PROTOCOL_FEE_RECEIVER_ID.to_owned()];
                let mut community_accounts: Vec<AccountId> = Vec::new();
                for (receiver, _) in fee_configuration.receivers_at(now) {
                    match receiver {
                        FeeReceiver::Account(account_id) => {
                            if !fee_accounts.contains(&account_id) {
                                fee_accounts.push(account_id);
                            }
                        }
                        // Whether a community account has a balance can't
                        // be read, so it's counted as new
                        FeeReceiver::Community(account_id) => {
                            if !community_accounts.contains(&account_id) {
                                community_accounts.push(account_id);
                            }
                        }
                        FeeReceiver::Pool => {}
                    }
                }
                storage_bytes.push(super::new_fee_balances_bytes(
                    preflight,
                    &dex_id,
                    &fee_accounts,
                    (&assets.0, &assets.1),
                )?);
                for account_id in &community_accounts {
                    storage_bytes.push(xyk_storage::community_fee_balance_bytes(
                        &dex_id,
                        account_id,
                        extra_bytes_per_record,
                    )?);
                }
                let total_storage_bytes = storage_bytes
                    .iter()
                    .try_fold(0u64, |total, bytes| total.checked_add(*bytes))
                    .ok_or_else(|| color_eyre::eyre::eyre!("Storage size overflow"))?;
                let storage_near = storage::storage_cost(
                    total_storage_bytes,
                    preflight.protocol_limits.storage_byte_cost,
                )?;
                attached_assets.insert(0, (AssetId::Near, storage_near.as_yoctonear()));

                let mut registrations =
                    super::storage_near_registrations(&signer_id, &dex_id).to_vec();
                // The dex holds the pool's assets
                registrations.push((AccountOrDexId::Dex(dex_id.clone()), assets.0.clone()));
                registrations.push((AccountOrDexId::Dex(dex_id.clone()), assets.1.clone()));
                let registration_calls = preflight.registration_calls(&registrations)?;
                let mut steps = registration_calls.steps;
                let mut function_calls = registration_calls.function_calls;
                steps.push(format!(
                    "Create a {kind_label} {pair} pool on {dex_id} with fees {}, plus the protocol fee",
                    format_fee_configuration(&fee_configuration)
                ));
                steps.extend(extra_steps);
                steps.push(format!(
                    "Attach {} from {signer_id}'s wallet for the pool's storage, which takes about that; {dex_id} sends back what it doesn't use",
                    format_near(storage_near)
                ));
                function_calls.push(FunctionCall {
                    method_name: "execute_operations",
                    args: json!({
                        "operations": [planning::dex_call_operation(
                            &dex_id,
                            "create_pool",
                            &CreatePoolArgs {
                                assets: assets.clone(),
                                fees: fee_configuration,
                                pool_type,
                            },
                            &attached_assets,
                        )?],
                    }),
                    deposit: storage_near,
                    gas: CREATE_POOL_GAS,
                });
                Ok(TransactionPlan {
                    receiver_id: ENGINE_ACCOUNT_ID.to_owned(),
                    steps,
                    function_calls,
                    expected_outcome: ExpectedOutcome {
                        action_name: "pool creation",
                        success_message: format!("Created a {kind_label} {pair} pool on {dex_id}"),
                        transfer_call: None,
                        dex_id: Some(dex_id.clone()),
                        deployment: None,
                    },
                })
            },
        )))
    }
}

impl From<PoolFeesContext> for ActionContext {
    fn from(item: PoolFeesContext) -> Self {
        item.0
    }
}
