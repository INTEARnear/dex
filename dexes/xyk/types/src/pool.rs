use intear_dex_types::AssetId;
use near_sdk::{AccountId, Timestamp, near, store::LookupMap};

use crate::{
    AssetWithBalance, CurrentFees, FeeConfigurationV2, FeePayoutType, FeeReceiver,
    LiquidityPosition, PayoutGroups, PoolAssets, PoolType, PoolView, ProviderShares, TokenAmount,
    TotalShares, protocol_fee_payout,
};

#[near(serializers=[borsh])]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum Pool {
    PrivateV2 {
        assets: (AssetWithBalance, AssetWithBalance),
        owner_id: AccountId,
        fees: FeeConfigurationV2,
        locked: bool,
    } = 3,
    LaunchV2 {
        quote_asset: AssetWithBalance,
        launched_asset: AssetWithBalance,
        fees: FeeConfigurationV2,
        phantom_liquidity: TokenAmount,
    } = 5,
    PublicV3 {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: FeeConfigurationV2,
        providers: LookupMap<AccountId, LiquidityPosition>,
        total_shares: TotalShares,
        payout_groups: Box<PayoutGroups>,
    } = 6,
}

impl Pool {
    pub fn asset_ids(&self) -> (&AssetId, &AssetId) {
        match self {
            Pool::PrivateV2 { assets, .. } | Pool::PublicV3 { assets, .. } => {
                (&assets.0.asset_id, &assets.1.asset_id)
            }
            Pool::LaunchV2 {
                quote_asset,
                launched_asset,
                ..
            } => (&quote_asset.asset_id, &launched_asset.asset_id),
        }
    }

    fn assets_mut(&mut self) -> [&mut AssetWithBalance; 2] {
        match self {
            Pool::PrivateV2 { assets, .. } | Pool::PublicV3 { assets, .. } => {
                [&mut assets.0, &mut assets.1]
            }
            Pool::LaunchV2 {
                quote_asset,
                launched_asset,
                ..
            } => [quote_asset, launched_asset],
        }
    }

    pub fn balances(&self) -> PoolAssets<TokenAmount> {
        let (asset_0, asset_1) = match self {
            Pool::PrivateV2 { assets, .. } | Pool::PublicV3 { assets, .. } => {
                (&assets.0, &assets.1)
            }
            Pool::LaunchV2 {
                quote_asset,
                launched_asset,
                ..
            } => (quote_asset, launched_asset),
        };
        PoolAssets([asset_0.balance, asset_1.balance])
    }

    pub fn set_balances(&mut self, balances: PoolAssets<TokenAmount>) {
        for (asset, balance) in self.assets_mut().into_iter().zip(balances.0) {
            asset.balance = balance;
        }
    }

    /// The pool fees, before the protocol takes its share
    pub fn fee_configuration(&self) -> &FeeConfigurationV2 {
        match self {
            Pool::PrivateV2 { fees, .. }
            | Pool::LaunchV2 { fees, .. }
            | Pool::PublicV3 { fees, .. } => fees,
        }
    }

    /// How a `receiver` of [`Self::current_fees`] other than the pool takes its fees
    pub fn account_fee_payout_type(&self, receiver: &FeeReceiver) -> FeePayoutType {
        match receiver {
            FeeReceiver::Account { payout_type, .. } => *payout_type,
            FeeReceiver::Protocol => protocol_fee_payout(self.asset_ids()),
            FeeReceiver::Pool => panic!("The pool's liquidity providers choose their payouts"),
        }
    }

    /// The shares of a liquidity provider of a public pool, `None` if it isn't registered
    pub fn liquidity_shares(&self, account_id: &AccountId) -> Option<ProviderShares> {
        match self {
            Pool::PublicV3 { providers, .. } => {
                providers.get(account_id).map(|position| position.shares)
            }
            Pool::PrivateV2 { .. } | Pool::LaunchV2 { .. } => None,
        }
    }

    /// The fees of a swap at `timestamp`, with the protocol's share
    pub fn current_fees(&self, timestamp: Timestamp) -> CurrentFees {
        self.fee_configuration().with_protocol_fee(timestamp)
    }

    /// The pool with its fees at `timestamp`, as views and events show it
    pub fn view(&self, timestamp: Timestamp) -> PoolView {
        let fees = self.current_fees(timestamp);
        let fee_configuration = self.fee_configuration().clone();
        match self {
            Pool::PrivateV2 {
                assets,
                owner_id,
                locked,
                ..
            } => PoolView::Private {
                assets: assets.clone(),
                fees,
                fee_configuration,
                owner_id: owner_id.clone(),
                locked: *locked,
            },
            Pool::PublicV3 {
                assets,
                total_shares,
                ..
            } => PoolView::Public {
                assets: assets.clone(),
                fees,
                fee_configuration,
                total_shares: *total_shares,
            },
            Pool::LaunchV2 {
                quote_asset,
                launched_asset,
                phantom_liquidity,
                ..
            } => PoolView::Launch {
                quote_asset: quote_asset.clone(),
                launched_asset: launched_asset.clone(),
                fees,
                fee_configuration,
                phantom_liquidity: *phantom_liquidity,
            },
        }
    }
}

impl From<&Pool> for PoolType {
    fn from(pool: &Pool) -> Self {
        match pool {
            Pool::PrivateV2 { .. } => PoolType::PrivateV2,
            Pool::LaunchV2 {
                phantom_liquidity, ..
            } => PoolType::LaunchV2 {
                phantom_liquidity: *phantom_liquidity,
            },
            Pool::PublicV3 { .. } => PoolType::PublicV3,
        }
    }
}
