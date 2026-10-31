#![deny(clippy::arithmetic_side_effects)]

use std::collections::HashMap;

use crypto_bigint::{ConstChoice, I256, U256};
use intear_dex_types::{
    AssetId, AssetWithdrawRequest, AssetWithdrawalType, Caller, Dex, DexCallResponse, SwapRequest,
    SwapRequestAmount, SwapResponse, env, expect,
};
use near_sdk::{
    AccountId, BlockHeight, BorshStorageKey, CurveType, NearToken, PublicKey,
    json_types::{U64, U128},
    near,
    store::{LookupMap, LookupSet, TreeMap},
};
use otc_dex_types::{
    AuthorizationMethod, AuthorizedTradeIntent, DepositAssetsArgs, ExpiryCondition,
    GetAuthorizedKeyArgs, GetBalanceArgs, IsNonceUsedArgs, MatchArgs, Nonce, OtcDexEvent,
    OutputDestination, RegisterAssetsArgs, SetAuthorizedKeyArgs, StorageBalance,
    StorageBalanceOfArgs, StorageDepositArgs, TradeIntent, TradeIntentSignaturePayload, Validity,
    WithdrawAssetsArgs, WithdrawRequest,
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

#[near(contract_state)]
pub struct OtcDex {
    balances: LookupMap<(AccountId, AssetId), U128>,
    authorized_keys: LookupMap<AccountId, PublicKey>,
    storage_balances: LookupMap<AccountId, StorageBalance>,
    used_expirable_nonces_block_height: LookupMap<AccountId, TreeMap<(BlockHeight, Nonce), ()>>,
    used_expirable_nonces_timestamp_millis: LookupMap<AccountId, TreeMap<(U64, Nonce), ()>>,
    used_nonces: LookupSet<(AccountId, Nonce)>,
}

#[near]
impl Dex for OtcDex {
    #[result_serializer(borsh)]
    fn swap(&mut self, #[serializer(borsh)] request: SwapRequest) -> SwapResponse {
        let Ok(maker_intent) =
            near_sdk::borsh::from_slice::<AuthorizedTradeIntent>(&request.message.0)
        else {
            near_sdk::env::panic_str("Invalid message: expected an AuthorizedTradeIntent");
        };
        let trader = env::caller();
        let trader_id = trader.alleged_account_id().clone();
        let maker_trade = &maker_intent.trade_intent;
        expect!(
            maker_trade.user_id != trader_id,
            "Intent not authorized: a trader can't swap with their own intent"
        );
        expect!(
            request.asset_in == maker_trade.asset_out && request.asset_out == maker_trade.asset_in,
            "Swap assets don't match the intent"
        );
        match request.amount {
            SwapRequestAmount::ExactIn(amount_in) => expect!(
                amount_in == maker_trade.amount_out,
                "Amount in doesn't match the intent"
            ),
            SwapRequestAmount::ExactOut(amount_out) => expect!(
                amount_out == maker_trade.amount_in,
                "Amount out doesn't match the intent"
            ),
        }
        if maker_trade.validity.only_for_whitelisted_parties.is_some() {
            expect!(
                match &trader {
                    Caller::Direct(_) => true,
                    Caller::OnTransfer { transfer_of, .. } => *transfer_of == request.asset_in,
                },
                "Intent not authorized: a whitelisted intent can't be swapped in a transfer call of another asset, whose sender can be faked"
            );
        }
        self.validate_authorized_trade_intent(
            &maker_intent,
            &[&maker_trade.user_id, &trader_id],
            &trader,
        );

        self.balances
            .entry((maker_trade.user_id.clone(), maker_trade.asset_in.clone()))
            .and_modify(|b| {
                b.0 =
                    b.0.checked_sub(maker_trade.amount_in.0)
                        .expect("Balance underflow")
            })
            .or_insert_with(|| {
                panic!(
                    "{} has no registered balance for asset {}",
                    maker_trade.user_id, maker_trade.asset_in,
                )
            });
        self.balances
            .entry((maker_trade.user_id.clone(), maker_trade.asset_out.clone()))
            .and_modify(|b| {
                b.0 =
                    b.0.checked_add(maker_trade.amount_out.0)
                        .expect("Balance overflow")
            })
            .or_insert_with(|| {
                panic!(
                    "{} has no registered balance for asset {}",
                    maker_trade.user_id, maker_trade.asset_out,
                )
            });
        self.mark_trade_intent_used(&maker_intent);

        let response = SwapResponse {
            amount_in: maker_trade.amount_out,
            amount_out: maker_trade.amount_in,
        };
        let trader_intent = AuthorizedTradeIntent {
            trade_intent: TradeIntent {
                user_id: trader_id,
                asset_in: request.asset_in,
                asset_out: request.asset_out,
                amount_in: response.amount_in,
                amount_out: response.amount_out,
                validity: Validity::default(),
            },
            authorization_method: AuthorizationMethod::Predecessor,
        };
        OtcDexEvent::Trade {
            authorized_trade_intents: vec![maker_intent, trader_intent],
        }
        .emit();
        response
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
        let Ok(MatchArgs {
            authorized_trade_intents,
            output_destination,
        }) = near_sdk::borsh::from_slice(&args)
        else {
            near_sdk::env::panic_str("Invalid args");
        };
        let trader = env::caller();

        let mut all_required_assets_were_attached = false;
        if !attached_assets.is_empty() {
            let mut required_assets = HashMap::<AssetId, U128>::new();
            for authorized_trade_intent in authorized_trade_intents.iter() {
                if authorized_trade_intent.trade_intent.user_id == *trader.alleged_account_id() {
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
            all_required_assets_were_attached || matches!(trader, Caller::Direct(_)),
            "You must attach all required assets in a transfer call",
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
            self.validate_authorized_trade_intent(
                authorized_trade_intent,
                &authorized_trade_intents
                    .iter()
                    .map(|i| &i.trade_intent.user_id)
                    .collect::<Vec<&AccountId>>(),
                &trader,
            );

            // update balances
            if authorized_trade_intent.trade_intent.user_id == *trader.alleged_account_id()
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

            if authorized_trade_intent.trade_intent.user_id == *trader.alleged_account_id()
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
                    .or_insert_with(|| {
                        panic!(
                            "{} has no registered balance for asset {}",
                            authorized_trade_intent.trade_intent.user_id,
                            authorized_trade_intent.trade_intent.asset_out,
                        )
                    });
            }

            self.mark_trade_intent_used(authorized_trade_intent);
        }

        OtcDexEvent::Trade {
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
        let Ok(StorageDepositArgs) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let account_id = env::caller().alleged_account_id().clone();
        let Some(attached_near) = attached_assets.remove(&AssetId::Near) else {
            panic!("Near not attached");
        };
        let attached_near = NearToken::from_yoctonear(attached_near.0);
        expect!(
            attached_assets.is_empty(),
            "No assets other than NEAR should be attached"
        );
        self.storage_balances
            .entry(account_id.clone())
            .and_modify(|b| b.total = b.total.saturating_add(attached_near))
            .or_insert(StorageBalance {
                total: attached_near,
                used: NearToken::ZERO,
            });
        let storage_usage_before = near_sdk::env::storage_usage();
        self.storage_balances.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.charge_storage_deposit(storage_usage_before, storage_usage_after, account_id);

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
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(SetAuthorizedKeyArgs { key }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");
        let storage_usage_before = near_sdk::env::storage_usage();
        self.authorized_keys.insert(caller_id.clone(), key.clone());
        self.authorized_keys.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.charge_storage_deposit(storage_usage_before, storage_usage_after, caller_id.clone());
        OtcDexEvent::AuthorizedKeyChanged {
            account_id: caller_id,
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
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(DepositAssetsArgs) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        let storage_usage_before = near_sdk::env::storage_usage();
        for (asset_id, amount) in attached_assets.iter() {
            self.balances
                .entry((caller_id.clone(), asset_id.clone()))
                .and_modify(|b| b.0 = b.0.checked_add(amount.0).expect("Balance overflow"))
                .or_insert(*amount);
        }
        self.balances.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.charge_storage_deposit(storage_usage_before, storage_usage_after, caller_id);
        DexCallResponse::default()
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn register_assets(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
        let Ok(RegisterAssetsArgs { asset_ids }) = near_sdk::borsh::from_slice(&args) else {
            near_sdk::env::panic_str("Invalid args");
        };
        expect!(attached_assets.is_empty(), "No assets should be attached");
        let storage_usage_before = near_sdk::env::storage_usage();
        for asset_id in asset_ids {
            self.balances
                .entry((caller_id.clone(), asset_id))
                .or_default();
        }
        self.balances.flush();
        let storage_usage_after = near_sdk::env::storage_usage();
        self.charge_storage_deposit(storage_usage_before, storage_usage_after, caller_id);
        DexCallResponse::default()
    }

    #[payable]
    #[result_serializer(borsh)]
    pub fn withdraw_assets(
        &mut self,
        #[serializer(borsh)] attached_assets: HashMap<AssetId, U128>,
        #[serializer(borsh)] args: Vec<u8>,
    ) -> DexCallResponse {
        expect!(
            let Caller::Direct(caller_id) = env::caller(),
            "This method can only be called directly, not in a transfer call"
        );
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
                    .get(&(caller_id.clone(), asset_id.clone()))
                    .copied()
                    .unwrap_or_default()
            });
            self.balances
                .entry((caller_id.clone(), asset_id.clone()))
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
            let to = to.clone().unwrap_or_else(|| caller_id.clone());
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
        self.charge_storage_deposit(storage_usage_before, storage_usage_after, caller_id);
        DexCallResponse {
            asset_withdraw_requests,
            ..Default::default()
        }
    }

    #[result_serializer(borsh)]
    pub fn storage_balance_of(
        &self,
        #[serializer(borsh)] args: StorageBalanceOfArgs,
    ) -> Option<&StorageBalance> {
        self.storage_balances.get(&args.account_id)
    }

    #[result_serializer(borsh)]
    pub fn get_authorized_key(
        &self,
        #[serializer(borsh)] args: GetAuthorizedKeyArgs,
    ) -> Option<&PublicKey> {
        self.authorized_keys.get(&args.account_id)
    }

    #[result_serializer(borsh)]
    pub fn is_nonce_used(&self, #[serializer(borsh)] args: IsNonceUsedArgs) -> bool {
        self.used_nonces.contains(&(args.account_id, args.nonce))
    }

    #[result_serializer(borsh)]
    pub fn get_balance(&self, #[serializer(borsh)] args: GetBalanceArgs) -> Option<&U128> {
        self.balances.get(&(args.account_id, args.asset_id))
    }
}

impl OtcDex {
    fn validate_authorized_trade_intent(
        &self,
        authorized_trade_intent: &AuthorizedTradeIntent,
        all_user_ids: &[&AccountId],
        trader: &Caller,
    ) {
        let AuthorizedTradeIntent {
            trade_intent,
            authorization_method,
        } = authorized_trade_intent;
        if let Some(expiry) = &trade_intent.validity.expiry {
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
            &trade_intent.validity.only_for_whitelisted_parties
        {
            for other_user_id in all_user_ids {
                if **other_user_id != trade_intent.user_id {
                    expect!(
                        only_for_whitelisted_parties.contains(*other_user_id),
                        "Intent not authorized: User {other_user_id} not whitelisted"
                    );
                }
            }
        }
        match authorization_method {
            AuthorizationMethod::Predecessor => match trader {
                Caller::Direct(caller_id) => {
                    expect!(
                        *caller_id == trade_intent.user_id,
                        "Intent not authorized: AuthorizationMethod::Predecessor cannot be used if the predecessor is not the user"
                    );
                }
                Caller::OnTransfer { alleged_sender, .. } => {
                    expect!(
                        *alleged_sender == trade_intent.user_id,
                        "Intent not authorized: AuthorizationMethod::Predecessor cannot be used if the sender of the transfer call is not the user"
                    );
                }
            },
            AuthorizationMethod::Signature { signature, nonce } => {
                if let Some(expected_public_key) = self.authorized_keys.get(&trade_intent.user_id) {
                    let signature_payload = TradeIntentSignaturePayload {
                        chain_id: near_sdk::env::chain_id(),
                        engine_contract_id: env::engine_contract_id(),
                        dex_id: env::dex_id(),
                        trade_intent: trade_intent.clone(),
                        nonce: *nonce,
                    };
                    let hash = near_sdk::env::sha256_array(
                        near_sdk::borsh::to_vec(&signature_payload).unwrap(),
                    );
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

    fn mark_trade_intent_used(&mut self, authorized_trade_intent: &AuthorizedTradeIntent) {
        let trade_intent = &authorized_trade_intent.trade_intent;
        let storage_usage_before = near_sdk::env::storage_usage();
        if let Some(map) = self
            .used_expirable_nonces_block_height
            .get_mut(&trade_intent.user_id)
            && let Some((block_height, nonce)) = map.keys().next().copied()
            && block_height < near_sdk::env::block_height()
        {
            map.remove(&(block_height, nonce));
            self.used_nonces
                .remove(&(trade_intent.user_id.clone(), nonce));
        }
        if let Some(map) = self
            .used_expirable_nonces_timestamp_millis
            .get_mut(&trade_intent.user_id)
            && let Some((timestamp_millis, nonce)) = map.keys().next().copied()
            && timestamp_millis.0 < near_sdk::env::block_timestamp_ms()
        {
            map.remove(&(timestamp_millis, nonce));
            self.used_nonces
                .remove(&(trade_intent.user_id.clone(), nonce));
        }

        // mark nonce as used
        if let AuthorizationMethod::Signature { nonce, .. } =
            &authorized_trade_intent.authorization_method
        {
            expect!(
                !self
                    .used_nonces
                    .contains(&(trade_intent.user_id.clone(), *nonce)),
                "Intent not authorized: Nonce already used"
            );
            self.used_nonces
                .insert((trade_intent.user_id.clone(), *nonce));
            if let Some(expiry_condition) = trade_intent.validity.expiry {
                match expiry_condition {
                    ExpiryCondition::BlockHeight(block_height) => {
                        self.used_expirable_nonces_block_height
                            .entry(trade_intent.user_id.clone())
                            .or_insert_with(|| {
                                TreeMap::new(StorageKey::UsedExpirableNoncesBlockHeightAccount {
                                    account_id: trade_intent.user_id.clone(),
                                })
                            })
                            .insert((block_height, *nonce), ());
                    }
                    ExpiryCondition::Timestamp { milliseconds } => {
                        self.used_expirable_nonces_timestamp_millis
                            .entry(trade_intent.user_id.clone())
                            .or_insert_with(|| {
                                TreeMap::new(
                                    StorageKey::UsedExpirableNoncesTimestampMillisAccount {
                                        account_id: trade_intent.user_id.clone(),
                                    },
                                )
                            })
                            .insert((milliseconds, *nonce), ());
                    }
                }
            }
        }

        if let Some(map) = self
            .used_expirable_nonces_block_height
            .get_mut(&trade_intent.user_id)
        {
            map.flush();
        }
        if let Some(map) = self
            .used_expirable_nonces_timestamp_millis
            .get_mut(&trade_intent.user_id)
        {
            map.flush();
        }
        self.used_expirable_nonces_block_height.flush();
        self.used_expirable_nonces_timestamp_millis.flush();

        let storage_usage_after = near_sdk::env::storage_usage();
        self.charge_storage_deposit(
            storage_usage_before,
            storage_usage_after,
            trade_intent.user_id.clone(),
        );
    }

    fn charge_storage_deposit(&mut self, before: u64, after: u64, payer: AccountId) {
        match after.cmp(&before) {
            std::cmp::Ordering::Greater => {
                // charge the difference
                let storage_cost = near_sdk::env::storage_byte_cost()
                    .saturating_mul(after.checked_sub(before).expect("Just compared") as u128);
                let storage_balance = self
                    .storage_balances
                    .entry(payer.clone())
                    .and_modify(|b| b.used = b.used.saturating_add(storage_cost))
                    .or_insert_with(|| panic!("Storage not registered for {payer}"));
                expect!(
                    storage_balance.total >= storage_balance.used,
                    "Not enough storage deposit"
                );
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
                    .or_insert_with(|| panic!("Storage not registered for {payer}"));
            }
            std::cmp::Ordering::Equal => {
                // nothing changed
            }
        }
    }
}
