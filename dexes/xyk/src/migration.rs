use std::collections::HashMap;

use intear_dex_common::{AssetId, Caller, FeeBalances, env, expect};
use near_sdk::{PublicKey, near, store::Vector};
use xyk_dex_types::{CAN_MIGRATE, Pool, TokenAmount};

use crate::{XykDex, XykDexExt};

#[near]
impl XykDex {
    /// Takes out the fee discount signer, since the engine checks fee discounts now
    #[init(ignore_state)]
    #[payable]
    pub fn migrate(
        #[serializer(borsh)] attached_assets: HashMap<AssetId, TokenAmount>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> Self {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        expect!(caller_id == CAN_MIGRATE, "Only owner can migrate");
        expect!(args.is_empty(), "Migrate takes no args");
        expect!(attached_assets.is_empty(), "No assets should be attached");

        #[near(serializers=[borsh])]
        struct XykDexBeforeMigration {
            pools: Vector<Pool>,
            fees_collected_by_users: FeeBalances,
            fees_claimable_by_anyone: FeeBalances,
            #[allow(dead_code)]
            fee_discount_signer_public_key: Option<PublicKey>,
        }

        let XykDexBeforeMigration {
            pools,
            fees_collected_by_users,
            fees_claimable_by_anyone,
            fee_discount_signer_public_key: _,
        } = near_sdk::env::state_read().expect("Failed to read old state");
        Self {
            pools,
            fees_collected_by_users,
            fees_claimable_by_anyone,
        }
    }
}
