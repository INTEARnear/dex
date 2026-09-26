#![deny(clippy::arithmetic_side_effects)]

use std::collections::HashMap;

use crypto_bigint::{ConstChoice, I256, U256};
use intear_dex_types::{
    AssetId, AssetWithdrawRequest, AssetWithdrawalType, Dex, DexCallResponse, SwapRequest,
    SwapResponse, expect,
};
use near_sdk::{
    AccountId, BlockHeight, BorshStorageKey, CurveType, NearToken, PublicKey, assert_one_yocto,
    json_types::{Base64VecU8, U64, U128},
    near,
    store::{LookupMap, LookupSet, TreeMap},
};

#[near(serializers=[borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Balances,
    AuthorizedKeys,
    StorageBalances,
    UsedExpirableNoncesBlockHeight,
    UsedExpirableNoncesTimestampMillis,
    UsedExpirableNoncesBlockHeightAccount { account_id: AccountId },
    UsedExpirableNoncesTimestampMillisAccount { account_id: AccountId },
    UsedNonces,
}

#[derive(Default)]
#[near(serializers=[borsh, json])]
pub struct StorageBalance {
    total: NearToken,
    used: NearToken,
}

impl StorageBalance {
    fn validate(&self) {
        expect!(self.total >= self.used, "Not enough storage deposit");
    }
}

#[near(contract_state)]
pub struct OtcDex {
    balances: LookupMap<(AccountId, AssetId), U128>,
    authorized_keys: LookupMap<AccountId, PublicKey>,
    storage_balances: LookupMap<AccountId, StorageBalance>,
    used_expirable_nonces_block_height: LookupMap<AccountId, TreeMap<(BlockHeight, Nonce), ()>>,
    used_expirable_nonces_timestamp_millis: LookupMap<AccountId, TreeMap<(U64, Nonce), ()>>,
    used_nonces: LookupSet<(AccountId, Nonce)>,
}

pub type Nonce = U128;

#[near(serializers=[json, borsh])]
enum AuthorizationMethod {
    Signature(Base64VecU8),
    Predecessor,
}

#[near(serializers=[json, borsh])]
pub struct AuthorizedTradeIntent {
    trade_intent: TradeIntent,
    authorization_method: AuthorizationMethod,
}

impl AuthorizedTradeIntent {
    fn validate(&self, dex: &OtcDex, all_user_ids: &[&AccountId]) {
        if let Some(expiry) = &self.trade_intent.validity.expiry {
            match expiry {
                ExpiryCondition::BlockHeight(block_height) => {
                    expect!(
                        near_sdk::env::block_height() <= *block_height,
                        "Intent expired: Block height expired"
                    );
                }
                ExpiryCondition::Timestamp { milliseconds } => {
                    expect!(
                        near_sdk::env::block_timestamp_ms() < milliseconds.0,
                        "Intent expired: Timestamp expired"
                    );
                }
            }
        }
        // nonce is checked after clearing up used nonces
        if let Some(only_for_whitelisted_parties) =
            &self.trade_intent.validity.only_for_whitelisted_parties
        {
            for other_user_id in all_user_ids {
                if **other_user_id != self.trade_intent.user_id {
                    expect!(
                        only_for_whitelisted_parties.contains(*other_user_id),
                        "Intent not authorized: User {other_user_id} not whitelisted"
                    );
                }
            }
        }
        match &self.authorization_method {
            AuthorizationMethod::Predecessor => {
                expect!(
                    near_sdk::env::predecessor_account_id() == self.trade_intent.user_id,
                    "Intent not authorized: AuthorizationMethod::Predecessor cannot be used if the predecessor is not the user"
                );
            }
            AuthorizationMethod::Signature(signature) => {
                if let Some(expected_public_key) =
                    dex.get_authorized_key(&self.trade_intent.user_id)
                {
                    let data = near_sdk::borsh::to_vec(&self.trade_intent).unwrap();
                    let hash = near_sdk::env::sha256_array(&data);
                    let is_verified = match expected_public_key.curve_type() {
                        CurveType::ED25519 => near_sdk::env::ed25519_verify(
                            signature
                                .0
                                .as_slice()
                                .try_into()
                                .expect("Invalid signature length"),
                            hash,
                            &expected_public_key.as_bytes()[1..].try_into().unwrap(),
                        ),
                        CurveType::SECP256K1 => {
                            let actual_public_key = near_sdk::env::ecrecover(
                                &hash,
                                &signature.0[..signature.0.len().saturating_sub(1)],
                                *signature.0.last().expect("Invalid signature"),
                                true,
                            )
                            .expect("Invalid signature");
                            actual_public_key == expected_public_key.as_bytes()[1..]
                        }
                        CurveType::MLDSA65 => {
                            panic!("Intent not authorized: MLDSA65 keys are not supported")
                        }
                    };
                    expect!(
                        is_verified,
                        "Intent not authorized: Signature verification failed"
                    );
                } else {
                    panic!(
                        "Intent not authorized: AuthorizationMethod::Signature cannot be used if there is no authorized key"
                    );
                }
            }
        }
    }
}

#[near(serializers=[json, borsh])]
#[derive(Debug)]
pub struct TradeIntent {
    user_id: AccountId,
    asset_in: AssetId,
    asset_out: AssetId,
    amount_in: U128,
    amount_out: U128,
    #[serde(default, skip_serializing_if = "is_default")]
    validity: Validity,
}

#[derive(Default, PartialEq, Debug)]
#[near(serializers=[json, borsh])]
pub struct Validity {
    expiry: Option<ExpiryCondition>,
    nonce: Option<Nonce>,
    only_for_whitelisted_parties: Option<Vec<AccountId>>,
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

#[derive(PartialEq, Clone, Copy, Debug)]
#[near(serializers=[borsh, json])]
pub enum ExpiryCondition {
    BlockHeight(BlockHeight),
    Timestamp { milliseconds: U64 },
}

#[near(event_json(standard = "simpleswap"))]
enum OtcTradeEvent {
    #[event_version("1.0.0")]
    AuthorizedKeyChanged {
        account_id: AccountId,
        key: PublicKey,
    },
    #[event_version("1.0.0")]
    Trade {
        authorized_trade_intents: Vec<AuthorizedTradeIntent>,
    },
}

#[near]
impl Dex for OtcDex {
    #[result_serializer(borsh)]
    fn swap(
        &mut self,
        #[allow(unused_variables)]
        #[serializer(borsh)]
        request: SwapRequest,
    ) -> SwapResponse {
        panic!("Method `swap` cannot be used for OtcDex. Use dex_call with `match` method instead.")
    }
}

impl Default for OtcDex {
    fn default() -> Self {
        Self {
            balances: LookupMap::new(StorageKey::Balances),
            authorized_keys: LookupMap::new(StorageKey::AuthorizedKeys),
            storage_balances: LookupMap::new(StorageKey::StorageBalances),
            used_expirable_nonces_block_height: LookupMap::new(
                StorageKey::UsedExpirableNoncesBlockHeight,
            ),
            used_expirable_nonces_timestamp_millis: LookupMap::new(
                StorageKey::UsedExpirableNoncesTimestampMillis,
            ),
            used_nonces: LookupSet::new(StorageKey::UsedNonces),
        }
    }
}

#[near]
impl OtcDex {
    #[payable]
    #[result_serializer(borsh)]
    pub fn r#match(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        #[near(serializers=[borsh])]
        enum OutputDestination {
            InternalOtcBalance,
            IntearDexBalance,
            WithdrawToUser,
        }
        #[near(serializers=[borsh])]
        struct MatchArgs {
            authorized_trade_intents: Vec<AuthorizedTradeIntent>,
            output_destination: OutputDestination,
        }
        let Ok(MatchArgs {
            authorized_trade_intents,
            output_destination,
        }) = near_sdk::borsh::from_slice(&args)
        else {
            near_sdk::env::panic_str("Invalid args");
        };

        let mut all_required_assets_were_attached = false;
        if !attached_assets.is_empty() {
            let mut required_assets = HashMap::<AssetId, U128>::new();
            for authorized_trade_intent in authorized_trade_intents.iter() {
                if authorized_trade_intent.trade_intent.user_id
                    == near_sdk::env::predecessor_account_id()
                {
                    required_assets
                        .entry(authorized_trade_intent.trade_intent.asset_in.clone())
                        .and_modify(|b| {
                            b.0 =
                                b.0.checked_add(authorized_trade_intent.trade_intent.amount_in.0)
                                    .expect("Required amount overflow")
                        })
                        .or_insert(authorized_trade_intent.trade_intent.amount_in);
                }
            }
            expect!(
                required_assets == attached_assets,
                "Invalid attached assets: {required_assets:?} != {attached_assets:?}"
            );
            all_required_assets_were_attached = true;
        }
        expect!(
            all_required_assets_were_attached || !near_sdk::env::attached_deposit().is_zero(),
            "You must attach all required assets if dex call is not authorized",
        );

        let mut assets_net_change = HashMap::<AssetId, I256>::new();
        for AuthorizedTradeIntent { trade_intent, .. } in authorized_trade_intents.iter() {
            // in
            let asset_in = trade_intent.asset_in.clone();
            let amount_in =
                I256::new_from_abs_sign(U256::from(trade_intent.amount_in.0), ConstChoice::TRUE)
                    .unwrap();
            assets_net_change
                .entry(asset_in)
                .and_modify(|b| *b = b.checked_add(&amount_in).expect("Amount overflow"))
                .or_insert(amount_in);

            // out
            let asset_out = trade_intent.asset_out.clone();
            let amount_out =
                I256::new_from_abs_sign(U256::from(trade_intent.amount_out.0), ConstChoice::FALSE)
                    .unwrap();
            assets_net_change
                .entry(asset_out)
                .and_modify(|b| *b = b.checked_add(&amount_out).expect("Amount overflow"))
                .or_insert(amount_out);
        }

        for (asset_id, net_change) in assets_net_change.iter() {
            if *net_change != I256::ZERO {
                panic!(
                    "Asset {asset_id} net sum is not zero: calculated {net_change} as total sum of all trade intents",
                );
            }
        }

        let mut asset_withdraw_requests = Vec::new();
        for authorized_trade_intent in authorized_trade_intents.iter() {
            // validate trade intent
            authorized_trade_intent.validate(
                self,
                &authorized_trade_intents
                    .iter()
                    .map(|i| &i.trade_intent.user_id)
                    .collect::<Vec<&AccountId>>(),
            );

            // update balances
            if authorized_trade_intent.trade_intent.user_id
                == near_sdk::env::predecessor_account_id()
                && all_required_assets_were_attached
            {
                // already accepted attached_assets
            } else {
                self.balances
                    .entry((
                        authorized_trade_intent.trade_intent.user_id.clone(),
                        authorized_trade_intent.trade_intent.asset_in.clone(),
                    ))
                    .and_modify(|b| {
                        b.0 =
                            b.0.checked_sub(authorized_trade_intent.trade_intent.amount_in.0)
                                .expect("Balance underflow")
                    })
                    .or_insert_with(|| {
                        panic!(
                            "{} has no registered balance for asset {}",
                            authorized_trade_intent.trade_intent.user_id,
                            authorized_trade_intent.trade_intent.asset_in,
                        )
                    });
            }

            if authorized_trade_intent.trade_intent.user_id
                == near_sdk::env::predecessor_account_id()
                && matches!(
                    output_destination,
                    OutputDestination::IntearDexBalance | OutputDestination::WithdrawToUser
                )
            {
                asset_withdraw_requests.push(AssetWithdrawRequest {
                    asset_id: authorized_trade_intent.trade_intent.asset_out.clone(),
                    amount: authorized_trade_intent.trade_intent.amount_out,
                    withdrawal_type: match output_destination {
                        OutputDestination::InternalOtcBalance => unreachable!(),
                        OutputDestination::IntearDexBalance => {
                            AssetWithdrawalType::ToInternalUserBalance(
                                authorized_trade_intent.trade_intent.user_id.clone(),
                            )
                        }
                        OutputDestination::WithdrawToUser => {
                            AssetWithdrawalType::WithdrawUnderlyingAsset(
                                authorized_trade_intent.trade_intent.user_id.clone(),
                            )
                        }
                    },
                });
            } else {
                let storage_usage_before = near_sdk::env::storage_usage();
                self.balances
                    .entry((
                        authorized_trade_intent.trade_intent.user_id.clone(),
                        authorized_trade_intent.trade_intent.asset_out.clone(),
                    ))
                    .and_modify(|b| {
                        b.0 =
                            b.0.checked_add(authorized_trade_intent.trade_intent.amount_out.0)
                                .expect("Balance overflow")
                    })
                    .or_insert(authorized_trade_intent.trade_intent.amount_out);
                self.balances.flush();
                let storage_usage_after = near_sdk::env::storage_usage();
                self.charge_storage_deposit(
                    storage_usage_before,
                    storage_usage_after,
                    authorized_trade_intent.trade_intent.user_id.clone(),
                );
            }

            // remove up to 10 used nonces that are no longer valid
            const NONCES_TO_REMOVE_AT_ONCE: usize = 10;
            let storage_usage_before = near_sdk::env::storage_usage();
            if let Some(map) = self
                .used_expirable_nonces_block_height
                .get_mut(&authorized_trade_intent.trade_intent.user_id)
            {
                for (block_height, nonce) in map
                    .keys()
                    .take(NONCES_TO_REMOVE_AT_ONCE)
                    .copied()
                    .collect::<Vec<_>>()
                {
                    if block_height < near_sdk::env::block_height() {
                        map.remove(&(block_height, nonce));
                        self.used_nonces
                            .remove(&(authorized_trade_intent.trade_intent.user_id.clone(), nonce));
                    }
                }
            }
            if let Some(map) = self
                .used_expirable_nonces_timestamp_millis
                .get_mut(&authorized_trade_intent.trade_intent.user_id)
            {
                for (timestamp_millis, nonce) in map
                    .keys()
                    .take(NONCES_TO_REMOVE_AT_ONCE)
                    .copied()
                    .collect::<Vec<_>>()
                {
                    if timestamp_millis.0 < near_sdk::env::block_timestamp_ms() {
                        map.remove(&(timestamp_millis, nonce));
                        self.used_nonces
                            .remove(&(authorized_trade_intent.trade_intent.user_id.clone(), nonce));
                    }
                }
            }

            // mark nonce as used
            if let Some(nonce) = &authorized_trade_intent.trade_intent.validity.nonce {
                expect!(
                    !self.is_nonce_used(
                        *nonce,
                        authorized_trade_intent.trade_intent.user_id.clone()
                    ),
                    "Intent not authorized: Nonce already used"
                );
            }
            if let Some(nonce) = &authorized_trade_intent.trade_intent.validity.nonce {
                self.used_nonces
                    .insert((authorized_trade_intent.trade_intent.user_id.clone(), *nonce));
                if let Some(expiry_condition) = authorized_trade_intent.trade_intent.validity.expiry
                {
                    match expiry_condition {
                        ExpiryCondition::BlockHeight(block_height) => {
                            self.used_expirable_nonces_block_height
                                .entry(authorized_trade_intent.trade_intent.user_id.clone())
                                .or_insert_with(|| {
                                    TreeMap::new(
                                        StorageKey::UsedExpirableNoncesBlockHeightAccount {
                                            account_id: authorized_trade_intent
                                                .trade_intent
                                                .user_id
                                                .clone(),
                                        },
                                    )
                                })
                                .insert((block_height, *nonce), ());
                        }
                        ExpiryCondition::Timestamp { milliseconds } => {
                            self.used_expirable_nonces_timestamp_millis
                                .entry(authorized_trade_intent.trade_intent.user_id.clone())
                                .or_insert_with(|| {
                                    TreeMap::new(
                                        StorageKey::UsedExpirableNoncesTimestampMillisAccount {
                                            account_id: authorized_trade_intent
                                                .trade_intent
                                                .user_id
                                                .clone(),
                                        },
                                    )
                                })
                                .insert((milliseconds, *nonce), ());
                        }
                    }
                }
            }

            // self.used_nonces is not flushed because it
            // is a LookupSet which writes immediately
            self.used_expirable_nonces_block_height.flush();
            self.used_expirable_nonces_timestamp_millis.flush();

            let storage_usage_after = near_sdk::env::storage_usage();
            self.charge_storage_deposit(
                storage_usage_before,
                storage_usage_after,
                authorized_trade_intent.trade_intent.user_id.clone(),
            );
        }

        OtcTradeEvent::Trade {
            authorized_trade_intents,
        }
        .emit();
        DexCallResponse {
            asset_withdraw_requests,
            ..Default::default()
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn storage_deposit(
        &mut self,
        #[allow(unused_mut)]
        #[serializer(borsh)]
        mut attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        #[near(serializers=[borsh])]
        struct StorageDepositArgs;
        let Ok(StorageDepositArgs) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let predecessor_id = near_sdk::env::predecessor_account_id();
        let Some(attached_near) = attached_assets.remove(&AssetId::Near) else {
            panic!("Near not attached");
        };
        let attached_near = NearToken::from_yoctonear(attached_near.0);
        expect!(
            attached_assets.is_empty(),
            "No assets other than NEAR should be attached"
        );
        self.storage_balances
            .entry(predecessor_id.clone())
            .and_modify(|b| b.total = b.total.saturating_add(attached_near))
            .or_insert(StorageBalance {
                total: attached_near,
                used: NearToken::ZERO,
            });
        let storage_usage_before = near_sdk::env::storage_usage();
        self.storage_balances.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.charge_storage_deposit(storage_usage_before, storage_usage_after, predecessor_id);

        DexCallResponse {
            add_storage_deposit: attached_near,
            ..Default::default()
        }
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn set_authorized_key(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_one_yocto();
        #[near(serializers=[borsh])]
        struct SetAuthorizedKeyArgs {
            key: PublicKey,
        }
        let Ok(SetAuthorizedKeyArgs { key }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");
        let storage_usage_before = near_sdk::env::storage_usage();
        self.authorized_keys
            .insert(near_sdk::env::predecessor_account_id(), key.clone());
        self.authorized_keys.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.charge_storage_deposit(
            storage_usage_before,
            storage_usage_after,
            near_sdk::env::predecessor_account_id(),
        );
        OtcTradeEvent::AuthorizedKeyChanged {
            account_id: near_sdk::env::predecessor_account_id(),
            key,
        }
        .emit();
        DexCallResponse::default()
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn deposit_assets(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_one_yocto();
        #[near(serializers=[borsh])]
        struct DepositAssetsArgs;
        let Ok(DepositAssetsArgs) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let storage_usage_before = near_sdk::env::storage_usage();
        for (asset_id, amount) in attached_assets.iter() {
            self.balances
                .entry((near_sdk::env::predecessor_account_id(), asset_id.clone()))
                .and_modify(|b| b.0 = b.0.checked_add(amount.0).expect("Balance overflow"))
                .or_insert(*amount);
        }
        self.balances.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.charge_storage_deposit(
            storage_usage_before,
            storage_usage_after,
            near_sdk::env::predecessor_account_id(),
        );
        DexCallResponse::default()
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn withdraw_assets(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        assert_one_yocto();
        #[near(serializers=[borsh])]
        struct WithdrawAssetsArgs {
            assets: Vec<WithdrawRequest>,
        }
        #[near(serializers=[borsh])]
        struct WithdrawRequest {
            asset_id: AssetId,
            // If None, the entire balance of the asset will be withdrawn.
            amount: Option<U128>,
            // If None, the assets will be withdrawn to the user's own account.
            to: Option<AccountId>,
            to_inner_balance: bool,
        }
        let Ok(WithdrawAssetsArgs { assets }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");
        let storage_usage_before = near_sdk::env::storage_usage();
        let mut asset_withdraw_requests = Vec::new();
        for WithdrawRequest {
            asset_id,
            amount,
            to,
            to_inner_balance,
        } in assets.iter()
        {
            let amount = amount.unwrap_or_else(|| {
                self.balances
                    .get(&(near_sdk::env::predecessor_account_id(), asset_id.clone()))
                    .copied()
                    .unwrap_or_default()
            });
            self.balances
                .entry((near_sdk::env::predecessor_account_id(), asset_id.clone()))
                .and_modify(|b| {
                    b.0 = b.0.checked_sub(amount.0).unwrap_or_else(|| {
                        panic!(
                            "Not enough balance for asset {asset_id}: {} < {}",
                            b.0, amount.0,
                        )
                    })
                })
                .or_insert_with(|| {
                    panic!("Not enough balance for asset {asset_id}: 0 < {}", amount.0)
                });
            let to = to
                .clone()
                .unwrap_or_else(near_sdk::env::predecessor_account_id);
            asset_withdraw_requests.push(AssetWithdrawRequest {
                asset_id: asset_id.clone(),
                amount,
                withdrawal_type: if *to_inner_balance {
                    AssetWithdrawalType::ToInternalUserBalance(to)
                } else {
                    AssetWithdrawalType::WithdrawUnderlyingAsset(to)
                },
            })
        }
        self.balances.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.charge_storage_deposit(
            storage_usage_before,
            storage_usage_after,
            near_sdk::env::predecessor_account_id(),
        );
        DexCallResponse {
            asset_withdraw_requests,
            ..Default::default()
        }
    }

    #[result_serializer(borsh)]
    pub fn storage_balance_of(
        &self,
        #[serializer(borsh)] account_id: AccountId,
    ) -> Option<&StorageBalance> {
        self.storage_balances.get(&account_id)
    }

    #[result_serializer(borsh)]
    pub fn get_authorized_key(
        &self,
        #[serializer(borsh)] account_id: &AccountId,
    ) -> Option<&PublicKey> {
        self.authorized_keys.get(account_id)
    }

    #[result_serializer(borsh)]
    pub fn is_nonce_used(
        &self,
        #[serializer(borsh)] nonce: Nonce,
        #[serializer(borsh)] account_id: AccountId,
    ) -> bool {
        self.used_nonces.contains(&(account_id, nonce))
    }

    #[result_serializer(borsh)]
    pub fn get_balance(
        &self,
        #[serializer(borsh)] account_id: AccountId,
        #[serializer(borsh)] asset_id: AssetId,
    ) -> Option<&U128> {
        self.balances.get(&(account_id.clone(), asset_id.clone()))
    }
}

impl OtcDex {
    fn charge_storage_deposit(&mut self, before: u64, after: u64, payer: AccountId) {
        match after.cmp(&before) {
            std::cmp::Ordering::Greater => {
                // charge the difference
                let storage_cost = near_sdk::env::storage_byte_cost()
                    .saturating_mul(after.checked_sub(before).expect("Just compared") as u128);
                self.storage_balances
                    .entry(payer.clone())
                    .and_modify(|b| b.used = b.used.saturating_add(storage_cost))
                    .or_insert_with(|| panic!("Storage not registered for {payer}"))
                    .validate();
            }
            std::cmp::Ordering::Less => {
                // refund the difference
                let storage_cost = near_sdk::env::storage_byte_cost()
                    .saturating_mul(before.checked_sub(after).expect("Just compared") as u128);
                self.storage_balances
                    .entry(payer.clone())
                    .and_modify(|b| {
                        b.used = b
                            .used
                            .checked_sub(storage_cost)
                            .expect("Storage used underflow")
                    })
                    .or_insert_with(|| panic!("Storage not registered for {payer}"))
                    .validate();
            }
            std::cmp::Ordering::Equal => {
                // nothing changed
            }
        }
    }
}

#[near(serializers=[borsh])]
pub struct SimplePool {
    assets: (AssetWithBalance, AssetWithBalance),
    owner_id: AccountId,
}

#[near(serializers=[borsh])]
pub struct AssetWithBalance {
    asset_id: AssetId,
    balance: U128,
}
