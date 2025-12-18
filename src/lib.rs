pub mod asset_deposit;
pub mod host_functions;
pub mod internal_asset_operations;
pub mod storage_management;

use std::collections::HashMap;

use crate::{internal_asset_operations::AccountOrDexId, storage_management::StorageBalances};
use intear_dex_types::{
    AssetId, AssetWithdrawRequest, AssetWithdrawalType, DexCallRequest, DexCallResponse, DexId,
    SwapRequest, SwapRequestAmount, SwapResponse, expect,
};
use near_sdk::{
    AccountId, BorshStorageKey,
    json_types::{Base58CryptoHash, Base64VecU8, U128},
    near,
    store::{IterableMap, LookupMap},
};
use wasmi::{Caller, Engine, Func, Linker, Module, Store};

#[near(contract_state)]
pub struct DexEngine {
    /// Assets that are custodied by the dex engine contract
    /// for the dexes that run inside it. Other dexes or users
    /// can't access other dexes' balances.
    dex_balances: LookupMap<(DexId, AssetId), U128>,
    /// Persistent storage for each dex, similar to contract
    /// storage of traditional smart contract dexes. It's
    /// public, but currently there's no way to access other
    /// dexes' storage from dex runtime.
    dex_storage: LookupMap<(DexId, Vec<u8>), Vec<u8>>,
    /// Wasm code for each dex.
    dex_codes: LookupMap<DexId, Vec<u8>>,
    /// Storage balances for each dex, translated to storage
    /// of this smart contract. use dex_* methods to interact
    /// with it, such as dex_storage_deposit.
    dex_storage_balances: StorageBalances<DexId>,
    /// Balances for each user, custodied by the dex engine
    /// contract for faster access. This reduces the need for
    /// ft_transfer_call, which takes time.
    user_balances: LookupMap<(AccountId, AssetId), U128>,
    /// Storage balances for each user, translated to storage
    /// of this smart contract. use storage management methods
    /// to interact with it, such as storage_deposit.
    user_storage_balances: StorageBalances<AccountId>,
    /// Balances for all the funds custodied by the dex engine
    /// contract. This means if the dex engine contract has
    /// less than this amount, it's either bug of the asset
    /// implementation, or funds have been drained from the
    /// dex engine contract. And if the balance is greater
    /// than this stored amount, it can be freely taken out
    /// without causing any issues.
    contract_tracked_balance: IterableMap<AssetId, U128>,
}

#[derive(BorshStorageKey)]
#[near(serializers=[borsh])]
enum StorageKey {
    DexBalances,
    DexStorage,
    DexCodes,
    DexStorageBalances,
    UserBalances,
    UserStorageBalances,
    ContractTrackedBalance,
}

impl Default for DexEngine {
    fn default() -> Self {
        Self {
            dex_balances: LookupMap::new(StorageKey::DexBalances),
            dex_storage: LookupMap::new(StorageKey::DexStorage),
            dex_codes: LookupMap::new(StorageKey::DexCodes),
            dex_storage_balances: StorageBalances::new(StorageKey::DexStorageBalances),
            user_balances: LookupMap::new(StorageKey::UserBalances),
            user_storage_balances: StorageBalances::new(StorageKey::UserStorageBalances),
            contract_tracked_balance: IterableMap::new(StorageKey::ContractTrackedBalance),
        }
    }
}

#[near(event_json(standard = "inteardex"))]
pub enum IntearDexEvent {
    #[event_version("1.0.0")]
    DexDeployed {
        dex_id: DexId,
        code_hash: Base58CryptoHash,
    },
    #[event_version("1.0.0")]
    DexEvent {
        dex_id: DexId,
        event: near_sdk::serde_json::Value,
    },
    #[event_version("1.0.0")]
    UserDeposit {
        account_id: AccountId,
        asset_id: AssetId,
        amount: U128,
    },
    #[event_version("1.0.0")]
    UserWithdraw {
        account_id: AccountId,
        asset_id: AssetId,
        amount: U128,
    },
    #[event_version("1.0.0")]
    UserBalanceUpdate {
        account_id: AccountId,
        asset_id: AssetId,
        balance: U128,
    },
    #[event_version("1.0.0")]
    DexBalanceUpdate {
        dex_id: DexId,
        asset_id: AssetId,
        balance: U128,
    },
    #[event_version("1.0.0")]
    SwapStep {
        dex_id: DexId,
        // request: SwapRequest,
        amount_in: U128,
        amount_out: U128,
        trader: AccountId,
    },
}

pub struct RunnerData<'a> {
    request: Vec<u8>,
    response: Option<Vec<u8>>,
    registers: HashMap<u64, Vec<u8>>,
    dex_storage: &'a mut LookupMap<(DexId, Vec<u8>), Vec<u8>>,
    predecessor_id: AccountId,
    dex_id: DexId,
    dex_storage_balances: &'a StorageBalances<DexId>,
    dex_storage_usage_before_transaction: u64,
}

#[near]
impl DexEngine {
    /// Deploy or upgrade the code for a dex.
    #[payable]
    pub fn deploy_code(&mut self, last_part_of_id: String, code_base64: Base64VecU8) {
        near_sdk::assert_one_yocto();

        let code_hash = near_sdk::env::sha256_array(&code_base64.0);
        let dex_id = DexId {
            deployer: near_sdk::env::predecessor_account_id(),
            id: last_part_of_id,
        };
        let storage_usage_before = near_sdk::env::storage_usage();
        self.dex_codes.insert(dex_id.clone(), code_base64.0);
        self.dex_codes.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.dex_storage_balances
            .charge(&dex_id, storage_usage_before, storage_usage_after);

        IntearDexEvent::DexDeployed {
            dex_id: dex_id.clone(),
            code_hash: Base58CryptoHash::from(code_hash),
        }
        .emit();
    }

    /// Swap one asset for another on a specific dex.
    /// Multi-step aggregator method coming soon.
    #[payable]
    pub fn swap_one_dex(
        &mut self,
        dex_id: DexId,
        message: Base64VecU8,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
    ) -> (U128, U128) {
        near_sdk::assert_one_yocto();

        let swap_request = SwapRequest {
            message: message.0,
            asset_in,
            asset_out,
            amount,
        };
        let trader = near_sdk::env::predecessor_account_id();

        let code = self.dex_codes.get(&dex_id).expect("Dex code not found");
        let engine = Engine::default();
        let module = match Module::new(&engine, code) {
            Ok(module) => module,
            Err(err) => panic!("Failed to load module: {err:?}"),
        };

        let storage_usage_before = near_sdk::env::storage_usage();
        let mut store = Store::new(
            &engine,
            RunnerData {
                request: near_sdk::borsh::to_vec(&swap_request)
                    .expect("Failed to serialize swap request"),
                response: None,
                registers: HashMap::new(),
                dex_storage: &mut self.dex_storage,
                predecessor_id: trader.clone(),
                dex_id: dex_id.clone(),
                dex_storage_balances: &self.dex_storage_balances,
                dex_storage_usage_before_transaction: storage_usage_before,
            },
        );
        let mut linker = Linker::new(&engine);

        impl_supported_host_functions!(linker);
        impl_unsupported_host_functions!(linker);

        let instance = match linker.instantiate_and_start(&mut store, &module) {
            Ok(i) => i,
            Err(err) => panic!("Failed to instantiate module: {err:?}"),
        };
        let swap_func: Func = match instance.get_func(&mut store, "swap") {
            Some(f) => f,
            None => panic!("Failed to get function"),
        };
        match swap_func.call(&mut store, &[], &mut []) {
            Ok(()) => (),
            Err(err) => panic!("Failed to call function: {err:?}"),
        };
        let response = store.data_mut().response.take();
        drop(store);
        drop(linker);

        self.dex_storage.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.dex_storage_balances
            .charge(&dex_id, storage_usage_before, storage_usage_after);

        let response: SwapResponse = match response {
            Some(response) => {
                near_sdk::borsh::from_slice(&response).expect("Failed to deserialize swap response")
            }
            None => panic!("No response from swap"),
        };
        match swap_request.amount {
            SwapRequestAmount::ExactIn(exact_in) => {
                expect!(exact_in == response.amount_in, "Amount in does not match");
            }
            SwapRequestAmount::ExactOut(exact_out) => {
                expect!(
                    exact_out == response.amount_out,
                    "Amount out does not match"
                );
            }
        }

        self.transfer_assets(
            AccountOrDexId::Account(trader.clone()),
            AccountOrDexId::Dex(dex_id.clone()),
            swap_request.asset_in.clone(),
            response.amount_in,
        );
        self.transfer_assets(
            AccountOrDexId::Dex(dex_id.clone()),
            AccountOrDexId::Account(trader.clone()),
            swap_request.asset_out.clone(),
            response.amount_out,
        );

        IntearDexEvent::SwapStep {
            dex_id: dex_id.clone(),
            // request: swap_request.clone(),
            amount_in: response.amount_in,
            amount_out: response.amount_out,
            trader: trader.clone(),
        }
        .emit();

        (response.amount_in, response.amount_out)
    }

    /// An arbitrary call to a dex method. Can be used for
    /// operations such as adding liquidity, removing liquidity,
    /// oracle updates, manual curve / strategy updates by the
    /// developer, etc.
    #[payable]
    pub fn dex_call(
        &mut self,
        dex_id: DexId,
        method: String,
        args: Base64VecU8,
        attached_assets: HashMap<AssetId, U128>,
    ) -> Base64VecU8 {
        near_sdk::assert_one_yocto();
        expect!(
            method != "swap",
            "Method name 'swap' is reserved for the swap operation"
        );

        let predecessor = near_sdk::env::predecessor_account_id();
        for (asset_id, amount) in attached_assets.clone() {
            self.assert_has_enough(
                AccountOrDexId::Account(predecessor.clone()),
                asset_id.clone(),
                amount,
            );
        }

        let code = self.dex_codes.get(&dex_id).expect("Dex code not found");
        let engine = Engine::default();
        let module = match Module::new(&engine, code) {
            Ok(module) => module,
            Err(err) => panic!("Failed to load module: {err:?}"),
        };

        let storage_usage_before = near_sdk::env::storage_usage();
        let request = DexCallRequest {
            args: args.0,
            attached_assets,
        };
        let mut store = Store::new(
            &engine,
            RunnerData {
                request: near_sdk::borsh::to_vec(&request).expect("Failed to serialize request"),
                response: None,
                registers: HashMap::new(),
                dex_storage: &mut self.dex_storage,
                predecessor_id: predecessor.clone(),
                dex_id: dex_id.clone(),
                dex_storage_balances: &self.dex_storage_balances,
                dex_storage_usage_before_transaction: storage_usage_before,
            },
        );
        let mut linker = Linker::new(&engine);

        impl_supported_host_functions!(linker);
        impl_unsupported_host_functions!(linker);

        let instance = match linker.instantiate_and_start(&mut store, &module) {
            Ok(i) => i,
            Err(err) => panic!("Failed to instantiate module: {err:?}"),
        };
        let dex_call_func: Func = match instance.get_func(&mut store, method.as_str()) {
            Some(f) => f,
            None => panic!("Failed to get function"),
        };
        match dex_call_func.call(&mut store, &[], &mut []) {
            Ok(()) => (),
            Err(err) => panic!("Failed to call function: {err:?}"),
        };
        let response = store.data_mut().response.take();
        drop(store);
        drop(linker);

        self.dex_storage.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.dex_storage_balances
            .charge(&dex_id, storage_usage_before, storage_usage_after);

        let response: DexCallResponse = match response {
            Some(response) => near_sdk::borsh::from_slice(&response)
                .expect("Failed to deserialize dex call response"),
            None => Default::default(),
        };
        for (asset_id, amount) in request.attached_assets {
            self.transfer_assets(
                AccountOrDexId::Account(predecessor.clone()),
                AccountOrDexId::Dex(dex_id.clone()),
                asset_id.clone(),
                amount,
            );
        }

        for AssetWithdrawRequest {
            asset_id,
            amount,
            withdrawal_type,
        } in response.asset_withdraw_requests
        {
            match withdrawal_type {
                AssetWithdrawalType::ToInternalUserBalance(account) => {
                    self.transfer_assets(
                        AccountOrDexId::Dex(dex_id.clone()),
                        AccountOrDexId::Account(account.clone()),
                        asset_id.clone(),
                        amount,
                    );
                }
                AssetWithdrawalType::ToInternalDexBalance(other_dex_id) => {
                    self.transfer_assets(
                        AccountOrDexId::Dex(dex_id.clone()),
                        AccountOrDexId::Dex(other_dex_id.clone()),
                        asset_id.clone(),
                        amount,
                    );
                }
                AssetWithdrawalType::WithdrawUnderlyingAsset(_) => {
                    unimplemented!()
                }
                AssetWithdrawalType::WithdrawUnderlyingAssetAndCall {
                    recipient_id: _,
                    message: _,
                } => {
                    unimplemented!()
                }
            }
        }

        if !response.add_storage_deposit.is_zero() {
            self.withdraw_assets(
                AccountOrDexId::Dex(dex_id.clone()),
                AssetId::Near,
                U128(response.add_storage_deposit.as_yoctonear()),
            );
            self.dex_storage_balances
                .deposit(&dex_id, response.add_storage_deposit);
        }
        Base64VecU8::from(response.response)
    }

    #[payable]
    pub fn transfer_personal_assets(
        &mut self,
        to: AccountOrDexId,
        asset_id: AssetId,
        amount: U128,
    ) {
        near_sdk::assert_one_yocto();
        self.transfer_assets(
            AccountOrDexId::Account(near_sdk::env::predecessor_account_id()),
            to,
            asset_id,
            amount,
        );
    }

    pub fn asset_balance_of(&self, of: AccountOrDexId, asset_id: AssetId) -> Option<U128> {
        match of {
            AccountOrDexId::Account(account) => {
                self.user_balances.get(&(account, asset_id)).copied()
            }
            AccountOrDexId::Dex(dex_id) => self.dex_balances.get(&(dex_id, asset_id)).copied(),
        }
    }

    #[payable]
    pub fn register_assets(&mut self, asset_ids: Vec<AssetId>, r#for: Option<AccountOrDexId>) {
        near_sdk::assert_one_yocto();
        let r#for = r#for
            .unwrap_or_else(|| AccountOrDexId::Account(near_sdk::env::predecessor_account_id()));
        let storage_usage_before = near_sdk::env::storage_usage();
        for asset_id in asset_ids {
            match r#for.clone() {
                AccountOrDexId::Account(account) => {
                    self.user_balances.insert((account, asset_id), U128(0));
                }
                AccountOrDexId::Dex(dex_id) => {
                    self.dex_balances.insert((dex_id, asset_id), U128(0));
                }
            }
        }
        self.user_balances.flush();
        self.dex_balances.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.user_storage_balances.charge(
            &near_sdk::env::predecessor_account_id(),
            storage_usage_before,
            storage_usage_after,
        );
    }
}
