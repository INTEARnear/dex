use std::collections::HashMap;

use amp_dex_types::{
    AmpDexEvent, Amplification, AssetWithBalance, FeeFraction, LiquidityPosition, PayoutGroups,
    Pool, PoolId, PoolRateSource, Price, TokenAmount, TotalShares,
};
use intear_dex_common::{AssetId, Caller, DexCallResponse, env, expect};
use near_sdk::{AccountId, IntoStorageKey, PublicKeyHandle, near, store::LookupMap};

use crate::{AmpDex, AmpDexExt, StorageKey};

#[near]
impl AmpDex {
    /// Makes the oracle of every pool its rate source, and takes the fixed oracle fee and its
    /// receiver out of it, since the engine takes the fees of oracles now. Fees that oracles
    /// already collected stay theirs to withdraw.
    #[payable]
    #[result_serializer(borsh)]
    pub fn migrate(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, TokenAmount>,
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
        expect!(attached_assets.is_empty(), "No assets should be attached");

        #[near(serializers=[borsh])]
        #[allow(dead_code)]
        struct FeesBeforeMigration {
            fraction: FeeFraction,
            oracle_fee_per_swap: (TokenAmount, TokenAmount),
            oracle_fee_receiver: AccountId,
        }

        #[near(serializers=[borsh])]
        struct PoolBeforeMigration {
            assets: (AssetWithBalance, AssetWithBalance),
            decimals: (u8, u8),
            amplification: Amplification,
            oracle_key: PublicKeyHandle,
            last_oracle_price: Price,
            fees: FeesBeforeMigration,
            providers: LookupMap<AccountId, LiquidityPosition>,
            total_shares: TotalShares,
            payout_groups: PayoutGroups,
        }

        let pools_prefix = StorageKey::Pools.into_storage_key();
        for pool_index in 0..self.pools.len() {
            let pool_key = [pools_prefix.as_slice(), &pool_index.to_le_bytes()].concat();
            let PoolBeforeMigration {
                assets,
                decimals,
                amplification,
                oracle_key,
                last_oracle_price,
                fees,
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
                rate_source: PoolRateSource::Oracle {
                    key: oracle_key,
                    last_price: last_oracle_price,
                },
                fee: fees.fraction,
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

        DexCallResponse::default()
    }
}
