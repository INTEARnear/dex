use intear_dex_types::{AssetId, DexId, expect};
use near_sdk::{
    AccountId, NearToken,
    json_types::U128,
    near,
    store::{IterableMap, LookupMap},
};

use crate::{
    DexEngine, DexEngineExt, DexStorage, StorageKey,
    storage_management::{StorageBalances, StorageUsed},
};

/// Balances in the layout from before `migrate`, keyed by owner and
/// asset together, so that the assets of an owner couldn't be listed
#[near(serializers=[borsh])]
pub struct FlatBalances {
    users: LookupMap<(AccountId, AssetId), U128>,
    dexes: LookupMap<(DexId, AssetId), U128>,
    /// How many are left to move, counted in the state after the engine
    /// was paused, when no balance can be registered anymore
    users_left: u32,
    dexes_left: u32,
}

#[near(serializers=[json])]
pub struct MigrationProgress {
    pub user_balances_left: u32,
    pub dex_balances_left: u32,
}

#[near]
impl DexEngine {
    /// Converts the state from before balances were kept by owner, which
    /// lets the assets of an account or a dex be listed. The engine must
    /// be paused, and stays paused until `migrate_balances` moved every
    /// balance and `finish_migration` ended the migration.
    #[private]
    #[init(ignore_state)]
    pub fn migrate(
        trusted_code_deployer: AccountId,
        user_balance_count: u32,
        dex_balance_count: u32,
    ) -> Self {
        // Before 3adeba3, NEAR converted to dex storage deposits through
        // `add_storage_deposit` was removed from the dex balance, but not from
        // `total_in_custody`, so NEAR in custody was overcounted by this amount.
        const NEAR_CUSTODY_OVERCOUNT: u128 = 2_455_360_000_000_000_000_000_001;

        #[near(serializers=[borsh])]
        struct OldState {
            dex_balances: LookupMap<(DexId, AssetId), U128>,
            dex_storage: DexStorage,
            dex_codes: LookupMap<DexId, Vec<u8>>,
            dex_storage_balances: LookupMap<DexId, StorageUsed>,
            user_balances: LookupMap<(AccountId, AssetId), U128>,
            user_storage_balances: LookupMap<AccountId, StorageUsed>,
            total_in_custody: IterableMap<AssetId, U128>,
            paused: bool,
            code_deployment_allowed: bool,
        }
        let mut old_state: OldState =
            near_sdk::env::state_read().expect("Failed to read old state");
        expect!(
            old_state.paused,
            "The engine must be paused before the migration, so that no balance is registered while balances are counted and moved"
        );
        let near_in_custody = old_state
            .total_in_custody
            .get_mut(&AssetId::Near)
            .expect("NEAR is not in total_in_custody");
        near_in_custody.0 = near_in_custody
            .0
            .checked_sub(NEAR_CUSTODY_OVERCOUNT)
            .expect("NEAR in custody is less than the overcount");
        old_state.total_in_custody.flush();
        Self {
            dex_balances: LookupMap::new(StorageKey::DexBalances),
            dex_storage: old_state.dex_storage,
            dex_codes: old_state.dex_codes,
            // The sums are set by `finish_migration`. The old code takes
            // storage deposits while paused, so they're only final now.
            dex_storage_balances: StorageBalances::from_parts(
                old_state.dex_storage_balances,
                StorageUsed::default(),
            ),
            user_balances: LookupMap::new(StorageKey::UserBalances),
            user_storage_balances: StorageBalances::from_parts(
                old_state.user_storage_balances,
                StorageUsed::default(),
            ),
            total_in_custody: old_state.total_in_custody,
            paused: true,
            balances_to_migrate: Some(FlatBalances {
                users: old_state.user_balances,
                dexes: old_state.dex_balances,
                users_left: user_balance_count,
                dexes_left: dex_balance_count,
            }),
            trusted_code_deployer,
        }
    }

    /// Moves balances from the layout before `migrate` into the balances
    /// of their owners. Each listed balance must still be in the old
    /// layout, so that none is moved twice. What they take in the new
    /// layout beyond the old one is paid from untracked NEAR.
    #[private]
    pub fn migrate_balances(
        &mut self,
        user_balances: Vec<(AccountId, AssetId)>,
        dex_balances: Vec<(DexId, AssetId)>,
    ) {
        let balances_to_migrate = self
            .balances_to_migrate
            .as_mut()
            .unwrap_or_else(|| panic!("Balances are already migrated"));
        for (account_id, asset_id) in user_balances {
            let balance = balances_to_migrate
                .users
                .remove(&(account_id.clone(), asset_id.clone()))
                .unwrap_or_else(|| {
                    panic!("{account_id} has no {asset_id} balance left to migrate")
                });
            let previous_balance = self
                .user_balances
                .entry(account_id.clone())
                .or_insert_with(|| {
                    IterableMap::new(StorageKey::UserBalancesOf {
                        account_id: account_id.clone(),
                    })
                })
                .insert(asset_id.clone(), balance);
            expect!(
                previous_balance.is_none(),
                "{account_id} already has a {asset_id} balance"
            );
            balances_to_migrate.users_left = balances_to_migrate
                .users_left
                .checked_sub(1)
                .expect("More user balances migrated than counted");
        }
        for (dex_id, asset_id) in dex_balances {
            let balance = balances_to_migrate
                .dexes
                .remove(&(dex_id.clone(), asset_id.clone()))
                .unwrap_or_else(|| panic!("{dex_id} has no {asset_id} balance left to migrate"));
            let previous_balance = self
                .dex_balances
                .entry(dex_id.clone())
                .or_insert_with(|| {
                    IterableMap::new(StorageKey::DexBalancesOf {
                        dex_id: dex_id.clone(),
                    })
                })
                .insert(asset_id.clone(), balance);
            expect!(
                previous_balance.is_none(),
                "{dex_id} already has a {asset_id} balance"
            );
            balances_to_migrate.dexes_left = balances_to_migrate
                .dexes_left
                .checked_sub(1)
                .expect("More dex balances migrated than counted");
        }
    }

    /// Ends the migration once every balance is moved, with the sums of
    /// storage balances, which the engine keeps from now on. They're read
    /// from the state after `migrate`, since this code doesn't take
    /// storage deposits or withdrawals while the engine is paused.
    #[private]
    pub fn finish_migration(
        &mut self,
        dex_storage_balances_total: NearToken,
        dex_storage_balances_used: NearToken,
        user_storage_balances_total: NearToken,
        user_storage_balances_used: NearToken,
    ) {
        let balances_to_migrate = self
            .balances_to_migrate
            .take()
            .unwrap_or_else(|| panic!("Balances are already migrated"));
        expect!(
            balances_to_migrate.users_left == 0 && balances_to_migrate.dexes_left == 0,
            "{} user balances and {} dex balances are left to migrate",
            balances_to_migrate.users_left,
            balances_to_migrate.dexes_left
        );
        expect!(
            dex_storage_balances_used <= dex_storage_balances_total
                && user_storage_balances_used <= user_storage_balances_total,
            "Storage balances can't use more than they hold"
        );
        self.dex_storage_balances.set_sum(StorageUsed {
            total: dex_storage_balances_total,
            used: dex_storage_balances_used,
        });
        self.user_storage_balances.set_sum(StorageUsed {
            total: user_storage_balances_total,
            used: user_storage_balances_used,
        });
        // Fails if the sums say that the engine owes more NEAR than it has
        self.untracked_near();
    }

    /// How many balances are left to move, while the engine is migrating
    pub fn migration_progress(&self) -> Option<MigrationProgress> {
        self.balances_to_migrate
            .as_ref()
            .map(|balances_to_migrate| MigrationProgress {
                user_balances_left: balances_to_migrate.users_left,
                dex_balances_left: balances_to_migrate.dexes_left,
            })
    }
}
