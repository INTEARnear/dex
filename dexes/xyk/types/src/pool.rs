use intear_dex_types::AssetId;
use near_sdk::{AccountId, Timestamp, json_types::U128, near, store::LookupMap};

use crate::{
    AssetWithBalance, CurrentFees, FeeConfigurationV2, FeePayoutType, FeeReceiver,
    LiquidityPosition, PayoutGroups, PoolType, PoolView, SharesBalance, protocol_fee_payout,
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
        phantom_liquidity: U128,
    } = 5,
    PublicV3 {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: FeeConfigurationV2,
        providers: LookupMap<AccountId, LiquidityPosition>,
        total_shares: Option<SharesBalance>,
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

    pub fn balances(&self) -> [u128; 2] {
        match self {
            Pool::PrivateV2 { assets, .. } | Pool::PublicV3 { assets, .. } => {
                [assets.0.balance.0, assets.1.balance.0]
            }
            Pool::LaunchV2 {
                quote_asset,
                launched_asset,
                ..
            } => [quote_asset.balance.0, launched_asset.balance.0],
        }
    }

    pub fn balances_mut(&mut self) -> [&mut u128; 2] {
        match self {
            Pool::PrivateV2 { assets, .. } | Pool::PublicV3 { assets, .. } => {
                [&mut assets.0.balance.0, &mut assets.1.balance.0]
            }
            Pool::LaunchV2 {
                quote_asset,
                launched_asset,
                ..
            } => [&mut quote_asset.balance.0, &mut launched_asset.balance.0],
        }
    }

    /// The pool fees, without the protocol fee
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
    pub fn liquidity_shares(&self, account_id: &AccountId) -> Option<u128> {
        match self {
            Pool::PublicV3 { providers, .. } => {
                providers.get(account_id).map(|position| position.shares)
            }
            Pool::PrivateV2 { .. } | Pool::LaunchV2 { .. } => None,
        }
    }

    /// The fees of a swap at `timestamp`, with the protocol fee
    pub fn current_fees(&self, timestamp: Timestamp) -> CurrentFees {
        let (asset_0, asset_1) = self.asset_ids();
        self.fee_configuration()
            .with_protocol_fee(&[asset_0, asset_1], timestamp)
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
                total_shares: total_shares.map(|s| U128(s.get())),
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
