use std::collections::HashMap;

use intear_dex_types::{
    AccountOrDexId, AssetId, AssetWithdrawRequest, AssetWithdrawalType, DexCallRequest,
    DexCallResponse, DexEventData, DexId, DirectWithdrawAmount, Operation, ReferralFees,
    SwapAdvancedMode, SwapOperationAmount, SwapOption, SwapRequest, SwapRequestAmount,
    SwapResponse, WithdrawAmount, expect,
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
    CallType, DexEngine, DexEngineExt, IntearDexEvent, PendingStorageUpdates, RunnerData,
    call_events::CallEvents,
};

pub enum TradeAccount<'a> {
    User(AccountId),
    Sandboxed {
        assets: &'a mut HashMap<AssetId, U128>,
        trader: intear_dex_types::Caller,
    },
}

/// What a dex did in a run of one of its functions
struct DexRun {
    response: Option<Vec<u8>>,
    dex_events: Vec<DexEventData>,
    pending_storage_updates: PendingStorageUpdates,
    pending_logs: Vec<String>,
}

/// A swap that a dex ran without writing or logging anything, which can be made real
pub(crate) struct SimulatedSwap {
    dex_id: DexId,
    request: SwapRequest,
    response: SwapResponse,
    referral_fees_paid: Vec<(AccountId, U128)>,
    pending_storage_updates: PendingStorageUpdates,
    pending_logs: Vec<String>,
    dex_events: Vec<DexEventData>,
}

impl SimulatedSwap {
    pub(crate) fn total_amount_in(&self) -> U128 {
        amount_in_with_fees(self.response.amount_in, &self.referral_fees_paid)
    }

    pub(crate) fn amount_out(&self) -> U128 {
        self.response.amount_out
    }

    /// Why the swap doesn't meet `constraint`, if it doesn't
    fn constraint_violation_message(&self, constraint: Option<U128>) -> Option<&'static str> {
        let constraint = constraint?;
        match self.request.amount {
            SwapRequestAmount::ExactIn(_) if self.amount_out() < constraint => {
                Some("Output amount is less than constraint")
            }
            SwapRequestAmount::ExactOut(_) if self.total_amount_in() > constraint => {
                Some("Input amount is greater than constraint")
            }
            _ => None,
        }
    }
}

impl TradeAccount<'_> {
    fn caller(&self) -> intear_dex_types::Caller {
        match self {
            TradeAccount::User(account) => intear_dex_types::Caller::Direct(account.clone()),
            TradeAccount::Sandboxed { trader, .. } => trader.clone(),
        }
    }
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
        expect!(
            near_sdk::env::predecessor_account_id() == self.trusted_code_deployer,
            "Only the trusted code deployer can deploy dex code"
        );
        // SAFETY: can only be deployed by trusted callers. Currently only slimedragon.near
        // can deploy, later another smart contract will validate and deploy permissionlessly.
        if let Err(err) = unsafe { Module::new_unchecked(&Engine::default(), &code_base64.0) } {
            near_sdk::env::panic_str(&format!("Invalid dex code: {err}"));
        }
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
    /// Returns what the dex did, or why it failed.
    fn run_wasm_function<'a>(
        code: &[u8],
        dex_id: &DexId,
        call_type: CallType<'a>,
        request: Vec<u8>,
        method: &str,
        dex_storage_balances: &'a crate::storage_management::StorageBalances<DexId>,
        referrer: Option<AccountId>,
    ) -> Result<DexRun, String> {
        let engine = Engine::default();
        // SAFETY: already validated in internal_deploy_dex_code
        let module = match unsafe { Module::new_unchecked(&engine, code) } {
            Ok(module) => module,
            Err(_) => panic!("Failed to load module"),
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
                dex_events: Vec::new(),
                pending_logs: Vec::new(),
            },
        );
        let mut linker = Linker::new(&engine);

        crate::impl_supported_host_functions!(linker);
        // might be needed for some near-sdk features that are actually never called,
        // but not for any of the dexes in this workspace, and this helps reduce wasm
        // size by 4kb.
        // crate::impl_unsupported_host_functions!(linker);

        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|err| format!("Failed to instantiate module: {err}"))?;
        let func: Func = instance
            .get_func(&mut store, method)
            .ok_or_else(|| format!("Failed to get function '{method}'"))?;
        func.call(&mut store, &[], &mut [])
            .map_err(|err| format!("Failed to call function '{method}': {err}"))?;
        let runner_data = store.into_data();
        Ok(DexRun {
            response: runner_data.response,
            dex_events: runner_data.dex_events,
            pending_storage_updates: match runner_data.call_type {
                CallType::Trade {
                    pending_storage_updates,
                    ..
                } => pending_storage_updates,
                _ => HashMap::new(),
            },
            pending_logs: runner_data.pending_logs,
        })
    }

    pub(crate) fn internal_swap_simple(
        &mut self,
        events: &mut CallEvents,
        dex_id: DexId,
        message: Base64VecU8,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
        trader: TradeAccount,
        referrer: Option<AccountId>,
        referral_fees: ReferralFees,
        constraint: Option<U128>,
    ) -> (U128, U128) {
        let swap = self
            .internal_simulate_swap_simple(
                dex_id,
                message,
                trader.caller(),
                asset_in,
                asset_out,
                amount,
                referrer.clone(),
                referral_fees,
            )
            .unwrap_or_else(|err| panic!("{err}"));
        self.internal_commit_swap(events, swap, trader, referrer, constraint)
    }

    /// Writes what a simulated swap did, moves the assets and logs the swap
    fn internal_commit_swap(
        &mut self,
        events: &mut CallEvents,
        swap: SimulatedSwap,
        trader: TradeAccount,
        referrer: Option<AccountId>,
        constraint: Option<U128>,
    ) -> (U128, U128) {
        if let Some(violation_message) = swap.constraint_violation_message(constraint) {
            panic!("{violation_message}");
        }
        let total_amount_in = swap.total_amount_in();
        let SimulatedSwap {
            dex_id,
            request: swap_request,
            response,
            referral_fees_paid,
            pending_storage_updates,
            pending_logs,
            dex_events,
        } = swap;
        let storage_usage_before = near_sdk::env::storage_usage();
        for (key, value) in pending_storage_updates {
            match value {
                Some(value) => self.dex_storage.insert((dex_id.clone(), key), value),
                None => self.dex_storage.remove(&(dex_id.clone(), key)),
            };
        }
        self.dex_storage.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.dex_storage_balances
            .charge(&dex_id, storage_usage_before, storage_usage_after);
        for log in pending_logs {
            near_sdk::env::log_str(&log);
        }
        events.extend_dex_events(dex_events);

        let dex_trader = trader.caller();
        match trader {
            TradeAccount::User(user_trader) => {
                // asset in
                self.internal_transfer_asset(
                    events,
                    AccountOrDexId::Account(user_trader.clone()),
                    AccountOrDexId::Dex(dex_id.clone()),
                    swap_request.asset_in.clone(),
                    response.amount_in,
                );
                for (referrer_id, fee) in &referral_fees_paid {
                    self.internal_transfer_asset(
                        events,
                        AccountOrDexId::Account(user_trader.clone()),
                        AccountOrDexId::Account(referrer_id.clone()),
                        swap_request.asset_in.clone(),
                        *fee,
                    );
                }
                // asset out
                self.internal_transfer_asset(
                    events,
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
                    .checked_sub(total_amount_in.0)
                    .expect("Not enough input balance in anonymous assets");
                self.internal_increase_assets(
                    events,
                    AccountOrDexId::Dex(dex_id.clone()),
                    swap_request.asset_in.clone(),
                    response.amount_in,
                );
                for (referrer_id, fee) in &referral_fees_paid {
                    // already decreased sandboxed balance by subtracting total_amount_in
                    self.internal_increase_assets(
                        events,
                        AccountOrDexId::Account(referrer_id.clone()),
                        swap_request.asset_in.clone(),
                        *fee,
                    );
                }
                // asset out
                self.internal_decrease_assets(
                    events,
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
            trader: dex_trader.alleged_account_id().clone(),
            referrer,
            referral_fees_paid,
        }
        .emit();

        (total_amount_in, response.amount_out)
    }

    /// Simulates the swap on the options and makes the simulation that `mode` picks real
    pub(crate) fn internal_swap_advanced(
        &mut self,
        events: &mut CallEvents,
        options: Vec<SwapOption>,
        mode: SwapAdvancedMode,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
        trader: TradeAccount,
        referrer: Option<AccountId>,
        referral_fees: ReferralFees,
        constraint: Option<U128>,
    ) -> (U128, U128) {
        expect!(!options.is_empty(), "At least one swap option is required");
        let mut best: Option<SimulatedSwap> = None;
        for option in options {
            let simulation = self.internal_simulate_swap_simple(
                option.dex_id.clone(),
                option.message,
                trader.caller(),
                asset_in.clone(),
                asset_out.clone(),
                amount,
                referrer.clone(),
                referral_fees.clone(),
            );
            let simulation = match simulation {
                Ok(simulation) => simulation,
                Err(err) => {
                    near_sdk::env::log_str(&format!(
                        "Skipping swap option on {}: {err}",
                        option.dex_id
                    ));
                    continue;
                }
            };
            if mode == SwapAdvancedMode::FallbackQueue {
                if let Some(violation) = simulation.constraint_violation_message(constraint) {
                    near_sdk::env::log_str(&format!(
                        "Skipping swap option on {}: {violation}",
                        option.dex_id
                    ));
                    continue;
                }
                return self.internal_commit_swap(events, simulation, trader, referrer, constraint);
            }
            let is_better = match (&best, amount) {
                (None, _) => true,
                (Some(best), SwapRequestAmount::ExactIn(_)) => {
                    simulation.amount_out() > best.amount_out()
                }
                (Some(best), SwapRequestAmount::ExactOut(_)) => {
                    simulation.total_amount_in() < best.total_amount_in()
                }
            };
            if is_better {
                best = Some(simulation);
            }
        }
        let Some(best) = best else {
            panic!("All swap options failed");
        };

        self.internal_commit_swap(events, best, trader, referrer, constraint)
    }

    /// The exact amount of a swap operation, which can depend on the previous swap or the balance
    fn swap_request_amount(
        &self,
        amount: SwapOperationAmount,
        asset_in: &AssetId,
        last_output: &Option<(AssetId, U128)>,
        anon_swap_available_assets: Option<&HashMap<AssetId, U128>>,
        by: &AccountId,
    ) -> SwapRequestAmount {
        match amount {
            SwapOperationAmount::Amount(amount) => amount,
            SwapOperationAmount::OutputOfLastIn => match last_output {
                Some((last_asset_out, amount)) => {
                    if last_asset_out == asset_in {
                        SwapRequestAmount::ExactIn(*amount)
                    } else {
                        panic!(
                            "Amount can only be omitted if the last swap asset out matches the current asset in"
                        );
                    }
                }
                None => panic!("Amount is required for the first swap operation"),
            },
            SwapOperationAmount::EntireBalanceIn => {
                SwapRequestAmount::ExactIn(match anon_swap_available_assets {
                    Some(assets) => *assets
                        .get(asset_in)
                        .expect("Asset in not found in anonymous assets"),
                    None => self
                        .asset_balance_of(AccountOrDexId::Account(by.clone()), asset_in.clone())
                        .unwrap_or_default(),
                })
            }
        }
    }

    /// The referral fees of the referrers who registered `asset_in`, as only they can be paid
    fn payable_referral_fees(
        &self,
        referral_fees: ReferralFees,
        asset_in: &AssetId,
    ) -> ReferralFees {
        referral_fees.assert_valid();
        ReferralFees(
            referral_fees
                .0
                .into_iter()
                .filter(|(referrer_id, _)| {
                    self.asset_is_registered(
                        AccountOrDexId::Account(referrer_id.clone()),
                        asset_in.clone(),
                    )
                })
                .collect(),
        )
    }

    pub(crate) fn internal_dex_call(
        &mut self,
        events: &mut CallEvents,
        dex_id: DexId,
        method: String,
        args: Base64VecU8,
        attached_assets: HashMap<AssetId, U128>,
        trader: intear_dex_types::Caller,
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
                    AccountOrDexId::Account(trader.alleged_account_id().clone()),
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
        let dex_run = Self::run_wasm_function(
            code,
            &dex_id,
            CallType::Call {
                dex_storage_mut: &mut self.dex_storage,
                caller: trader.clone(),
            },
            near_sdk::borsh::to_vec(&request).expect("Failed to serialize request"),
            &method,
            &self.dex_storage_balances,
            referrer,
        )
        .unwrap_or_else(|err| panic!("Running wasm failed: {err}"));
        events.extend_dex_events(dex_run.dex_events);

        let response: DexCallResponse = match dex_run.response {
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
                    events,
                    AccountOrDexId::Dex(dex_id.clone()),
                    asset_id,
                    amount,
                );
            }
        } else {
            for (asset_id, amount) in request.attached_assets {
                self.internal_transfer_asset(
                    events,
                    AccountOrDexId::Account(trader.alleged_account_id().clone()),
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
                        events,
                        AccountOrDexId::Dex(dex_id.clone()),
                        AccountOrDexId::Account(account.clone()),
                        asset_id.clone(),
                        amount,
                    );
                }
                AssetWithdrawalType::ToInternalDexBalance(other_dex_id) => {
                    self.internal_transfer_asset(
                        events,
                        AccountOrDexId::Dex(dex_id.clone()),
                        AccountOrDexId::Dex(other_dex_id.clone()),
                        asset_id.clone(),
                        amount,
                    );
                }
                AssetWithdrawalType::WithdrawUnderlyingAsset(to_account_id) => {
                    self.internal_withdraw(
                        events,
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
                events,
                AccountOrDexId::Dex(dex_id.clone()),
                AssetId::Near,
                U128(response.add_storage_deposit.as_yoctonear()),
            );
            self.total_in_custody
                .entry(AssetId::Near)
                .and_modify(|b| {
                    b.0 = b.0.checked_sub(response.add_storage_deposit.as_yoctonear())
                        .unwrap_or_else(|| {
                            panic!(
                                "Balance underflow for contract and asset Near: {} - {} < {}",
                                b.0,
                                response.add_storage_deposit.as_yoctonear(),
                                u128::MIN,
                            )
                        })
                })
                .or_insert_with(|| {
                    panic!(
                        "Failed to convert NEAR to dex storage deposit: asset Near not registered in total_in_custody"
                    )
                });
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
        trader: intear_dex_types::Caller,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
        referrer: Option<AccountId>,
        referral_fees: ReferralFees,
    ) -> Result<SimulatedSwap, String> {
        let referral_fees = self.payable_referral_fees(referral_fees, &asset_in);
        let swap_request = SwapRequest {
            message,
            asset_in,
            asset_out,
            amount: match amount {
                SwapRequestAmount::ExactIn(amount_in) => SwapRequestAmount::ExactIn(
                    amount_in_after_fees(amount_in, &referral_fees.amounts_out_of(amount_in)),
                ),
                SwapRequestAmount::ExactOut(_) => amount,
            },
            referrer: referrer.clone(),
        };

        let code = self.dex_codes.get(&dex_id).expect("Dex code not found");
        let dex_run = Self::run_wasm_function(
            code,
            &dex_id,
            CallType::Trade {
                dex_storage: &self.dex_storage,
                trader,
                pending_storage_updates: HashMap::new(),
            },
            near_sdk::borsh::to_vec(&swap_request).expect("Failed to serialize swap request"),
            "swap",
            &self.dex_storage_balances,
            referrer,
        )
        .map_err(|err| format!("Running wasm failed: {err}"))?;
        let response: SwapResponse =
            near_sdk::borsh::from_slice(&dex_run.response.ok_or("No response from swap")?)
                .map_err(|_| "Failed to deserialize swap response")?;
        match swap_request.amount {
            SwapRequestAmount::ExactIn(exact_in) if exact_in != response.amount_in => {
                return Err("Amount in does not match".to_string());
            }
            SwapRequestAmount::ExactOut(exact_out) if exact_out != response.amount_out => {
                return Err("Amount out does not match".to_string());
            }
            _ => (),
        }

        Ok(SimulatedSwap {
            referral_fees_paid: match amount {
                SwapRequestAmount::ExactIn(amount_in) => referral_fees.amounts_out_of(amount_in),
                SwapRequestAmount::ExactOut(_) => {
                    referral_fees.amounts_on_top_of(response.amount_in)
                }
            }
            .into_iter()
            .filter(|(_, fee)| fee.0 > 0)
            .collect(),
            dex_id,
            request: swap_request,
            response,
            pending_storage_updates: dex_run.pending_storage_updates,
            pending_logs: dex_run.pending_logs,
            dex_events: dex_run.dex_events,
        })
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
        let dex_run = Self::run_wasm_function(
            code,
            &dex_id,
            CallType::View {
                dex_storage: &self.dex_storage,
            },
            args.0,
            &method,
            &self.dex_storage_balances,
            None,
        )
        .unwrap_or_else(|err| panic!("Running wasm failed: {err}"));

        Base64VecU8::from(dex_run.response.unwrap_or_default())
    }

    pub(crate) fn internal_register_assets(
        &mut self,
        asset_ids: Vec<AssetId>,
        r#for: Option<AccountOrDexId>,
        storage_payer: AccountId,
    ) {
        let r#for = r#for.unwrap_or_else(|| AccountOrDexId::Account(storage_payer.clone()));
        let storage_usage_before = near_sdk::env::storage_usage();
        let asset_balances = self.asset_balances_or_new(&r#for);
        for asset_id in &asset_ids {
            if !asset_balances.contains_key(asset_id) {
                asset_balances.insert(asset_id.clone(), U128(0));
            }
        }
        asset_balances.flush();
        for asset_id in asset_ids {
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
        events: &mut CallEvents,
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
                        "Not enough balance for withdraw {asset_id}: {} < {at_least:?}",
                        balance.0,
                    );
                }
            }
            DirectWithdrawAmount::Exact(amount) => amount,
        };
        if amount.0 == 0 {
            return PromiseOrValue::Value(true);
        }
        self.internal_decrease_assets(events, withdraw_from.clone(), asset_id.clone(), amount);
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
        const GAS_FOR_WITHDRAWAL_CALLBACK: Gas = Gas::from_tgas(5);

        PromiseOrValue::Promise(
            Self::transfer_asset_promise(&asset_id, amount, withdraw_to.clone()).then(
                Self::ext(near_sdk::env::current_account_id())
                    .with_static_gas(GAS_FOR_WITHDRAWAL_CALLBACK)
                    .after_withdraw(asset_id, amount, withdraw_to, withdraw_from),
            ),
        )
    }

    /// Sends assets from this contract's account, without touching any balances.
    pub(crate) fn transfer_asset_promise(
        asset_id: &AssetId,
        amount: U128,
        to: AccountId,
    ) -> Promise {
        const GAS_FOR_FT_TRANSFER: Gas = Gas::from_tgas(10);
        const GAS_FOR_NFT_TRANSFER: Gas = Gas::from_tgas(10);
        const GAS_FOR_MT_TRANSFER: Gas = Gas::from_tgas(10);

        match asset_id {
            AssetId::Near => Promise::new(to).transfer(NearToken::from_yoctonear(amount.0)),
            AssetId::Nep141(contract_id) => ext_ft_core::ext(contract_id.clone())
                .with_attached_deposit(NearToken::from_yoctonear(1))
                .with_static_gas(GAS_FOR_FT_TRANSFER)
                .ft_transfer(to, amount, None),
            AssetId::Nep171(contract_id, token_id) => ext_nft_core::ext(contract_id.clone())
                .with_attached_deposit(NearToken::from_yoctonear(1))
                .with_static_gas(GAS_FOR_NFT_TRANSFER)
                .nft_transfer(to, token_id.clone(), None, None),
            AssetId::Nep245(contract_id, token_id) => Promise::new(contract_id.clone())
                .function_call(
                    "mt_transfer",
                    near_sdk::serde_json::json!({
                        "receiver_id": to,
                        "token_id": token_id,
                        "amount": amount,
                        "approval": null,
                        "memo": null,
                    })
                    .to_string()
                    .into_bytes(),
                    NearToken::from_yoctonear(1),
                    GAS_FOR_MT_TRANSFER,
                ),
        }
    }

    pub(crate) fn internal_execute_operations(
        &mut self,
        events: &mut CallEvents,
        operations: Vec<Operation>,
        trader: intear_dex_types::Caller,
        mut anon_swap_available_assets: Option<HashMap<AssetId, U128>>,
        referrer: Option<AccountId>,
        mut referral_fees_of_first_swap: ReferralFees,
    ) {
        let by = trader.alleged_account_id().clone();
        let fully_authorized = anon_swap_available_assets.as_ref().is_none();
        near_sdk::env::log_str(&format!("Fully authorized: {fully_authorized}"));
        let mut last_output = None;
        for (operation_index, operation) in operations.into_iter().enumerate() {
            near_sdk::env::log_str(&format!("Executing operation {operation_index}"));
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
                                if let Some(at_least) = at_least {
                                    if *asset_balance >= at_least {
                                        *asset_balance
                                    } else {
                                        panic!(
                                            "Not enough balance for withdrawal of {asset_id}: {} < {:?}",
                                            asset_balance.0, at_least,
                                        );
                                    }
                                } else {
                                    *asset_balance
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
                        if amount.0 > 0 {
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
                                to.unwrap_or_else(|| by.clone()),
                                AccountOrDexId::Account(rescue_address.clone()),
                            )
                            .detach();
                        }
                    } else {
                        self.internal_withdraw(
                            events,
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
                    let amount = self.swap_request_amount(
                        amount,
                        &asset_in,
                        &last_output,
                        anon_swap_available_assets.as_ref(),
                        &by,
                    );
                    let (_amount_in, amount_out) = self.internal_swap_simple(
                        events,
                        dex_id,
                        message,
                        asset_in,
                        asset_out.clone(),
                        amount,
                        match &mut anon_swap_available_assets {
                            Some(assets) => TradeAccount::Sandboxed {
                                assets,
                                trader: trader.clone(),
                            },
                            None => TradeAccount::User(by.clone()),
                        },
                        referrer.clone(),
                        std::mem::take(&mut referral_fees_of_first_swap),
                        constraint,
                    );
                    last_output = Some((asset_out, amount_out));
                }
                Operation::SwapAdvanced {
                    asset_in,
                    asset_out,
                    amount,
                    constraint,
                    options,
                    mode,
                } => {
                    let amount = self.swap_request_amount(
                        amount,
                        &asset_in,
                        &last_output,
                        anon_swap_available_assets.as_ref(),
                        &by,
                    );
                    let (_amount_in, amount_out) = self.internal_swap_advanced(
                        events,
                        options,
                        mode,
                        asset_in,
                        asset_out.clone(),
                        amount,
                        match &mut anon_swap_available_assets {
                            Some(assets) => TradeAccount::Sandboxed {
                                assets,
                                trader: trader.clone(),
                            },
                            None => TradeAccount::User(by.clone()),
                        },
                        referrer.clone(),
                        std::mem::take(&mut referral_fees_of_first_swap),
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
                        events,
                        dex_id,
                        method,
                        args,
                        attached_assets,
                        trader.clone(),
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
                        self.internal_increase_assets(events, to, asset_id, amount);
                    }
                    None => {
                        self.internal_transfer_asset(
                            events,
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
                    } else {
                        self.internal_decrease_assets(
                            events,
                            AccountOrDexId::Account(by.clone()),
                            AssetId::Near,
                            amount,
                        );
                    }
                    self.total_in_custody
                        .entry(AssetId::Near)
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
                                "Failed to convert NEAR to storage deposit: asset Near not registered in total_in_custody"
                            )
                        });
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
                "Sandboxed assets must be empty after execution. Did you forget to withdraw {} {asset_id}?",
                amount.0
            );
        }
    }
}

fn amount_in_after_fees(amount_in: U128, referral_fees_paid: &[(AccountId, U128)]) -> U128 {
    referral_fees_paid
        .iter()
        .try_fold(amount_in.0, |amount, (_, fee)| amount.checked_sub(fee.0))
        .map(U128)
        .expect("Referral fees exceed the amount in")
}

fn amount_in_with_fees(dex_amount_in: U128, referral_fees_paid: &[(AccountId, U128)]) -> U128 {
    referral_fees_paid
        .iter()
        .try_fold(dex_amount_in.0, |amount, (_, fee)| {
            amount.checked_add(fee.0)
        })
        .map(U128)
        .expect("Overflow")
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
        let mut events = CallEvents::default();
        near_sdk::env::log_str(&format!("After withdraw: {result:?}"));
        let withdrawn = match result {
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
                if self.asset_is_registered(
                    AccountOrDexId::Account(withdraw_to.clone()),
                    asset_id.clone(),
                ) {
                    near_sdk::env::log_str(&format!(
                        "Refunding to {withdraw_to} (recipient) because withdrawal to {withdraw_to} failed: {error:?}"
                    ));
                    self.internal_increase_assets(
                        &mut events,
                        AccountOrDexId::Account(withdraw_to.clone()),
                        asset_id.clone(),
                        amount,
                    );
                } else if self.asset_is_registered(withdraw_from.clone(), asset_id.clone()) {
                    near_sdk::env::log_str(&format!(
                        "Refunding to {withdraw_from} (rescue) because withdrawal to {withdraw_to} failed: {error:?}"
                    ));
                    self.internal_increase_assets(
                        &mut events,
                        withdraw_from,
                        asset_id.clone(),
                        amount,
                    );
                } else {
                    panic!(
                        "No registered balance found for withdrawal from {withdraw_to} (recipient) or {withdraw_from} (rescue). Assets are lost."
                    );
                }
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
        };
        events.emit();
        withdrawn
    }
}
