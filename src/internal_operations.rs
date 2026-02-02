use std::collections::HashMap;

use intear_dex_types::{
    AssetId, AssetWithdrawRequest, AssetWithdrawalType, DexCallRequest, DexCallResponse, DexId,
    SwapRequest, SwapRequestAmount, SwapResponse, expect,
};
use near_contract_standards::{
    fungible_token::core::ext_ft_core, non_fungible_token::core::ext_nft_core,
};
use near_sdk::{
    AccountId, Gas, NearToken, Promise, PromiseError, PromiseOrValue,
    json_types::{Base58CryptoHash, Base64VecU8, U128},
    near,
};
use wasmi::{Engine, Func, Linker, Module, Store};

use crate::{
    CallType, DexEngine, DexEngineExt, IntearDexEvent, RunnerData,
    internal_asset_operations::AccountOrDexId,
};

#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[json])]
pub enum SwapOperationAmount {
    Amount(SwapRequestAmount),
    OutputOfLastIn,
    EntireBalanceIn,
}

pub enum TradeAccount<'a> {
    User(AccountId),
    Sandboxed {
        assets: &'a mut HashMap<AssetId, U128>,
        alleged_trader: AccountId,
    },
}

#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
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
        amount: WithdrawAmount,
        to: Option<AccountId>,
        /// If the withdrawal fails and current user doesn't have
        /// a registerd balance in this asset, the assets will be
        /// refunded to this address. It's required that either
        /// the user address or rescue address is registered.
        rescue_address: Option<AccountId>,
    },
    /// Swap assets between two assets on the selected dex.
    SwapSimple {
        dex_id: DexId,
        message: Base64VecU8,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapOperationAmount,
        /// Either minimum amount out (for ExactIn) or maximum amount in (for ExactOut)
        constraint: Option<U128>,
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
    /// Convert some of AssetId::Near to storage for an account
    /// or a dex.
    StorageDeposit {
        amount: U128,
        r#for: Option<AccountOrDexId>,
    },
}

#[near(serializers=[json])]
#[derive(Clone, Copy)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum WithdrawAmount {
    Full { at_least: Option<U128> },
    Exact(U128),
    PreviousSwapOutput,
}

#[near(serializers=[json])]
#[derive(Clone, Copy)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum DirectWithdrawAmount {
    Full { at_least: Option<U128> },
    Exact(U128),
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

    /// Core WASM execution logic shared between swap, dex_call, and dex_view methods.
    /// Returns the raw response bytes after running the WASM function.
    fn run_wasm_function<'a>(
        code: &[u8],
        dex_id: &DexId,
        call_type: CallType<'a>,
        request: Vec<u8>,
        method: &str,
        dex_storage_balances: &'a crate::storage_management::StorageBalances<DexId>,
        referrer: Option<AccountId>,
    ) -> Option<Vec<u8>> {
        let engine = Engine::default();
        let module = match Module::new(&engine, code) {
            Ok(module) => module,
            Err(err) => panic!("Failed to load module: {err:?}"),
        };

        let storage_usage_before = near_sdk::env::storage_usage();
        let mut store = Store::new(
            &engine,
            RunnerData {
                request,
                response: None,
                registers: HashMap::new(),
                call_type,
                dex_id: dex_id.clone(),
                dex_storage_balances,
                dex_storage_usage_before_transaction: storage_usage_before,
                referrer,
            },
        );
        let mut linker = Linker::new(&engine);

        crate::impl_supported_host_functions!(linker);
        // might be needed for some near-sdk features that are actually never called,
        // but not for any of the dexes in this workspace, and this helps reduce wasm
        // size by 4kb.
        // crate::impl_unsupported_host_functions!(linker);

        let instance = match linker.instantiate_and_start(&mut store, &module) {
            Ok(i) => i,
            Err(err) => panic!("Failed to instantiate module: {err:?}"),
        };
        let func: Func = match instance.get_func(&mut store, method) {
            Some(f) => f,
            None => panic!("Failed to get function '{method}'"),
        };
        match func.call(&mut store, &[], &mut []) {
            Ok(()) => (),
            Err(err) => panic!("Failed to call function '{method}': {err:?}"),
        };
        store.data_mut().response.take()
    }

    pub(crate) fn internal_swap_simple(
        &mut self,
        dex_id: DexId,
        message: Base64VecU8,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
        mut trader: TradeAccount,
        referrer: Option<AccountId>,
        constraint: Option<U128>,
    ) -> (U128, U128) {
        let swap_request = SwapRequest {
            message,
            asset_in,
            asset_out,
            amount,
        };

        let code = self.dex_codes.get(&dex_id).expect("Dex code not found");
        let storage_usage_before = near_sdk::env::storage_usage();
        let response = Self::run_wasm_function(
            code,
            &dex_id,
            CallType::Trade {
                dex_storage_mut: &mut self.dex_storage,
                alleged_trader: match &trader {
                    TradeAccount::User(account) => account.clone(),
                    TradeAccount::Sandboxed { alleged_trader, .. } => alleged_trader.clone(),
                },
            },
            near_sdk::borsh::to_vec(&swap_request).expect("Failed to serialize swap request"),
            "swap",
            &self.dex_storage_balances,
            referrer.clone(),
        );

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
                if let Some(constraint) = constraint {
                    expect!(
                        response.amount_out >= constraint,
                        "Output amount is less than constraint"
                    );
                }
            }
            SwapRequestAmount::ExactOut(exact_out) => {
                expect!(
                    exact_out == response.amount_out,
                    "Amount out does not match"
                );
                if let Some(constraint) = constraint {
                    expect!(
                        response.amount_in <= constraint,
                        "Input amount is greater than constraint"
                    );
                }
            }
        }

        match &mut trader {
            TradeAccount::User(user_trader) => {
                // asset in
                self.internal_transfer_asset(
                    AccountOrDexId::Account(user_trader.clone()),
                    AccountOrDexId::Dex(dex_id.clone()),
                    swap_request.asset_in.clone(),
                    response.amount_in,
                );
                // asset out
                self.internal_transfer_asset(
                    AccountOrDexId::Dex(dex_id.clone()),
                    AccountOrDexId::Account(user_trader.clone()),
                    swap_request.asset_out.clone(),
                    response.amount_out,
                );
            }
            TradeAccount::Sandboxed { assets, .. } => {
                // asset in
                let anon_swap_balance_in = assets
                    .get_mut(&swap_request.asset_in)
                    .expect("Asset in not found in anonymous assets");
                anon_swap_balance_in.0 = anon_swap_balance_in
                    .0
                    .checked_sub(response.amount_in.0)
                    .expect("Not enough input balance in anonymous assets");
                self.internal_increase_assets(
                    AccountOrDexId::Dex(dex_id.clone()),
                    swap_request.asset_in.clone(),
                    response.amount_in,
                );
                // asset out
                self.internal_decrease_assets(
                    AccountOrDexId::Dex(dex_id.clone()),
                    swap_request.asset_out.clone(),
                    response.amount_out,
                );
                let anon_swap_balance_out =
                    assets.entry(swap_request.asset_out.clone()).or_default();
                anon_swap_balance_out.0 = anon_swap_balance_out
                    .0
                    .checked_add(response.amount_out.0)
                    .expect("Balance overflow");
            }
        }
        IntearDexEvent::Swap {
            dex_id: dex_id.clone(),
            request: swap_request,
            amount_in: response.amount_in,
            amount_out: response.amount_out,
            trader: match trader {
                TradeAccount::User(account) => account,
                TradeAccount::Sandboxed { alleged_trader, .. } => alleged_trader,
            },
            referrer,
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
        anon_swap_available_assets: Option<&mut HashMap<AssetId, U128>>,
        referrer: Option<AccountId>,
    ) -> Base64VecU8 {
        expect!(
            method != "swap",
            "Method name 'swap' is reserved for the swap operation"
        );

        if anon_swap_available_assets.is_none() {
            for (asset_id, amount) in attached_assets.clone() {
                self.assert_has_enough(
                    AccountOrDexId::Account(predecessor.clone()),
                    asset_id.clone(),
                    amount,
                );
            }
        }

        let code = self.dex_codes.get(&dex_id).expect("Dex code not found");
        let storage_usage_before = near_sdk::env::storage_usage();
        let request = DexCallRequest {
            args: args.0,
            attached_assets,
        };
        let response = Self::run_wasm_function(
            code,
            &dex_id,
            CallType::Call {
                dex_storage_mut: &mut self.dex_storage,
                predecessor_id: predecessor.clone(),
                is_authorized: anon_swap_available_assets.is_none(),
            },
            near_sdk::borsh::to_vec(&request).expect("Failed to serialize request"),
            &method,
            &self.dex_storage_balances,
            referrer,
        );

        let response: DexCallResponse = match response {
            Some(response) => near_sdk::borsh::from_slice(&response)
                .expect("Failed to deserialize dex call response"),
            None => Default::default(),
        };
        if let Some(anon_swap_available_assets) = anon_swap_available_assets {
            for (asset_id, amount) in request.attached_assets {
                anon_swap_available_assets
                    .entry(asset_id.clone())
                    .and_modify(|b| {
                        b.0 = b.0.checked_sub(amount.0).unwrap_or_else(|| {
                            panic!("Not enough balance for attached deposit of {asset_id}")
                        });
                    })
                    .or_insert_with(|| {
                        panic!("Asset {asset_id} was not attached, while specified that it was attached")
                    });
                self.internal_increase_assets(
                    AccountOrDexId::Dex(dex_id.clone()),
                    asset_id,
                    amount,
                );
            }
        } else {
            for (asset_id, amount) in request.attached_assets {
                self.internal_transfer_asset(
                    AccountOrDexId::Account(predecessor.clone()),
                    AccountOrDexId::Dex(dex_id.clone()),
                    asset_id.clone(),
                    amount,
                );
            }
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
                        DirectWithdrawAmount::Exact(amount),
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

        self.dex_storage.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.dex_storage_balances
            .charge(&dex_id, storage_usage_before, storage_usage_after);

        Base64VecU8::from(response.response)
    }

    pub(crate) fn internal_simulate_swap_simple(
        &self,
        dex_id: DexId,
        message: Base64VecU8,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
        referrer: Option<AccountId>,
    ) -> (U128, U128) {
        let swap_request = SwapRequest {
            message,
            asset_in,
            asset_out,
            amount,
        };

        let code = self.dex_codes.get(&dex_id).expect("Dex code not found");
        let response = Self::run_wasm_function(
            code,
            &dex_id,
            CallType::TradeView {
                dex_storage: &self.dex_storage,
                ephemeral_storage_updates: HashMap::new(),
            },
            near_sdk::borsh::to_vec(&swap_request).expect("Failed to serialize swap request"),
            "swap",
            &self.dex_storage_balances,
            referrer,
        );
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

        (response.amount_in, response.amount_out)
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
        let response = Self::run_wasm_function(
            code,
            &dex_id,
            CallType::View {
                dex_storage: &self.dex_storage,
            },
            args.0,
            &method,
            &self.dex_storage_balances,
            None,
        );

        Base64VecU8::from(response.unwrap_or_default())
    }

    pub(crate) fn internal_register_assets(
        &mut self,
        asset_ids: Vec<AssetId>,
        r#for: Option<AccountOrDexId>,
        storage_payer: AccountId,
    ) {
        let r#for = r#for.unwrap_or_else(|| AccountOrDexId::Account(storage_payer.clone()));
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
        self.user_storage_balances.charge(
            &storage_payer,
            storage_usage_before,
            storage_usage_after,
        );
    }

    pub(crate) fn internal_withdraw(
        &mut self,
        asset_id: AssetId,
        amount: DirectWithdrawAmount,
        withdraw_to: Option<AccountId>,
        withdraw_from: AccountOrDexId,
    ) -> PromiseOrValue<bool> {
        let amount = match amount {
            DirectWithdrawAmount::Full { at_least } => {
                let balance = self
                    .asset_balance_of(withdraw_from.clone(), asset_id.clone())
                    .unwrap_or_default();
                if balance >= at_least.unwrap_or_default() {
                    balance
                } else {
                    panic!(
                        "Not enough balance for withdraw {asset_id}: {} < {:?}",
                        balance.0, at_least
                    );
                }
            }
            DirectWithdrawAmount::Exact(amount) => amount,
        };
        if amount.0 == 0 {
            return PromiseOrValue::Value(true);
        }
        self.internal_decrease_assets(withdraw_from.clone(), asset_id.clone(), amount);
        self.total_in_custody
            .entry(asset_id.clone())
            .and_modify(|b| {
                b.0 = b.0.checked_sub(amount.0).unwrap_or_else(|| {
                    panic!(
                        "Balance underflow for contract and asset {asset_id}: {} - {} < {}",
                        b.0,
                        amount.0,
                        u128::MIN,
                    )
                })
            })
            .or_insert_with(|| {
                panic!(
                    "Failed to withdraw assets from contract tracked balance: asset {asset_id} not registered"
                )
            });

        let withdraw_to = withdraw_to.unwrap_or_else(|| match withdraw_from.clone() {
            AccountOrDexId::Account(account) => account,
            AccountOrDexId::Dex(_) => {
                panic!("withdraw_to must be present when withdrawing from a dex")
            }
        });
        self.internal_withdraw_unchecked(asset_id, amount, withdraw_to, withdraw_from)
    }

    /// Withdraws assets without reducing or checking any balances.
    fn internal_withdraw_unchecked(
        &mut self,
        asset_id: AssetId,
        amount: U128,
        withdraw_to: AccountId,
        withdraw_from: AccountOrDexId,
    ) -> PromiseOrValue<bool> {
        const GAS_FOR_FT_TRANSFER: Gas = Gas::from_tgas(10);
        const GAS_FOR_NFT_TRANSFER: Gas = Gas::from_tgas(10);
        const GAS_FOR_MT_TRANSFER: Gas = Gas::from_tgas(10);
        const GAS_FOR_WITHDRAWAL_CALLBACK: Gas = Gas::from_tgas(5);

        PromiseOrValue::Promise(match &asset_id {
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
                        "receiver_id": withdraw_to,
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
        })
    }

    pub(crate) fn internal_execute_operations(
        &mut self,
        operations: Vec<Operation>,
        by: AccountId,
        mut anon_swap_available_assets: Option<HashMap<AssetId, U128>>,
        referrer: Option<AccountId>,
    ) {
        let fully_authorized = anon_swap_available_assets.as_ref().is_none();
        near_sdk::env::log_str(&format!("Fully authorized: {fully_authorized}"));
        let mut last_output = None;
        for operation in operations {
            match operation {
                Operation::RegisterAssets { asset_ids, r#for } => {
                    if !fully_authorized {
                        panic!("Operation only available in execute_actions");
                    }
                    self.internal_register_assets(asset_ids, r#for, by.clone());
                }
                Operation::DeployDexCode {
                    last_part_of_id,
                    code_base64,
                } => {
                    if !fully_authorized {
                        panic!("Operation only available in execute_actions");
                    }
                    self.internal_deploy_dex_code(last_part_of_id, code_base64, by.clone());
                }
                Operation::Withdraw {
                    asset_id,
                    amount,
                    to,
                    rescue_address,
                } => {
                    if let Some(anonymous_assets) = &mut anon_swap_available_assets {
                        let asset_balance = anonymous_assets
                            .get_mut(&asset_id)
                            .expect("Asset to withdraw not found in anonymous assets");
                        let amount = match amount {
                            WithdrawAmount::Full { at_least } => {
                                if *asset_balance >= at_least.unwrap_or_default() {
                                    *asset_balance
                                } else {
                                    panic!(
                                        "Not enough balance for withdraw {asset_id}: {} < {:?}",
                                        asset_balance.0, at_least,
                                    );
                                }
                            }
                            WithdrawAmount::Exact(amount) => amount,
                            WithdrawAmount::PreviousSwapOutput => match &last_output {
                                Some((last_asset_out, amount)) => {
                                    if *last_asset_out == asset_id {
                                        *amount
                                    } else {
                                        panic!(
                                            "PreviousSwapOutput can only be used if the last swap asset out matches the withdraw asset"
                                        );
                                    }
                                }
                                None => panic!(
                                    "PreviousSwapOutput can only be used after a swap, in the same operation batch"
                                ),
                            },
                        };
                        asset_balance.0 = asset_balance
                            .0
                            .checked_sub(amount.0)
                            .expect("Not enough balance in anonymous assets");
                        let rescue_address = if self.asset_is_registered(
                            AccountOrDexId::Account(by.clone()),
                            asset_id.clone(),
                        ) {
                            by.clone()
                        } else if let Some(rescue_address) = rescue_address {
                            self.assert_asset_registered(
                                AccountOrDexId::Account(rescue_address.clone()),
                                asset_id.clone(),
                            );
                            rescue_address.clone()
                        } else {
                            panic!(
                                "No rescue address provided and user doesn't have a registered balance for this asset"
                            );
                        };
                        self.total_in_custody
                            .entry(asset_id.clone())
                            .and_modify(|b| {
                                b.0 = b.0.checked_sub(amount.0).unwrap_or_else(|| {
                                    panic!(
                                        "Balance underflow for contract and asset {asset_id}: {} - {} < {}",
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
                        self.internal_withdraw_unchecked(
                            asset_id,
                            amount,
                            by.clone(),
                            AccountOrDexId::Account(rescue_address.clone()),
                        )
                        .detach();
                    } else {
                        self.internal_withdraw(
                            asset_id.clone(),
                            match amount {
                                WithdrawAmount::Full { at_least } => DirectWithdrawAmount::Full { at_least },
                                WithdrawAmount::Exact(amount) => DirectWithdrawAmount::Exact(amount),
                                WithdrawAmount::PreviousSwapOutput => {
                                    match &last_output {
                                        Some((last_asset_out, amount)) => {
                                            if *last_asset_out == asset_id {
                                                DirectWithdrawAmount::Exact(*amount)
                                            } else {
                                                panic!("PreviousSwapOutput can only be used if the last swap asset out matches the withdraw asset");
                                            }
                                        }
                                        None => panic!("PreviousSwapOutput can only be used after a swap, in the same operation batch"),
                                    }
                                },
                            },
                            to,
                            AccountOrDexId::Account(by.clone()),
                        )
                        .detach();
                    }
                }
                Operation::SwapSimple {
                    dex_id,
                    message,
                    asset_in,
                    asset_out,
                    amount,
                    constraint,
                } => {
                    let amount = match amount {
                        SwapOperationAmount::Amount(amount) => amount,
                        SwapOperationAmount::OutputOfLastIn => match last_output {
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
                        SwapOperationAmount::EntireBalanceIn => {
                            SwapRequestAmount::ExactIn(match &anon_swap_available_assets {
                                Some(assets) => *assets
                                    .get(&asset_in)
                                    .expect("Asset in not found in anonymous assets"),
                                None => self
                                    .asset_balance_of(
                                        AccountOrDexId::Account(by.clone()),
                                        asset_in.clone(),
                                    )
                                    .unwrap_or_default(),
                            })
                        }
                    };
                    let (_amount_in, amount_out) = self.internal_swap_simple(
                        dex_id,
                        message,
                        asset_in,
                        asset_out.clone(),
                        amount,
                        match &mut anon_swap_available_assets {
                            Some(assets) => TradeAccount::Sandboxed {
                                assets,
                                alleged_trader: by.clone(),
                            },
                            None => TradeAccount::User(by.clone()),
                        },
                        referrer.clone(),
                        constraint,
                    );
                    last_output = Some((asset_out, amount_out));
                }
                Operation::DexCall {
                    dex_id,
                    method,
                    args,
                    attached_assets,
                } => {
                    self.internal_dex_call(
                        dex_id,
                        method,
                        args,
                        attached_assets,
                        by.clone(),
                        anon_swap_available_assets.as_mut(),
                        referrer.clone(),
                    );
                }
                Operation::TransferAsset {
                    to,
                    asset_id,
                    amount,
                } => match &mut anon_swap_available_assets {
                    Some(assets) => {
                        let asset_balance = assets
                            .get_mut(&asset_id)
                            .expect("Asset to transfer not found in anonymous assets");
                        asset_balance.0 = asset_balance
                            .0
                            .checked_sub(amount.0)
                            .expect("Not enough balance in anonymous assets");
                        self.internal_increase_assets(to, asset_id, amount);
                    }
                    None => {
                        self.internal_transfer_asset(
                            AccountOrDexId::Account(by.clone()),
                            to,
                            asset_id,
                            amount,
                        );
                    }
                },
                Operation::StorageDeposit { amount, r#for } => {
                    if let Some(sandboxed_assets) = &mut anon_swap_available_assets {
                        let near_balance = sandboxed_assets
                            .get_mut(&AssetId::Near)
                            .expect("Near balance not found in anonymous assets");
                        near_balance.0 = near_balance
                            .0
                            .checked_sub(amount.0)
                            .expect("Not enough near balance in anonymous assets");
                    }
                    match r#for {
                        Some(AccountOrDexId::Account(account)) => {
                            self.user_storage_balances.storage_deposit(
                                account,
                                Some(false),
                                NearToken::from_yoctonear(amount.0),
                            );
                        }
                        Some(AccountOrDexId::Dex(dex_id)) => {
                            self.dex_storage_balances.storage_deposit(
                                dex_id,
                                Some(false),
                                NearToken::from_yoctonear(amount.0),
                            );
                        }
                        None => {
                            panic!("r#for is required for StorageDeposit operation");
                        }
                    }
                }
            }
        }
        for (asset_id, amount) in anon_swap_available_assets.unwrap_or_default() {
            expect!(
                amount.0 == 0,
                "Sandboxed assets must be empty after execution. Did you forget to withdraw {asset_id}?"
            );
        }
    }
}

#[near]
impl DexEngine {
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
                    "Refunding to {withdraw_from} because withdrawal to {withdraw_to} failed: {error:?}"
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
