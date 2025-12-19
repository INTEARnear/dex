use std::collections::HashMap;

use intear_dex_types::{
    AssetId, AssetWithdrawRequest, AssetWithdrawalType, DexCallRequest, DexCallResponse, DexId,
    SwapRequest, SwapRequestAmount, SwapResponse, expect,
};
use near_contract_standards::{
    fungible_token::core::ext_ft_core, non_fungible_token::core::ext_nft_core,
};
use near_sdk::{
    AccountId, Gas, NearToken, Promise, PromiseError,
    json_types::{Base58CryptoHash, Base64VecU8, U128},
    near,
};
use wasmi::{Engine, Func, Linker, Module, Store};

use crate::{
    CallType, DexEngine, DexEngineExt, IntearDexEvent, RunnerData, impl_supported_host_functions,
    impl_unsupported_host_functions, internal_asset_operations::AccountOrDexId,
};

#[derive(Clone, Debug)]
#[near(serializers=[json])]
pub enum Operation {
    /// Register storage for assets. No-op if assets are
    /// already registered for the given account or dex.
    RegisterAssets {
        asset_ids: Vec<AssetId>,
        r#for: Option<AccountOrDexId>,
    },
    /// Deploy new code to your dex.
    DeployDexCode {
        last_part_of_id: String,
        code_base64: Base64VecU8,
    },
    /// Withdraw assets from the dex engine contract's inner
    /// balance to the user. If amount is None, the entire
    /// balance of the asset will be withdrawn.
    Withdraw {
        asset_id: AssetId,
        amount: Option<U128>,
        to: Option<AccountId>,
    },
    /// Swap assets between two assets on the selected dex.
    SwapSimple {
        dex_id: DexId,
        message: Base64VecU8,
        asset_in: AssetId,
        asset_out: AssetId,
        /// If None, the ExactIn amount will be taken from
        /// the previous swap operation result.
        amount: Option<SwapRequestAmount>,
    },
    /// Call a method on a dex.
    DexCall {
        dex_id: DexId,
        method: String,
        args: Base64VecU8,
        attached_assets: HashMap<AssetId, U128>,
    },
    /// Transfer assets to a different account or dex.
    TransferAsset {
        to: AccountOrDexId,
        asset_id: AssetId,
        amount: U128,
    },
}

impl DexEngine {
    pub(crate) fn internal_deploy_dex_code(
        &mut self,
        last_part_of_id: String,
        code_base64: Base64VecU8,
        deployer: AccountId,
    ) {
        let code_hash = near_sdk::env::sha256_array(&code_base64.0);
        let dex_id = DexId {
            deployer,
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

    pub(crate) fn internal_swap_simple(
        &mut self,
        dex_id: DexId,
        message: Base64VecU8,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
        trader: AccountId,
    ) -> (U128, U128) {
        let swap_request = SwapRequest {
            message,
            asset_in,
            asset_out,
            amount,
        };

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
                call_type: CallType::Call {
                    dex_storage_mut: &mut self.dex_storage,
                    predecessor_id: trader.clone(),
                },
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

        self.internal_transfer_asset(
            AccountOrDexId::Account(trader.clone()),
            AccountOrDexId::Dex(dex_id.clone()),
            swap_request.asset_in.clone(),
            response.amount_in,
        );
        self.internal_transfer_asset(
            AccountOrDexId::Dex(dex_id.clone()),
            AccountOrDexId::Account(trader.clone()),
            swap_request.asset_out.clone(),
            response.amount_out,
        );

        IntearDexEvent::Swap {
            dex_id: dex_id.clone(),
            request: swap_request.clone(),
            amount_in: response.amount_in,
            amount_out: response.amount_out,
            trader: trader.clone(),
        }
        .emit();

        (response.amount_in, response.amount_out)
    }

    pub(crate) fn internal_dex_call(
        &mut self,
        dex_id: DexId,
        method: String,
        args: Base64VecU8,
        attached_assets: HashMap<AssetId, U128>,
        predecessor: AccountId,
    ) -> Base64VecU8 {
        expect!(
            method != "swap",
            "Method name 'swap' is reserved for the swap operation"
        );

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
                call_type: CallType::Call {
                    dex_storage_mut: &mut self.dex_storage,
                    predecessor_id: predecessor.clone(),
                },
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
            self.internal_transfer_asset(
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
                    self.internal_transfer_asset(
                        AccountOrDexId::Dex(dex_id.clone()),
                        AccountOrDexId::Account(account.clone()),
                        asset_id.clone(),
                        amount,
                    );
                }
                AssetWithdrawalType::ToInternalDexBalance(other_dex_id) => {
                    self.internal_transfer_asset(
                        AccountOrDexId::Dex(dex_id.clone()),
                        AccountOrDexId::Dex(other_dex_id.clone()),
                        asset_id.clone(),
                        amount,
                    );
                }
                AssetWithdrawalType::WithdrawUnderlyingAsset(to_account_id) => {
                    self.internal_withdraw(
                        asset_id.clone(),
                        Some(amount),
                        Some(to_account_id.clone()),
                        AccountOrDexId::Dex(dex_id.clone()),
                    )
                    .detach();
                }
            }
        }

        if !response.add_storage_deposit.is_zero() {
            self.internal_decrease_assets(
                AccountOrDexId::Dex(dex_id.clone()),
                AssetId::Near,
                U128(response.add_storage_deposit.as_yoctonear()),
            );
            self.dex_storage_balances
                .deposit(&dex_id, response.add_storage_deposit);
        }
        Base64VecU8::from(response.response)
    }

    pub(crate) fn internal_dex_view(
        &self,
        dex_id: DexId,
        method: String,
        args: Base64VecU8,
    ) -> Base64VecU8 {
        expect!(
            method != "swap",
            "Method name 'swap' is reserved for the swap operation"
        );

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
                request: args.0,
                response: None,
                registers: HashMap::new(),
                call_type: CallType::View {
                    dex_storage: &self.dex_storage,
                },
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

        Base64VecU8::from(response.unwrap_or_default())
    }

    pub(crate) fn internal_register_assets(
        &mut self,
        asset_ids: Vec<AssetId>,
        r#for: Option<AccountOrDexId>,
        predecessor: AccountId,
    ) {
        let r#for = r#for.unwrap_or_else(|| AccountOrDexId::Account(predecessor.clone()));
        let storage_usage_before = near_sdk::env::storage_usage();
        for asset_id in asset_ids {
            match r#for.clone() {
                AccountOrDexId::Account(account) => {
                    if self
                        .user_balances
                        .get(&(account.clone(), asset_id.clone()))
                        .is_none()
                    {
                        self.user_balances
                            .insert((account, asset_id.clone()), U128(0));
                    }
                }
                AccountOrDexId::Dex(dex_id) => {
                    if self
                        .dex_balances
                        .get(&(dex_id.clone(), asset_id.clone()))
                        .is_none()
                    {
                        self.dex_balances
                            .insert((dex_id, asset_id.clone()), U128(0));
                    }
                }
            }
            if self.total_in_custody.get(&asset_id).is_none() {
                self.total_in_custody.insert(asset_id, U128(0));
            }
        }
        self.user_balances.flush();
        self.dex_balances.flush();
        self.total_in_custody.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.user_storage_balances
            .charge(&predecessor, storage_usage_before, storage_usage_after);
    }

    pub(crate) fn internal_withdraw(
        &mut self,
        asset_id: AssetId,
        amount: Option<U128>,
        withdraw_to: Option<AccountId>,
        withdraw_from: AccountOrDexId,
    ) -> Promise {
        const GAS_FOR_FT_TRANSFER: Gas = Gas::from_tgas(10);
        const GAS_FOR_NFT_TRANSFER: Gas = Gas::from_tgas(10);
        const GAS_FOR_MT_TRANSFER: Gas = Gas::from_tgas(10);
        const GAS_FOR_WITHDRAWAL_CALLBACK: Gas = Gas::from_tgas(5);

        let amount = amount.unwrap_or_else(|| {
            self.asset_balance_of(withdraw_from.clone(), asset_id.clone())
                .unwrap_or_default()
        });
        if amount.0 == 0 {
            panic!("Withdrawal amount is zero");
        }
        self.internal_decrease_assets(withdraw_from.clone(), asset_id.clone(), amount);
        self.total_in_custody
            .entry(asset_id.clone())
            .and_modify(|b| {
                b.0 = b.0.checked_sub(amount.0).unwrap_or_else(|| {
                    panic!(
                        "Balance underflow for contract and asset Near: {} - {} < {}",
                        b.0,
                        amount.0,
                        u128::MIN,
                    )
                })
            })
            .or_insert_with(|| {
                panic!(
                    "Failed to withdraw assets from contract tracked balance: asset not registered"
                )
            });

        let withdraw_to = withdraw_to.unwrap_or_else(|| match withdraw_from.clone() {
            AccountOrDexId::Account(account) => account,
            AccountOrDexId::Dex(_) => {
                panic!("withdraw_to must be present when withdrawing from a dex")
            }
        });
        match &asset_id {
            AssetId::Near => Promise::new(withdraw_to.clone())
                .transfer(NearToken::from_yoctonear(amount.0))
                .then(
                    Self::ext(near_sdk::env::current_account_id())
                        .with_static_gas(GAS_FOR_WITHDRAWAL_CALLBACK)
                        .after_withdraw(asset_id, amount, withdraw_to, withdraw_from),
                ),
            AssetId::Nep141(contract_id) => ext_ft_core::ext(contract_id.clone())
                .with_attached_deposit(NearToken::from_yoctonear(1))
                .with_static_gas(GAS_FOR_FT_TRANSFER)
                .ft_transfer(withdraw_to.clone(), amount, None)
                .then(
                    Self::ext(near_sdk::env::current_account_id())
                        .with_static_gas(GAS_FOR_WITHDRAWAL_CALLBACK)
                        .after_withdraw(asset_id, amount, withdraw_to, withdraw_from),
                ),
            AssetId::Nep171(contract_id, token_id) => ext_nft_core::ext(contract_id.clone())
                .with_attached_deposit(NearToken::from_yoctonear(1))
                .with_static_gas(GAS_FOR_NFT_TRANSFER)
                .nft_transfer(withdraw_to.clone(), token_id.clone(), None, None)
                .then(
                    Self::ext(near_sdk::env::current_account_id())
                        .with_static_gas(GAS_FOR_WITHDRAWAL_CALLBACK)
                        .after_withdraw(asset_id, amount, withdraw_to, withdraw_from),
                ),
            AssetId::Nep245(contract_id, token_id) => Promise::new(contract_id.clone())
                .function_call(
                    "mt_transfer",
                    near_sdk::serde_json::json!({
                        "receiver_id": withdraw_to.clone(),
                        "token_id": token_id,
                        "amount": amount,
                        "approval": null,
                        "memo": null,
                    })
                    .to_string()
                    .into_bytes(),
                    NearToken::from_yoctonear(1),
                    GAS_FOR_MT_TRANSFER,
                )
                .then(
                    Self::ext(near_sdk::env::current_account_id())
                        .with_static_gas(GAS_FOR_WITHDRAWAL_CALLBACK)
                        .after_withdraw(asset_id, amount, withdraw_to, withdraw_from),
                ),
        }
    }

    pub(crate) fn internal_execute_operations(
        &mut self,
        operations: Vec<Operation>,
        by: AccountId,
    ) {
        let mut last_output = None;
        for operation in operations {
            match operation {
                Operation::RegisterAssets { asset_ids, r#for } => {
                    self.internal_register_assets(asset_ids, r#for, by.clone());
                }
                Operation::DeployDexCode {
                    last_part_of_id,
                    code_base64,
                } => {
                    self.internal_deploy_dex_code(last_part_of_id, code_base64, by.clone());
                }
                Operation::Withdraw {
                    asset_id,
                    amount,
                    to,
                } => {
                    self.internal_withdraw(
                        asset_id,
                        amount,
                        to,
                        AccountOrDexId::Account(by.clone()),
                    )
                    .detach();
                }
                Operation::SwapSimple {
                    dex_id,
                    message,
                    asset_in,
                    asset_out,
                    amount,
                } => {
                    let amount = match amount {
                        Some(amount) => amount,
                        None => match last_output {
                            Some((last_asset_out, amount)) => {
                                if last_asset_out == asset_in {
                                    SwapRequestAmount::ExactIn(amount)
                                } else {
                                    panic!(
                                        "Amount can only be omitted if the last swap asset out matches the current asset in"
                                    );
                                }
                            }
                            None => panic!("Amount is required for first SwapSimple operation"),
                        },
                    };
                    let (_amount_in, amount_out) = self.internal_swap_simple(
                        dex_id,
                        message,
                        asset_in,
                        asset_out.clone(),
                        amount,
                        by.clone(),
                    );
                    last_output = Some((asset_out, amount_out));
                }
                Operation::DexCall {
                    dex_id,
                    method,
                    args,
                    attached_assets,
                } => {
                    self.internal_dex_call(dex_id, method, args, attached_assets, by.clone());
                }
                Operation::TransferAsset {
                    to,
                    asset_id,
                    amount,
                } => {
                    self.internal_transfer_asset(
                        AccountOrDexId::Account(by.clone()),
                        to,
                        asset_id,
                        amount,
                    );
                }
            }
        }
    }
}

#[near]
impl DexEngine {
    // TODO: Test failure case
    #[private]
    pub fn after_withdraw(
        &mut self,
        asset_id: AssetId,
        amount: U128,
        withdraw_to: AccountId,
        withdraw_from: AccountOrDexId,
        #[callback_result] result: Result<(), PromiseError>,
    ) -> bool {
        near_sdk::env::log_str(&format!("After withdraw: {result:?}"));
        match result {
            Ok(()) => {
                IntearDexEvent::Withdraw {
                    from: withdraw_from,
                    to: withdraw_to,
                    asset_id,
                    amount,
                }
                .emit();
                true
            }
            Err(error) => {
                near_sdk::env::log_str(&format!(
                    "Refunding to {withdraw_from:?} because withdrawal to {withdraw_to} failed: {error:?}"
                ));
                self.internal_increase_assets(withdraw_from, asset_id.clone(), amount);
                self.total_in_custody
                    .entry(asset_id.clone())
                    .and_modify(|b| {
                        b.0 = b.0.checked_add(amount.0).unwrap_or_else(|| {
                            panic!(
                                "Balance overflow for contract and asset {asset_id}: {} + {} > {}",
                                b.0,
                                amount.0,
                                u128::MAX
                            )
                        });
                    })
                    .or_insert_with(|| {
                        panic!("Failed to refund assets to contract tracked balance: asset not registered")
                    });
                false
            }
        }
    }
}
