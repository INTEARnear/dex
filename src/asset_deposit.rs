use std::collections::HashMap;

use intear_dex_types::{AccountOrDexId, AssetId, DepositMessage, expect};
use near_contract_standards::{
    fungible_token::receiver::FungibleTokenReceiver,
    non_fungible_token::{self, core::NonFungibleTokenReceiver},
};
use near_sdk::{AccountId, PromiseOrValue, json_types::U128, near};

use crate::{DexEngine, DexEngineExt, IntearDexEvent, call_events::CallEvents};

#[near]
impl DexEngine {
    #[payable]
    /// Deposit near to the dex engine contract's inner
    /// balance for the user.
    pub fn deposit_near(&mut self, operations: Option<DepositMessage>) {
        let mut events = CallEvents::default();
        self.assert_not_paused();
        let deposit = U128(near_sdk::env::attached_deposit().as_yoctonear());
        self.total_in_custody
            .entry(AssetId::Near)
            .and_modify(|b| {
                b.0 = b.0.checked_add(deposit.0).unwrap_or_else(|| {
                    panic!(
                        "Balance overflow for contract and asset Near: {} + {} > {}",
                        b.0,
                        deposit.0,
                        u128::MAX
                    )
                })
            })
            .or_insert_with(|| {
                panic!("Failed to deposit assets to contract tracked balance: asset not registered")
            });

        if let Some(message) = operations {
            self.internal_execute_operations(
                &mut events,
                message.operations(),
                near_sdk::env::predecessor_account_id(),
                Some(HashMap::from_iter([(AssetId::Near, deposit)])),
                message.referrer(),
                message.referral_fees(),
            );
        } else {
            self.internal_increase_assets(
                &mut events,
                AccountOrDexId::Account(near_sdk::env::predecessor_account_id()),
                AssetId::Near,
                U128(near_sdk::env::attached_deposit().as_yoctonear()),
            );
            IntearDexEvent::UserDeposit {
                account_id: near_sdk::env::predecessor_account_id(),
                asset_id: AssetId::Near,
                amount: deposit,
            }
            .emit();
        }
        events.emit();
    }
}

#[near]
impl FungibleTokenReceiver for DexEngine {
    fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        let mut events = CallEvents::default();
        self.assert_not_paused();
        let contract_id = near_sdk::env::predecessor_account_id();
        let operations: Option<DepositMessage> = if msg.is_empty() {
            None
        } else {
            Some(near_sdk::serde_json::from_str(&msg).expect("Failed to parse operations"))
        };

        self.total_in_custody
            .entry(AssetId::Nep141(contract_id.clone()))
            .and_modify(|b| {
                b.0 = b.0.checked_add(amount.0).unwrap_or_else(|| {
                    panic!(
                        "Balance overflow for contract and asset Nep141: {} + {} > {}",
                        b.0,
                        amount.0,
                        u128::MAX
                    )
                })
            })
            .or_insert_with(|| {
                panic!("Failed to deposit assets to contract tracked balance: asset not registered")
            });

        if let Some(message) = operations {
            self.internal_execute_operations(
                &mut events,
                message.operations(),
                sender_id,
                Some(HashMap::from_iter([(AssetId::Nep141(contract_id), amount)])),
                message.referrer(),
                message.referral_fees(),
            );
        } else {
            self.internal_increase_assets(
                &mut events,
                AccountOrDexId::Account(sender_id.clone()),
                AssetId::Nep141(contract_id.clone()),
                amount,
            );
            IntearDexEvent::UserDeposit {
                account_id: sender_id,
                asset_id: AssetId::Nep141(contract_id),
                amount,
            }
            .emit();
        }

        events.emit();
        PromiseOrValue::Value(U128(0))
    }
}

#[near]
impl NonFungibleTokenReceiver for DexEngine {
    fn nft_on_transfer(
        &mut self,
        sender_id: AccountId,
        previous_owner_id: AccountId,
        token_id: non_fungible_token::TokenId,
        msg: String,
    ) -> PromiseOrValue<bool> {
        let mut events = CallEvents::default();
        self.assert_not_paused();
        let contract_id = near_sdk::env::predecessor_account_id();
        let message: Option<DepositMessage> = if msg.is_empty() {
            None
        } else {
            if sender_id != previous_owner_id {
                panic!(
                    "Only the previous owner can execute operations on behalf of the previous owner"
                );
            }
            Some(near_sdk::serde_json::from_str(&msg).expect("Failed to parse operations"))
        };

        self.total_in_custody
            .entry(AssetId::Nep171(contract_id.clone(), token_id.to_string()))
            .and_modify(|b| {
                b.0 = b.0.checked_add(1).unwrap_or_else(|| {
                    panic!(
                        "Balance overflow for contract and asset Nep171: {} + 1 > {}",
                        b.0,
                        u128::MAX
                    )
                })
            })
            .or_insert_with(|| {
                panic!("Failed to deposit assets to contract tracked balance: asset not registered")
            });

        if let Some(message) = message {
            self.internal_execute_operations(
                &mut events,
                message.operations(),
                previous_owner_id,
                Some(HashMap::from_iter([(
                    AssetId::Nep171(contract_id, token_id),
                    U128(1),
                )])),
                message.referrer(),
                message.referral_fees(),
            );
        } else {
            self.internal_increase_assets(
                &mut events,
                AccountOrDexId::Account(previous_owner_id.clone()),
                AssetId::Nep171(contract_id.clone(), token_id.to_string()),
                U128(1),
            );
            IntearDexEvent::UserDeposit {
                account_id: previous_owner_id,
                asset_id: AssetId::Nep171(contract_id, token_id),
                amount: U128(1),
            }
            .emit();
        }

        events.emit();
        PromiseOrValue::Value(false)
    }
}

// There's no interface in near-contract-standards for nep245
#[near]
impl DexEngine {
    pub fn mt_on_transfer(
        &mut self,
        sender_id: AccountId,
        previous_owner_ids: Vec<AccountId>,
        token_ids: Vec<String>,
        amounts: Vec<U128>,
        msg: String,
    ) -> PromiseOrValue<Vec<U128>> {
        let mut events = CallEvents::default();
        self.assert_not_paused();
        expect!(
            previous_owner_ids.len() == token_ids.len()
                && previous_owner_ids.len() == amounts.len(),
            "Invalid input array lengths"
        );

        let contract_id = near_sdk::env::predecessor_account_id();

        let message: Option<DepositMessage> = if msg.is_empty() {
            None
        } else {
            for previous_owner_id in previous_owner_ids.iter() {
                if *previous_owner_id != sender_id {
                    panic!(
                        "Only the previous owner can execute operations on behalf of the previous owner"
                    );
                }
            }
            Some(near_sdk::serde_json::from_str(&msg).expect("Failed to parse operations"))
        };

        let mut attached_assets = HashMap::new();
        for (token_id, amount) in token_ids.iter().zip(amounts.iter()) {
            self.total_in_custody
                .entry(AssetId::Nep245(contract_id.clone(), token_id.clone()))
                .and_modify(|b| {
                    b.0 = b.0.checked_add(amount.0).unwrap_or_else(|| {
                        panic!(
                            "Balance overflow for contract and asset Nep245: {} + {} > {}",
                            b.0,
                            amount.0,
                            u128::MAX
                        )
                    })
                })
                .or_insert_with(|| {
                    panic!(
                        "Failed to deposit assets to contract tracked balance: asset not registered"
                    )
                });
            attached_assets
                .entry(AssetId::Nep245(contract_id.clone(), token_id.clone()))
                .and_modify(|b: &mut U128| {
                    b.0 = b.0.checked_add(amount.0).unwrap_or_else(|| {
                        panic!(
                            "Balance overflow for contract and asset Nep245: {} + {} > {}",
                            b.0,
                            amount.0,
                            u128::MAX
                        )
                    })
                })
                .or_insert(*amount);
        }

        if let Some(message) = message {
            self.internal_execute_operations(
                &mut events,
                message.operations(),
                sender_id,
                Some(attached_assets),
                message.referrer(),
                message.referral_fees(),
            );
        } else {
            for ((token_id, previous_owner_id), amount) in token_ids
                .iter()
                .zip(previous_owner_ids.iter())
                .zip(amounts.iter())
            {
                self.internal_increase_assets(
                    &mut events,
                    AccountOrDexId::Account(previous_owner_id.clone()),
                    AssetId::Nep245(contract_id.clone(), token_id.clone()),
                    *amount,
                );
                IntearDexEvent::UserDeposit {
                    account_id: previous_owner_id.clone(),
                    asset_id: AssetId::Nep245(contract_id.clone(), token_id.clone()),
                    amount: *amount,
                }
                .emit();
            }
        }

        events.emit();
        PromiseOrValue::Value(vec![U128(0); token_ids.len()])
    }
}
