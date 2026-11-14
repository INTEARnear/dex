use std::collections::HashMap;

use amp_dex_types::{
    AmpDexEvent, Amplification, AssetWithBalance, FeeConfiguration, FeeFraction, LiquidityPosition,
    PROTOCOL_FEE_RECEIVER_ID, PayoutGroups, Pool, PoolId, Price, TokenAmount, TotalShares,
};
use intear_dex_common::{
    AssetId, AssetWithdrawRequest, AssetWithdrawalType, Caller, DexCallResponse, env, expect,
};
use near_sdk::{AccountId, IntoStorageKey, NearToken, PublicKeyHandle, near, store::LookupMap};

use crate::{AmpDex, AmpDexExt, StorageKey};

#[near]
impl AmpDex {
    #[payable]
    #[result_serializer(borsh)]
    pub fn migrate(
        &mut self,
        #[serializer(borsh)]
        #[allow(unused_mut)]
        mut attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        expect!(
            caller_id == env::dex_id().deployer,
            "Only the owner of the dex can migrate"
        );
        expect!(args.is_empty(), "Migrate takes no args");
        let attached_near = NearToken::from_yoctonear(
            attached_assets
                .remove(&AssetId::Near)
                .unwrap_or_default()
                .get(),
        );
        expect!(
            attached_assets.is_empty(),
            "No assets other than NEAR should be attached"
        );

        #[near(serializers=[borsh])]
        struct PoolBeforeMigration {
            assets: (AssetWithBalance, AssetWithBalance),
            decimals: (u8, u8),
            amplification: Amplification,
            oracle_key: PublicKeyHandle,
            last_oracle_price: Price,
            fee: FeeFraction,
            providers: LookupMap<AccountId, LiquidityPosition>,
            total_shares: TotalShares,
            payout_groups: PayoutGroups,
        }

        let storage_usage_before = near_sdk::env::storage_usage();
        let pools_prefix = StorageKey::Pools.into_storage_key();
        for pool_index in 0..self.pools.len() {
            let pool_key = [pools_prefix.as_slice(), &pool_index.to_le_bytes()].concat();
            let PoolBeforeMigration {
                assets,
                decimals,
                amplification,
                oracle_key,
                last_oracle_price,
                fee,
                providers,
                total_shares,
                payout_groups,
            } = near_sdk::borsh::from_slice(
                &near_sdk::env::storage_read(&pool_key).expect("Pool not found"),
            )
            .expect("Invalid pool");
            let pool = Pool {
                assets,
                decimals,
                amplification,
                oracle_key,
                last_oracle_price,
                fees: FeeConfiguration {
                    fraction: fee,
                    oracle_fee_per_swap: (TokenAmount::ZERO, TokenAmount::ZERO),
                    oracle_fee_receiver: PROTOCOL_FEE_RECEIVER_ID.to_owned(),
                },
                providers,
                total_shares,
                payout_groups,
            };
            near_sdk::env::storage_write(&pool_key, &near_sdk::borsh::to_vec(&pool).unwrap());
            AmpDexEvent::PoolUpdated {
                pool_id: PoolId::new(pool_index),
                pool: pool.view(),
            }
            .emit();
        }

        let storage_usage_after = near_sdk::env::storage_usage();
        let storage_cost = near_sdk::env::storage_byte_cost().saturating_mul(
            (storage_usage_after as u128).saturating_sub(storage_usage_before as u128),
        );
        expect!(
            let Some(leftover) = attached_near.checked_sub(storage_cost),
            "Not enough near attached for storage. Required: {storage_cost}, attached: {attached_near}"
        );
        DexCallResponse {
            asset_withdraw_requests: if !leftover.is_zero() {
                vec![AssetWithdrawRequest {
                    asset_id: AssetId::Near,
                    amount: TokenAmount::new(leftover.as_yoctonear()),
                    withdrawal_type: AssetWithdrawalType::WithdrawUnderlyingAsset(caller_id),
                }]
            } else {
                vec![]
            },
            add_storage_deposit: storage_cost,
            ..Default::default()
        }
    }
}
