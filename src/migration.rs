use intear_dex_types::{AssetId, DexId};
use near_sdk::{
    AccountId,
    json_types::U128,
    near,
    store::{IterableMap, LazyOption, LookupMap},
};

use crate::{
    AssetBalances, DexEngine, DexEngineExt, DexStorage, StorageKey,
    storage_management::StorageBalances,
};

#[near]
impl DexEngine {
    /// Adds `oracle_requests` and `oracle_responses`
    #[private]
    #[init(ignore_state)]
    pub fn migrate() -> Self {
        #[near(serializers=[borsh])]
        struct OldState {
            dex_balances: LookupMap<DexId, AssetBalances>,
            dex_storage: DexStorage,
            dex_codes: LookupMap<DexId, Vec<u8>>,
            dex_storage_balances: StorageBalances<DexId>,
            user_balances: LookupMap<AccountId, AssetBalances>,
            user_storage_balances: StorageBalances<AccountId>,
            total_in_custody: IterableMap<AssetId, U128>,
            paused: bool,
            trusted_code_deployer: AccountId,
            uploaded_code: LazyOption<Vec<u8>>,
        }
        let old_state: OldState = near_sdk::env::state_read().expect("Failed to read old state");
        Self {
            dex_balances: old_state.dex_balances,
            dex_storage: old_state.dex_storage,
            dex_codes: old_state.dex_codes,
            dex_storage_balances: old_state.dex_storage_balances,
            user_balances: old_state.user_balances,
            user_storage_balances: old_state.user_storage_balances,
            total_in_custody: old_state.total_in_custody,
            paused: old_state.paused,
            trusted_code_deployer: old_state.trusted_code_deployer,
            uploaded_code: old_state.uploaded_code,
            oracle_requests: LookupMap::new(StorageKey::OracleRequests),
            oracle_responses: LookupMap::new(StorageKey::OracleResponses),
        }
    }
}
