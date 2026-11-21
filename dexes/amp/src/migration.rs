use std::collections::HashMap;

use amp_dex_types::{Pool, TokenAmount};
use intear_dex_common::{AssetId, Caller, FeeBalances, env, expect};
use near_sdk::{PublicKey, near, store::Vector};

use crate::{AmpDex, AmpDexExt};

#[near]
impl AmpDex {
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
        expect!(
            caller_id == env::dex_id().deployer,
            "Only the owner of the dex can migrate"
        );
        expect!(args.is_empty(), "Migrate takes no args");
        expect!(attached_assets.is_empty(), "No assets should be attached");

        #[near(serializers=[borsh])]
        struct AmpDexBeforeMigration {
            pools: Vector<Pool>,
            fees_collected_by_users: FeeBalances,
            #[allow(dead_code)]
            fee_discount_signer_public_key: Option<PublicKey>,
        }

        let AmpDexBeforeMigration {
            pools,
            fees_collected_by_users,
            fee_discount_signer_public_key: _,
        } = near_sdk::env::state_read().expect("Failed to read old state");
        Self {
            pools,
            fees_collected_by_users,
        }
    }
}
