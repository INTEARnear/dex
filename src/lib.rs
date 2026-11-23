#![deny(clippy::arithmetic_side_effects)]
#![allow(clippy::too_many_arguments)]

pub mod asset_deposit;
pub mod call_events;
pub mod dex_code;
pub mod host_functions;
pub mod internal_asset_operations;
pub mod internal_operations;

pub mod oracle;
pub mod rescue;
pub mod storage_management;

use std::collections::HashMap;

use crate::{
    call_events::CallEvents,
    dex_code::TranslatedDexCodes,
    internal_operations::TradeAccount,
    oracle::{MAX_ORACLE_REQUEST_BYTES, MAX_ORACLE_RESPONSE_BYTES},
    storage_management::StorageBalances,
};
use intear_dex_common::{
    AssetId, Caller, DexId, FULL_FEE_DISCOUNT, FeeDiscountFraction, SwapRequestAmount, TokenAmount,
    expect,
};
use intear_dex_types::{
    AccountOrDexId, CAN_PAUSE, DexCallAfterDeploy, DexEventData, DirectWithdrawAmount, FeeDiscount,
    FeeDiscountSignaturePayload, IntearDexEvent, MAX_TRADER_FEE_DISCOUNT_BLOCKS, Operation,
    OperationsFeeDiscount, QuoteImprovementFee, ReferralFees, SignedFeeDiscount,
    SwapOperationAmount, TraderFeeDiscount, UnsignedOracleResponse,
};
use near_sdk::{
    AccountId, BorshStorageKey, CryptoHash, CurveType, Gas, GasWeight, IntoStorageKey, NearToken,
    PanicOnDefault, Promise, PromiseOrValue, PublicKey, PublicKeyHandle,
    json_types::Base64VecU8,
    near,
    store::{IterableMap, LazyOption, LookupMap},
};

pub type AssetBalances = IterableMap<AssetId, TokenAmount>;

#[cfg(target_arch = "wasm32")]
#[global_allocator]
static ALLOCATOR: intear_dex_common::BumpAllocator = intear_dex_common::BumpAllocator;

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct DexEngine {
    /// Assets that are custodied by the dex engine contract
    /// for the dexes that run inside it. Other dexes or users
    /// can't access other dexes' balances.
    dex_balances: LookupMap<DexId, AssetBalances>,
    /// Persistent storage for each dex, similar to contract
    /// storage of traditional smart contract dexes. It's
    /// public, but currently there's no way to access other
    /// dexes' storage from dex runtime.
    dex_storage: DexStorage,
    /// Wasm code for each dex.
    dex_codes: LookupMap<DexId, Vec<u8>>,
    /// Storage balances for each dex, translated to storage
    /// of this smart contract. use dex_* methods to interact
    /// with it, such as dex_storage_deposit.
    pub dex_storage_balances: StorageBalances<DexId>,
    /// Balances for each user, custodied by the dex engine
    /// contract for faster access. This reduces the need for
    /// ft_transfer_call, which takes time.
    user_balances: LookupMap<AccountId, AssetBalances>,
    /// Storage balances for each user, translated to storage
    /// of this smart contract. use storage management methods
    /// to interact with it, such as storage_deposit.
    pub user_storage_balances: StorageBalances<AccountId>,
    /// Balances for all the funds custodied by the dex engine
    /// contract. This means if the dex engine contract has
    /// less than this amount, it's either bug of the asset
    /// implementation, or funds have been drained from the
    /// dex engine contract. And if the balance is greater
    /// than this stored amount, it can be freely taken out
    /// without causing any issues.
    total_in_custody: IterableMap<AssetId, TokenAmount>,
    paused: bool,
    /// The only account that can deploy dex code. Dex code isn't
    /// validated when it's loaded, so it must come from a trusted source.
    trusted_code_deployer: AccountId,
    /// New engine code that's uploaded in chunks, since it may not
    /// fit in one transaction.
    uploaded_code: LazyOption<Vec<u8>>,
    /// The account that takes the fees of each oracle
    oracle_fee_receivers: LookupMap<PublicKeyHandle, AccountId>,
    /// What each oracle takes for a request, in each asset that it takes
    /// fees in
    oracle_fees: LookupMap<PublicKeyHandle, HashMap<AssetId, TokenAmount>>,
    /// The key that signs the fee discounts that dexes read
    fee_discount_signer: Option<PublicKey>,
    /// Wasm code with only the swap method
    dex_swap_codes: LookupMap<DexId, Vec<u8>>,
    /// The translation of each dex's code
    #[borsh(skip)]
    translated_dex_codes: TranslatedDexCodes,
}

#[derive(BorshStorageKey)]
#[near(serializers=[borsh])]
enum StorageKey {
    #[allow(dead_code)]
    UnusedFlatDexBalances,
    DexStorage,
    DexCodes,
    DexStorageBalances,
    #[allow(dead_code)]
    UnusedFlatUserBalances,
    UserStorageBalances,
    ContractTrackedBalance,
    UserBalances,
    UserBalancesOf {
        account_id: AccountId,
    },
    DexBalances,
    DexBalancesOf {
        dex_id: DexId,
    },
    UploadedCode,
    TranslatedDexCodes,
    OracleFeeReceivers,
    OracleFees,
    DexSwapCodes,
    TranslatedDexSwapCodes,
}

enum CallType<'a> {
    Trade {
        dex_storage: &'a DexStorage,
        trader: Caller,
        pending_storage_updates: PendingStorageUpdates,
    },
    View {
        dex_storage: &'a DexStorage,
    },
    Call {
        dex_storage_mut: &'a mut DexStorage,
        caller: Caller,
    },
}

type DexStorage = LookupMap<(DexId, Vec<u8>), Vec<u8>>;

type PendingStorageUpdates = HashMap<Vec<u8>, Option<Vec<u8>>>;

/// `storage_num_extra_bytes_record` in
/// https://github.com/near/nearcore/blob/2.14.0/core/parameters/res/runtime_configs/parameters.yaml#L37
const STORAGE_BYTES_PER_RECORD: u64 = 40;

impl CallType<'_> {
    pub fn dex_storage_get(&self, dex_id: DexId, key: Vec<u8>) -> Option<&Vec<u8>> {
        match self {
            CallType::Trade {
                dex_storage,
                trader: _,
                pending_storage_updates,
            } => {
                if let Some(value) = pending_storage_updates.get(&key) {
                    value.as_ref()
                } else if let Some(value) = dex_storage.get(&(dex_id, key)) {
                    Some(value)
                } else {
                    None
                }
            }
            CallType::View { dex_storage } => dex_storage.get(&(dex_id, key)),
            CallType::Call {
                dex_storage_mut, ..
            } => dex_storage_mut.get(&(dex_id, key)),
        }
    }

    pub fn dex_storage_contains_key(&self, dex_id: DexId, key: Vec<u8>) -> bool {
        match self {
            CallType::Trade {
                dex_storage,
                pending_storage_updates,
                ..
            } => {
                if let Some(value) = pending_storage_updates.get(&key) {
                    value.is_some()
                } else {
                    dex_storage.contains_key(&(dex_id, key))
                }
            }
            CallType::View { dex_storage } => dex_storage.contains_key(&(dex_id, key)),
            CallType::Call {
                dex_storage_mut, ..
            } => dex_storage_mut.contains_key(&(dex_id, key)),
        }
    }

    pub fn dex_storage_insert(
        &mut self,
        dex_id: DexId,
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, wasmi::Error> {
        match self {
            CallType::Trade {
                dex_storage,
                pending_storage_updates,
                ..
            } => {
                if let Some(prev_value) = pending_storage_updates.get(&key).cloned() {
                    pending_storage_updates.insert(key, Some(value));
                    Ok(prev_value)
                } else if let Some(prev_value) = dex_storage.get(&(dex_id, key.clone())) {
                    pending_storage_updates.insert(key, Some(value));
                    Ok(Some(prev_value.clone()))
                } else {
                    pending_storage_updates.insert(key, Some(value));
                    Ok(None)
                }
            }
            CallType::View { .. } => Err(wasmi::Error::new("Cannot modify data in view-only mode")),
            CallType::Call {
                dex_storage_mut, ..
            } => {
                let key = (dex_id, key);
                if dex_storage_mut.get(&key) == Some(&value) {
                    return Ok(Some(value));
                }
                Ok(dex_storage_mut.insert(key, value))
            }
        }
    }

    pub fn dex_storage_remove(
        &mut self,
        dex_id: DexId,
        key: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, wasmi::Error> {
        match self {
            CallType::Trade {
                dex_storage,
                pending_storage_updates,
                ..
            } => {
                if let Some(prev_value) = pending_storage_updates.insert(key.clone(), None) {
                    Ok(prev_value)
                } else {
                    Ok(dex_storage.get(&(dex_id, key.clone())).cloned())
                }
            }
            CallType::View { .. } => Err(wasmi::Error::new("Cannot modify data in view-only mode")),
            CallType::Call {
                dex_storage_mut, ..
            } => Ok(dex_storage_mut.remove(&(dex_id, key))),
        }
    }

    /// How much the contract's storage usage would change if a simulated trade's
    /// storage updates were written, so that the dex sees the same storage usage
    /// as in a real trade
    pub fn pending_storage_usage(&self, dex_id: &DexId) -> Result<i64, wasmi::Error> {
        let CallType::Trade {
            dex_storage,
            pending_storage_updates,
            ..
        } = self
        else {
            return Ok(0);
        };
        let prefix_len = StorageKey::DexStorage.into_storage_key().len();
        let record_size = |key: &Vec<u8>, value: Option<&Vec<u8>>| -> Result<i64, wasmi::Error> {
            let Some(value) = value else {
                return Ok(0);
            };
            let key_len = near_sdk::borsh::object_length(&(dex_id, key))
                .map_err(|err| wasmi::Error::new(format!("Failed to measure key: {err}")))?;
            let value_len = near_sdk::borsh::object_length(value)
                .map_err(|err| wasmi::Error::new(format!("Failed to measure value: {err}")))?;
            prefix_len
                .checked_add(key_len)
                .and_then(|size| size.checked_add(value_len))
                .and_then(|size| i64::try_from(size).ok())
                .and_then(|size| size.checked_add(STORAGE_BYTES_PER_RECORD as i64))
                .ok_or_else(|| wasmi::Error::new("Storage record size overflow"))
        };
        pending_storage_updates
            .iter()
            .try_fold(0i64, |total, (key, new_value)| {
                let old_value = dex_storage.get(&(dex_id.clone(), key.clone()));
                record_size(key, new_value.as_ref())?
                    .checked_sub(record_size(key, old_value)?)
                    .and_then(|change| total.checked_add(change))
                    .ok_or_else(|| wasmi::Error::new("Storage usage overflow"))
            })
    }

    pub fn dex_storage_flush(&mut self) {
        match self {
            CallType::Trade { .. } => (),
            CallType::View { .. } => (),
            CallType::Call {
                dex_storage_mut, ..
            } => dex_storage_mut.flush(),
        }
    }
}

struct OracleResponse {
    oracle_key: PublicKeyHandle,
    dex_id: DexId,
    request: Vec<u8>,
    response: Vec<u8>,
}

pub struct RunnerData<'a> {
    request: Vec<u8>,
    response: Option<Vec<u8>>,
    registers: HashMap<u64, Vec<u8>>,
    call_type: CallType<'a>,
    dex_id: DexId,
    dex_storage_balances: &'a StorageBalances<DexId>,
    dex_storage_usage_before_transaction: u64,
    referrer: Option<AccountId>,
    dex_events: Vec<DexEventData>,
    oracle_response: Option<&'a OracleResponse>,
    fee_discount: Option<FeeDiscountFraction>,
}

#[near]
impl DexEngine {
    #[init]
    pub fn new(trusted_code_deployer: AccountId) -> Self {
        Self {
            dex_balances: LookupMap::new(StorageKey::DexBalances),
            dex_storage: LookupMap::new(StorageKey::DexStorage),
            dex_codes: LookupMap::new(StorageKey::DexCodes),
            dex_storage_balances: StorageBalances::new(StorageKey::DexStorageBalances),
            user_balances: LookupMap::new(StorageKey::UserBalances),
            user_storage_balances: StorageBalances::new(StorageKey::UserStorageBalances),
            total_in_custody: IterableMap::new(StorageKey::ContractTrackedBalance),
            paused: false,
            trusted_code_deployer,
            uploaded_code: LazyOption::new(StorageKey::UploadedCode, None),
            oracle_fee_receivers: LookupMap::new(StorageKey::OracleFeeReceivers),
            oracle_fees: LookupMap::new(StorageKey::OracleFees),
            fee_discount_signer: None,
            dex_swap_codes: LookupMap::new(StorageKey::DexSwapCodes),
            translated_dex_codes: TranslatedDexCodes::default(),
        }
    }

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
            total_in_custody: IterableMap<AssetId, TokenAmount>,
            paused: bool,
            trusted_code_deployer: AccountId,
            uploaded_code: LazyOption<Vec<u8>>,
            oracle_fee_receivers: LookupMap<PublicKeyHandle, AccountId>,
            oracle_fees: LookupMap<PublicKeyHandle, HashMap<AssetId, TokenAmount>>,
            fee_discount_signer: Option<PublicKey>,
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
            oracle_fee_receivers: old_state.oracle_fee_receivers,
            oracle_fees: old_state.oracle_fees,
            fee_discount_signer: old_state.fee_discount_signer,
            dex_swap_codes: LookupMap::new(StorageKey::DexSwapCodes),
            translated_dex_codes: TranslatedDexCodes::default(),
        }
    }

    /// Upload a chunk of new engine code at `offset`, replacing everything
    /// after it. `code_hash_so_far` is the hash of the code up to the chunk's end.
    #[private]
    pub fn store_code_chunk(
        &mut self,
        #[serializer(borsh)] offset: u32,
        #[serializer(borsh)] chunk: Vec<u8>,
        #[serializer(borsh)] code_hash_so_far: CryptoHash,
    ) {
        let code = self.uploaded_code.get_mut().get_or_insert_with(Vec::new);
        expect!(
            offset as usize <= code.len(),
            "Only {} bytes of the code are stored, so a chunk can't start at byte {offset}",
            code.len()
        );
        code.truncate(offset as usize);
        code.extend(chunk);
        expect!(
            near_sdk::env::sha256_array(code) == code_hash_so_far,
            "The code stored so far doesn't have the hash it should have"
        );
    }

    /// Upload the last chunk and deploy the code. `then_call` is called with
    /// `then_call_args` in the same receipt, so if the new code doesn't compile
    /// or the call fails, the old code stays and `then_call` doesn't run.
    #[private]
    pub fn deploy_code_chunks(
        &mut self,
        #[serializer(borsh)] offset: u32,
        #[serializer(borsh)] last_chunk: Vec<u8>,
        #[serializer(borsh)] code_hash: CryptoHash,
        #[serializer(borsh)] then_call: String,
        #[serializer(borsh)] then_call_args: Vec<u8>,
    ) -> Promise {
        self.store_code_chunk(offset, last_chunk, code_hash);
        let code = self
            .uploaded_code
            .get_mut()
            .take()
            .expect("The code was just stored");
        Promise::new(near_sdk::env::current_account_id())
            .deploy_contract(code)
            .function_call_weight(
                then_call,
                then_call_args,
                NearToken::from_yoctonear(0),
                Gas::from_gas(0),
                GasWeight(1),
            )
    }

    /// Transfer the permission to deploy dex code to another account
    #[payable]
    pub fn set_trusted_code_deployer(&mut self, account_id: AccountId) {
        near_sdk::assert_one_yocto();
        expect!(
            near_sdk::env::predecessor_account_id() == self.trusted_code_deployer,
            "Only the trusted code deployer can transfer this permission"
        );
        self.trusted_code_deployer = account_id;
    }

    pub fn get_trusted_code_deployer(&self) -> AccountId {
        self.trusted_code_deployer.clone()
    }

    #[payable]
    pub fn pause(&mut self) {
        near_sdk::assert_one_yocto();
        expect!(
            CAN_PAUSE.contains(&near_sdk::env::predecessor_account_id().as_str()),
            "Only authorized accounts can pause the contract"
        );
        self.paused = true;
    }

    #[payable]
    pub fn unpause(&mut self) {
        near_sdk::assert_one_yocto();
        expect!(
            CAN_PAUSE.contains(&near_sdk::env::predecessor_account_id().as_str()),
            "Only authorized accounts can pause the contract"
        );
        self.paused = false;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Deploy or upgrade the code for a dex, once the translator translated it.
    #[payable]
    pub fn deploy_dex_code(
        &mut self,
        last_part_of_id: String,
        code_base64: Base64VecU8,
        call_after_deploy: Option<DexCallAfterDeploy>,
        swap_code_base64: Option<Base64VecU8>,
    ) -> Promise {
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
        let deployer = near_sdk::env::predecessor_account_id();
        expect!(
            deployer == self.trusted_code_deployer,
            "Only the trusted code deployer can deploy dex code"
        );
        let dex_id = DexId {
            deployer,
            id: last_part_of_id,
        };
        Self::translate_dex_code(
            dex_id,
            code_base64.0,
            swap_code_base64.map(|swap_code| swap_code.0),
            true,
            call_after_deploy,
        )
    }

    /// Swap one asset for another on a specific dex.
    /// Multi-step aggregator method coming soon.
    #[payable]
    pub fn swap_simple(
        &mut self,
        dex_id: DexId,
        message: Base64VecU8,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
        referrer: Option<AccountId>,
        referral_fees: Option<ReferralFees>,
        constraint: Option<TokenAmount>,
        quote_improvement_fee: Option<QuoteImprovementFee>,
        fee_discount: Option<SignedFeeDiscount>,
    ) -> (TokenAmount, TokenAmount) {
        let mut events = CallEvents::default();
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
        let trader_id = near_sdk::env::predecessor_account_id();
        let fee_discount = fee_discount.map(|fee_discount| {
            let swap_operation = Operation::SwapSimple {
                dex_id: dex_id.clone(),
                message: message.clone(),
                asset_in: asset_in.clone(),
                asset_out: asset_out.clone(),
                amount: SwapOperationAmount::Amount(amount),
                constraint,
                quote_improvement_fee: quote_improvement_fee.clone(),
            };
            self.verify_fee_discount(
                fee_discount,
                &[swap_operation],
                &Caller::Direct(trader_id.clone()),
            )
        });
        let amounts = self.internal_swap_simple(
            &mut events,
            dex_id,
            message,
            asset_in,
            asset_out,
            amount,
            TradeAccount::User(trader_id),
            referrer,
            referral_fees.unwrap_or_default(),
            constraint,
            quote_improvement_fee,
            None,
            fee_discount,
        );
        events.emit();
        amounts
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
        attached_assets: HashMap<AssetId, TokenAmount>,
        referrer: Option<AccountId>,
    ) -> Base64VecU8 {
        let mut events = CallEvents::default();
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
        let response = self.internal_dex_call(
            &mut events,
            dex_id,
            method,
            args,
            attached_assets,
            Caller::Direct(near_sdk::env::predecessor_account_id()),
            None,
            referrer,
            None,
            None,
        );
        events.emit();
        response
    }

    #[payable]
    pub fn transfer_asset(&mut self, to: AccountOrDexId, asset_id: AssetId, amount: TokenAmount) {
        let mut events = CallEvents::default();
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
        self.internal_transfer_asset(
            &mut events,
            AccountOrDexId::Account(near_sdk::env::predecessor_account_id()),
            to,
            asset_id,
            amount,
        );
        events.emit();
    }

    /// Register assets for an account or dex, reserving storage
    /// for the balance. Unregistering is not possible. No-op if
    /// already registered.
    #[payable]
    pub fn register_assets(&mut self, asset_ids: Vec<AssetId>, r#for: Option<AccountOrDexId>) {
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
        self.internal_register_assets(asset_ids, r#for, near_sdk::env::predecessor_account_id());
    }

    /// Withdraw assets from the dex engine contract's inner
    /// balance for the user. If `withdraw_to` is not provided,
    /// the assets will be withdrawn to the user's account.
    ///
    /// Returns `true` if the withdrawal was successful, `false`
    /// otherwise. If a withdrawal fails, the assets go to the
    /// inner balance of `withdraw_to` if it has the asset
    /// registered, or back to the user's if it doesn't.
    #[payable]
    pub fn withdraw(
        &mut self,
        asset_id: AssetId,
        amount: DirectWithdrawAmount,
        withdraw_to: Option<AccountId>,
    ) -> PromiseOrValue<bool> {
        let mut events = CallEvents::default();
        near_sdk::assert_one_yocto();
        self.assert_not_paused();
        let withdrawal = self.internal_withdraw(
            &mut events,
            asset_id,
            amount,
            withdraw_to,
            AccountOrDexId::Account(near_sdk::env::predecessor_account_id()),
        );
        events.emit();
        withdrawal
    }

    #[payable]
    pub fn execute_operations(
        &mut self,
        operations: Vec<Operation>,
        referrer: Option<AccountId>,
        referral_fees: Option<ReferralFees>,
        fee_discount: Option<SignedFeeDiscount>,
    ) {
        let mut events = CallEvents::default();
        expect!(
            !near_sdk::env::attached_deposit().is_zero(),
            "Deposit of at least one yoctoNEAR is required."
        );
        self.assert_not_paused();
        if near_sdk::env::attached_deposit() > NearToken::from_yoctonear(1) {
            let deposit = TokenAmount::new(near_sdk::env::attached_deposit().as_yoctonear());
            self.internal_increase_assets(
                &mut events,
                AccountOrDexId::Account(near_sdk::env::predecessor_account_id()),
                AssetId::Near,
                deposit,
            );
            self.total_in_custody
                .entry(AssetId::Near)
                .and_modify(|b| {
                    *b = b.checked_add(deposit).unwrap_or_else(|| {
                        panic!(
                            "Balance overflow for contract and asset Near: {} + {} > {}",
                            b,
                            deposit,
                            u128::MAX
                        )
                    });
                })
                .or_insert_with(|| {
                    panic!(
                        "Failed to deposit assets to contract tracked balance: asset not registered"
                    )
                });
        }
        let trader = Caller::Direct(near_sdk::env::predecessor_account_id());
        let fee_discount = fee_discount
            .map(|fee_discount| self.verify_fee_discount(fee_discount, &operations, &trader));
        if let Some(waiting_for_oracles) = self.internal_execute_operations(
            &mut events,
            operations,
            trader,
            None,
            referrer,
            referral_fees.unwrap_or_default(),
            None,
            fee_discount,
        ) {
            waiting_for_oracles.as_return().detach();
        }
        events.emit();
    }

    #[payable]
    #[private]
    pub fn set_fee_discount_signer(&mut self, fee_discount_signer: Option<PublicKey>) {
        near_sdk::assert_one_yocto();
        self.fee_discount_signer = fee_discount_signer;
    }

    pub fn get_fee_discount_signer(&self) -> Option<PublicKey> {
        self.fee_discount_signer.clone()
    }

    pub fn asset_balance_of(&self, of: AccountOrDexId, asset_id: AssetId) -> Option<TokenAmount> {
        self.asset_balances(&of)?.get(&asset_id).copied()
    }

    pub fn registered_assets_of(
        &self,
        account_id: Option<AccountId>,
        dex_id: Option<DexId>,
        from_index: u32,
        limit: u32,
    ) -> Vec<(AssetId, TokenAmount)> {
        let of = match (account_id, dex_id) {
            (Some(account_id), None) => AccountOrDexId::Account(account_id),
            (None, Some(dex_id)) => AccountOrDexId::Dex(dex_id),
            _ => panic!("account_id and dex_id are mutually exclusive"),
        };
        let Some(asset_balances) = self.asset_balances(&of) else {
            return Vec::new();
        };
        asset_balances
            .iter()
            .skip(from_index as usize)
            .take(limit as usize)
            .map(|(asset_id, balance)| (asset_id.clone(), *balance))
            .collect()
    }

    pub fn total_in_custody(&self, asset_id: AssetId) -> Option<TokenAmount> {
        self.total_in_custody.get(&asset_id).copied()
    }

    pub fn dex_code(&self, dex_id: DexId) -> Base64VecU8 {
        Base64VecU8(
            self.dex_codes
                .get(&dex_id)
                .expect("Dex code not found")
                .clone(),
        )
    }

    pub fn dex_swap_code(&self, dex_id: DexId) -> Option<Base64VecU8> {
        self.dex_swap_codes.get(&dex_id).cloned().map(Base64VecU8)
    }

    // View method, but needs &mut for compatibility ergonomics with RunnerData
    pub fn dex_view(&self, dex_id: DexId, method: String, args: Base64VecU8) -> Base64VecU8 {
        self.internal_dex_view(dex_id, method, args)
    }

    /// Simulate a swap without executing it. Returns the expected
    /// (amount_in, amount_out) that would result from the swap.
    /// This is a view method and does not modify any state.
    pub fn simulate_swap_simple(
        &self,
        dex_id: DexId,
        message: Base64VecU8,
        trader: AccountId,
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapRequestAmount,
        referrer: Option<AccountId>,
        referral_fees: Option<ReferralFees>,
        quote_improvement_fee: Option<QuoteImprovementFee>,
        oracle_response: Option<UnsignedOracleResponse>,
        fee_discount: Option<FeeDiscountFraction>,
    ) -> (TokenAmount, TokenAmount) {
        self.assert_not_paused();
        let oracle_response = oracle_response.map(
            |UnsignedOracleResponse {
                 oracle_key,
                 request,
                 response,
             }| {
                expect!(
                    request.0.len() <= MAX_ORACLE_REQUEST_BYTES,
                    "Request is more than {MAX_ORACLE_REQUEST_BYTES} bytes"
                );
                expect!(
                    response.0.len() <= MAX_ORACLE_RESPONSE_BYTES,
                    "Response is more than {MAX_ORACLE_RESPONSE_BYTES} bytes"
                );
                OracleResponse {
                    oracle_key,
                    dex_id: dex_id.clone(),
                    request: request.0,
                    response: response.0,
                }
            },
        );
        let module = self.translated_dex_codes.load_for_swaps(&dex_id);
        self.internal_simulate_swap_simple(
            dex_id,
            &module,
            message,
            Caller::Direct(trader),
            asset_in,
            asset_out,
            amount,
            referrer,
            referral_fees.unwrap_or_default(),
            quote_improvement_fee,
            oracle_response.as_ref(),
            fee_discount,
        )
        .map(|swap| {
            (
                swap.total_amount_in_with_improvement_fee(),
                swap.amount_out_without_improvement_fee(),
            )
        })
        .unwrap_or_else(|err| panic!("{err}"))
    }

    pub fn are_assets_registered(&self, asset_ids: Vec<AssetId>, r#for: AccountOrDexId) -> bool {
        let Some(asset_balances) = self.asset_balances(&r#for) else {
            return asset_ids.is_empty();
        };
        asset_ids
            .iter()
            .all(|asset_id| asset_balances.contains_key(asset_id))
    }
}

impl DexEngine {
    pub fn assert_not_paused(&self) {
        expect!(!self.paused, "Contract is paused");
    }

    fn verify_fee_discount(
        &self,
        signed_fee_discount: SignedFeeDiscount,
        operations: &[Operation],
        caller: &Caller,
    ) -> FeeDiscountFraction {
        let SignedFeeDiscount {
            discount,
            signature,
        } = signed_fee_discount;
        let Some(signer_public_key) = &self.fee_discount_signer else {
            near_sdk::env::panic_str("Fee discounts are disabled");
        };
        let signature_payload = FeeDiscountSignaturePayload {
            chain_id: near_sdk::env::chain_id(),
            engine_contract_id: near_sdk::env::current_account_id(),
            discount,
        };

        let public_key_bytes = &signer_public_key.as_bytes()[1..];
        let hash =
            near_sdk::env::sha256_array(near_sdk::borsh::to_vec(&signature_payload).unwrap());
        let is_verified = match signer_public_key.curve_type() {
            CurveType::ED25519 => near_sdk::env::ed25519_verify(
                signature
                    .0
                    .as_slice()
                    .try_into()
                    .expect("Invalid signature"),
                hash,
                public_key_bytes.try_into().unwrap(),
            ),
            CurveType::SECP256K1 => {
                let Some((recovery_id, signature)) = signature.0.split_last() else {
                    near_sdk::env::panic_str("Invalid signature");
                };
                near_sdk::env::ecrecover(&hash, signature, *recovery_id, true)
                    .is_some_and(|recovered| recovered.as_slice() == public_key_bytes)
            }
            CurveType::MLDSA65 => near_sdk::env::ml_dsa_verify(
                signature
                    .0
                    .as_slice()
                    .try_into()
                    .expect("Invalid signature"),
                hash,
                public_key_bytes.try_into().unwrap(),
            ),
        };
        expect!(is_verified, "Fee discount signature verification failed");
        let block_height = near_sdk::env::block_height();
        let fee_discount = match signature_payload.discount {
            FeeDiscount::ForOperations(OperationsFeeDiscount {
                operations_hash,
                trader_id,
                fee_discount,
                expiration_block_height,
            }) => {
                expect!(
                    CryptoHash::from(operations_hash)
                        == near_sdk::env::sha256_array(
                            near_sdk::borsh::to_vec(operations).unwrap()
                        ),
                    "Fee discount is for other operations"
                );
                if let Some(trader_id) = trader_id {
                    expect!(
                        *caller.alleged_account_id() == trader_id,
                        "Fee discount is for another trader"
                    );
                }
                expect!(
                    block_height <= expiration_block_height,
                    "Fee discount expired"
                );
                fee_discount
            }
            FeeDiscount::ForTrader(TraderFeeDiscount {
                trader_id,
                fee_discount,
                expiration_block_height,
            }) => {
                let Caller::Direct(caller_id) = caller else {
                    near_sdk::env::panic_str(
                        "Trader fee discounts can only be used in direct calls, not in transfer calls",
                    );
                };
                expect!(
                    *caller_id == trader_id,
                    "Fee discount is for another trader"
                );
                expect!(
                    block_height <= expiration_block_height,
                    "Fee discount expired"
                );
                expect!(
                    expiration_block_height
                        <= block_height
                            .checked_add(MAX_TRADER_FEE_DISCOUNT_BLOCKS)
                            .expect("Overflow"),
                    "Trader fee discount expires more than {MAX_TRADER_FEE_DISCOUNT_BLOCKS} blocks from now"
                );
                fee_discount
            }
        };
        expect!(
            fee_discount <= FULL_FEE_DISCOUNT,
            "Fee discount can't exceed 100%"
        );
        fee_discount
    }
}
