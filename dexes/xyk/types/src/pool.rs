use intear_dex_types::AssetId;
use near_sdk::{AccountId, Timestamp, json_types::U128, near, store::LookupMap};

use crate::{
    AssetWithBalance, CurrentFees, FeeConfiguration, PoolType, PoolView, SharesBalance,
    asset_account_ids,
};

#[near(serializers=[borsh])]
pub enum Pool {
    PrivateV1 {
        assets: (AssetWithBalance, AssetWithBalance),
        owner_id: AccountId,
        fees: CurrentFees,
    },
    PublicV1 {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: CurrentFees,
        user_shares: LookupMap<AccountId, Option<SharesBalance>>,
        total_shares: Option<SharesBalance>,
    },
    LaunchV1 {
        near_amount: U128,
        launched_asset: AssetWithBalance,
        fees: FeeConfiguration,
        phantom_liquidity_near: U128,
    },
    PrivateV2 {
        assets: (AssetWithBalance, AssetWithBalance),
        owner_id: AccountId,
        fees: FeeConfiguration,
        locked: bool,
    },
    PublicV2 {
        assets: (AssetWithBalance, AssetWithBalance),
        fees: FeeConfiguration,
        user_shares: LookupMap<AccountId, Option<SharesBalance>>,
        total_shares: Option<SharesBalance>,
    },
    /// The first asset is the quote asset, which the phantom liquidity is in
    LaunchV2 {
        quote_asset: AssetWithBalance,
        launched_asset: AssetWithBalance,
        fees: FeeConfiguration,
        phantom_liquidity: U128,
    },
}

impl Pool {
    pub fn asset_ids(&self) -> (&AssetId, &AssetId) {
        match self {
            Pool::PrivateV1 { assets, .. }
            | Pool::PublicV1 { assets, .. }
            | Pool::PrivateV2 { assets, .. }
            | Pool::PublicV2 { assets, .. } => (&assets.0.asset_id, &assets.1.asset_id),
            Pool::LaunchV1 { launched_asset, .. } => (&AssetId::Near, &launched_asset.asset_id),
            Pool::LaunchV2 {
                quote_asset,
                launched_asset,
                ..
            } => (&quote_asset.asset_id, &launched_asset.asset_id),
        }
    }

    pub fn balances(&self) -> [u128; 2] {
        match self {
            Pool::PrivateV1 { assets, .. }
            | Pool::PublicV1 { assets, .. }
            | Pool::PrivateV2 { assets, .. }
            | Pool::PublicV2 { assets, .. } => [assets.0.balance.0, assets.1.balance.0],
            Pool::LaunchV1 {
                near_amount,
                launched_asset,
                ..
            } => [near_amount.0, launched_asset.balance.0],
            Pool::LaunchV2 {
                quote_asset,
                launched_asset,
                ..
            } => [quote_asset.balance.0, launched_asset.balance.0],
        }
    }

    pub fn balances_mut(&mut self) -> [&mut u128; 2] {
        match self {
            Pool::PrivateV1 { assets, .. }
            | Pool::PublicV1 { assets, .. }
            | Pool::PrivateV2 { assets, .. }
            | Pool::PublicV2 { assets, .. } => [&mut assets.0.balance.0, &mut assets.1.balance.0],
            Pool::LaunchV1 {
                near_amount,
                launched_asset,
                ..
            } => [&mut near_amount.0, &mut launched_asset.balance.0],
            Pool::LaunchV2 {
                quote_asset,
                launched_asset,
                ..
            } => [&mut quote_asset.balance.0, &mut launched_asset.balance.0],
        }
    }

    /// The pool fees, without the protocol fee
    pub fn fee_configuration(&self) -> FeeConfiguration {
        match self {
            Pool::PrivateV1 { fees, .. } | Pool::PublicV1 { fees, .. } => {
                FeeConfiguration::V1(fees.clone())
            }
            Pool::LaunchV1 { fees, .. }
            | Pool::PrivateV2 { fees, .. }
            | Pool::PublicV2 { fees, .. }
            | Pool::LaunchV2 { fees, .. } => fees.clone(),
        }
    }

    /// The fees of a swap at `timestamp`, with the protocol fee
    pub fn current_fees(&self, timestamp: Timestamp) -> CurrentFees {
        let (asset_0, asset_1) = self.asset_ids();
        self.fee_configuration()
            .with_protocol_fee(&asset_account_ids([asset_0, asset_1]), timestamp)
    }

    /// The pool with its fees at `timestamp`, as views and events show it
    pub fn view(&self, timestamp: Timestamp) -> PoolView {
        let fees = self.current_fees(timestamp);
        let fee_configuration = self.fee_configuration();
        match self {
            Pool::PrivateV1 {
                assets, owner_id, ..
            } => PoolView::Private {
                assets: assets.clone(),
                fees,
                fee_configuration,
                owner_id: owner_id.clone(),
                locked: false,
            },
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
            Pool::PublicV1 {
                assets,
                total_shares,
                ..
            }
            | Pool::PublicV2 {
                assets,
                total_shares,
                ..
            } => PoolView::Public {
                assets: assets.clone(),
                fees,
                fee_configuration,
                total_shares: total_shares.map(|s| U128(s.get())),
            },
            Pool::LaunchV1 {
                near_amount,
                launched_asset,
                phantom_liquidity_near,
                ..
            } => PoolView::Launch {
                near_amount: *near_amount,
                launched_asset: launched_asset.clone(),
                fees,
                fee_configuration,
                phantom_liquidity_near: *phantom_liquidity_near,
            },
            Pool::LaunchV2 {
                quote_asset,
                launched_asset,
                phantom_liquidity,
                ..
            } => PoolView::LaunchV2 {
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
            Pool::PrivateV1 { .. } => PoolType::PrivateV1,
            Pool::PublicV1 { .. } => PoolType::PublicV1,
            Pool::LaunchV1 {
                phantom_liquidity_near,
                ..
            } => PoolType::LaunchV1 {
                phantom_liquidity_near: *phantom_liquidity_near,
            },
            Pool::PrivateV2 { .. } => PoolType::PrivateV2,
            Pool::PublicV2 { .. } => PoolType::PublicV2,
            Pool::LaunchV2 {
                phantom_liquidity, ..
            } => PoolType::LaunchV2 {
                phantom_liquidity: *phantom_liquidity,
            },
        }
    }
}
