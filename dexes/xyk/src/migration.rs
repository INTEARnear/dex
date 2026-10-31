use std::collections::HashMap;

use intear_dex_types::{AssetId, Caller, env, expect};
use near_sdk::{
    AccountId, IntoStorageKey, NearToken, PublicKey, assert_one_yocto,
    json_types::U128,
    near,
    store::{LookupMap, Vector},
};
use xyk_dex_types::{
    AssetWithBalance, CAN_MIGRATE, ClaimMode, CurrentFees, FeeAmount, FeeConfigurationV2,
    FeeGrowth, FeePayoutType, FeeReceiver, LiquidityPosition, MigrateArgs, PayoutGroup,
    PayoutGroups, Pool, PoolAssets, PoolType, Shares, SharesBalance,
};

use crate::{StorageKey, XykDex, XykDexExt, start_fee_balances};

#[near]
impl XykDex {
    #[init(ignore_state)]
    #[payable]
    pub fn migrate(
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> Self {
        assert_one_yocto();
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        expect!(caller_id == CAN_MIGRATE, "Only owner can migrate");
        let Ok(MigrateArgs { providers }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");

        #[near(serializers=[borsh])]
        struct OldXykDex {
            pools: Vector<Pool>,
            fees_collected_by_users: LookupMap<(AccountId, AssetId), U128>,
            _unused_referral_settings: LookupMap<AccountId, ()>,
            _community_owned_fees: LookupMap<AccountId, NearToken>,
            fee_discount_signer_public_key: Option<PublicKey>,
        }

        /// The fee configurations of pools before the migration
        #[near(serializers=[borsh])]
        enum OldFeeConfiguration {
            V1(CurrentFees),
            V2(FeeConfigurationV2),
        }

        /// The pools as they were stored, with their borsh tags, but for LaunchV1, which is
        /// LaunchV2 without its quote asset
        #[near(serializers=[borsh])]
        #[borsh(use_discriminant = true)]
        #[repr(u8)]
        enum OldPool {
            PrivateV1 {
                assets: (AssetWithBalance, AssetWithBalance),
                owner_id: AccountId,
                fees: CurrentFees,
            } = 0,
            PublicV1 {
                assets: (AssetWithBalance, AssetWithBalance),
                fees: CurrentFees,
                user_shares: LookupMap<AccountId, Option<SharesBalance>>,
                total_shares: Option<SharesBalance>,
            } = 1,
            PrivateV2 {
                assets: (AssetWithBalance, AssetWithBalance),
                owner_id: AccountId,
                fees: OldFeeConfiguration,
                locked: bool,
            } = 3,
            PublicV2 {
                assets: (AssetWithBalance, AssetWithBalance),
                fees: OldFeeConfiguration,
                user_shares: LookupMap<AccountId, Option<SharesBalance>>,
                total_shares: Option<SharesBalance>,
            } = 4,
            LaunchV2 {
                quote_asset: AssetWithBalance,
                launched_asset: AssetWithBalance,
                fees: OldFeeConfiguration,
                phantom_liquidity: U128,
            } = 5,
        }
        const LAUNCH_V1_TAG: u8 = 2;
        const LAUNCH_V2_TAG: u8 = 5;

        let fixed_fees = |fees: CurrentFees| FeeConfigurationV2 {
            receivers: fees
                .receivers
                .into_iter()
                .map(|(receiver, fee_fraction)| (receiver, FeeAmount::Fixed(fee_fraction)))
                .collect(),
        };
        let fee_configuration = |fees: OldFeeConfiguration| match fees {
            OldFeeConfiguration::V1(fees) => fixed_fees(fees),
            OldFeeConfiguration::V2(fees) => fees,
        };

        // Every share keeps its fees from now on, from fee growth zero, where the providers'
        // positions start
        let public_v3 =
            |pool_id, assets, fees, total_shares: Option<SharesBalance>| Pool::PublicV3 {
                assets,
                fees,
                providers: LookupMap::new(StorageKey::PublicPoolUserShares { pool_id }),
                total_shares,
                payout_groups: Box::new(PayoutGroups {
                    keep_both: PayoutGroup {
                        shares: total_shares.map_or(0, |shares| shares.get()),
                        fee_growth: FeeGrowth::default(),
                    },
                    ..Default::default()
                }),
            };

        let old_state: OldXykDex = near_sdk::env::state_read().expect("State read failed");
        // Each pool is read and written by itself, as the vector of pools would keep every pool
        // it read in memory, and there isn't enough memory for all of them
        let pools_prefix = StorageKey::Pools.into_storage_key();
        let mut providers = providers.into_iter().peekable();
        for pool_id in 0..old_state.pools.len() {
            let pool_key = [pools_prefix.as_slice(), &pool_id.to_le_bytes()].concat();
            let old_pool = near_sdk::env::storage_read(&pool_key).expect("Pool not found");
            let old_pool = match old_pool[0] {
                // Its fields after its NEAR amount are those of LaunchV2, whose quote asset holds
                // that amount
                LAUNCH_V1_TAG => near_sdk::borsh::from_slice(
                    &[
                        [LAUNCH_V2_TAG].as_slice(),
                        &near_sdk::borsh::to_vec(&AssetId::Near).unwrap(),
                        &old_pool[1..],
                    ]
                    .concat(),
                ),
                _ => near_sdk::borsh::from_slice(&old_pool),
            }
            .expect("Invalid pool");
            let mut pool = match old_pool {
                OldPool::PrivateV1 {
                    assets,
                    owner_id,
                    fees,
                } => Pool::PrivateV2 {
                    assets,
                    owner_id,
                    fees: fixed_fees(fees),
                    locked: false,
                },
                OldPool::PrivateV2 {
                    assets,
                    owner_id,
                    fees,
                    locked,
                } => Pool::PrivateV2 {
                    assets,
                    owner_id,
                    fees: fee_configuration(fees),
                    locked,
                },
                OldPool::PublicV1 {
                    assets,
                    fees,
                    total_shares,
                    ..
                } => public_v3(pool_id, assets, fixed_fees(fees), total_shares),
                OldPool::PublicV2 {
                    assets,
                    fees,
                    total_shares,
                    ..
                } => public_v3(pool_id, assets, fee_configuration(fees), total_shares),
                OldPool::LaunchV2 {
                    quote_asset,
                    launched_asset,
                    fees,
                    phantom_liquidity,
                } => Pool::LaunchV2 {
                    quote_asset,
                    launched_asset,
                    fees: fee_configuration(fees),
                    phantom_liquidity,
                },
            };

            // The accounts and communities, which read as accounts that keep both assets, take
            // their fees as they did, with the legacy payout of the pool's type, and a community's
            // fees go to balances that anyone can claim, which start here with what it collected
            let legacy_payout_type = if PoolType::from(&pool).is_launch() {
                FeePayoutType::ConvertToFirstAsset
            } else {
                FeePayoutType::KeepBoth
            };
            let (asset_id_0, asset_id_1) = pool.asset_ids();
            let asset_ids = PoolAssets([asset_id_0.clone(), asset_id_1.clone()]);
            let (Pool::PrivateV2 { fees, .. }
            | Pool::LaunchV2 { fees, .. }
            | Pool::PublicV3 { fees, .. }) = &mut pool;
            for (receiver, _) in &mut fees.receivers {
                if let FeeReceiver::Account { payout_type, .. } = receiver {
                    *payout_type = legacy_payout_type;
                }
            }
            // Maps of its own for each pool, as the dex's would keep every balance they read
            let mut fees_claimable_by_anyone: LookupMap<(AccountId, AssetId), U128> =
                LookupMap::new(StorageKey::FeesClaimableByAnyone);
            let mut community_owned_fees: LookupMap<AccountId, NearToken> =
                LookupMap::new(StorageKey::UnusedCommunityOwnedFees);
            for (receiver, _) in fees.receivers_at(near_sdk::env::block_timestamp()) {
                if let FeeReceiver::Account {
                    account_id,
                    payout_type,
                    claim_mode: ClaimMode::Anyone,
                } = receiver
                {
                    start_fee_balances(
                        &mut fees_claimable_by_anyone,
                        &account_id,
                        asset_ids.each_ref(),
                        payout_type,
                    );
                    // Unless an earlier pool of the community moved it
                    if let Some(community_fees) = community_owned_fees.remove(&account_id) {
                        let claimable_near = fees_claimable_by_anyone
                            .get_mut(&(account_id, AssetId::Near))
                            .expect("Communities collect their fees in NEAR");
                        claimable_near.0 = claimable_near
                            .0
                            .checked_add(community_fees.as_yoctonear())
                            .expect("Overflow");
                    }
                }
            }
            fees_claimable_by_anyone.flush();
            community_owned_fees.flush();

            if let Pool::PublicV3 {
                assets,
                providers: pool_providers,
                total_shares,
                ..
            } = &mut pool
            {
                let account_ids = providers
                    .next_if(|(providers_pool_id, _)| *providers_pool_id == pool_id)
                    .map_or(Vec::new(), |(_, account_ids)| account_ids);
                let shares_before_migration: LookupMap<AccountId, Option<SharesBalance>> =
                    LookupMap::new(StorageKey::PublicPoolUserShares { pool_id });
                // A map of its own for each pool, as the dex's would keep every balance it reads
                let mut fees_collected_by_users = LookupMap::new(StorageKey::FeesCollectedByUsers);
                let mut providers_shares: Shares = 0;
                for account_id in account_ids {
                    expect!(
                        let Some(shares) = shares_before_migration.get(&account_id),
                        "{account_id} isn't a liquidity provider of pool {pool_id}"
                    );
                    let shares = shares.map_or(0, |shares| shares.get());
                    providers_shares = providers_shares.checked_add(shares).expect("Overflow");
                    // Unlike insert, set doesn't read the entry it replaces, which is shares
                    pool_providers.set(
                        account_id.clone(),
                        Some(LiquidityPosition {
                            payout_type: FeePayoutType::KeepBoth,
                            shares,
                            fee_growth_collected: FeeGrowth::default(),
                        }),
                    );
                    start_fee_balances(
                        &mut fees_collected_by_users,
                        &account_id,
                        PoolAssets([&assets.0.asset_id, &assets.1.asset_id]),
                        FeePayoutType::KeepBoth,
                    );
                }
                // A provider left out would have shares and no position
                expect!(
                    providers_shares == total_shares.map_or(0, |shares| shares.get()),
                    "The providers of pool {pool_id} don't have all of its shares"
                );
                pool_providers.flush();
                fees_collected_by_users.flush();
            }
            near_sdk::env::storage_write(&pool_key, &near_sdk::borsh::to_vec(&pool).unwrap());
        }
        expect!(
            providers.next().is_none(),
            "Providers must be of public pools from before the migration, in order of pool id"
        );

        Self {
            pools: old_state.pools,
            fees_collected_by_users: old_state.fees_collected_by_users,
            fee_discount_signer_public_key: old_state.fee_discount_signer_public_key,
            fees_claimable_by_anyone: LookupMap::new(StorageKey::FeesClaimableByAnyone),
        }
    }
}
