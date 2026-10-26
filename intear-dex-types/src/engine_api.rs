use std::{collections::HashMap, fmt::Display};

use near_sdk::{
    AccountId,
    json_types::{Base58CryptoHash, Base64VecU8, U128},
    near,
    serde::Deserialize,
};

use crate::{AssetId, DexId, SwapRequest, SwapRequestAmount, expect};

/// 100% = 1000000
pub type FeeFraction = u32;
pub const FULL_FEE_FRACTION: FeeFraction = 1000000;
/// The most all referrers of a swap can charge together
pub const MAX_REFERRAL_FEE_FRACTION: FeeFraction = FULL_FEE_FRACTION / 20; // 5%
pub const MAX_REFERRERS: usize = 5;

/// Fees a swap pays referrers out of its input, before the dex gets it. Each
/// fee goes to the referrer's internal balance of the input asset, and a
/// referrer who hasn't registered that asset isn't paid, so the trader keeps
/// that fee. In a batch of operations, only the first swap pays them.
#[derive(Clone, Default)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[json])]
pub struct ReferralFees(pub Vec<(AccountId, FeeFraction)>);

impl ReferralFees {
    pub fn assert_valid(&self) {
        expect!(
            self.0.len() <= MAX_REFERRERS,
            "At most {MAX_REFERRERS} referrers are allowed"
        );
        let total_fee_fraction = self
            .0
            .iter()
            .try_fold(0 as FeeFraction, |total, (_, fee_fraction)| {
                total.checked_add(*fee_fraction)
            });
        expect!(
            total_fee_fraction.is_some_and(|total| total <= MAX_REFERRAL_FEE_FRACTION),
            "Referral fees must add up to at most {MAX_REFERRAL_FEE_FRACTION}"
        );
    }

    /// The fee of each referrer out of `amount_in` that includes the fees, rounded down
    pub fn amounts_out_of(&self, amount_in: U128) -> Vec<(AccountId, U128)> {
        self.0
            .iter()
            .map(|(referrer_id, fee_fraction)| {
                let fee = fraction_of(amount_in.0, *fee_fraction, FULL_FEE_FRACTION, false);
                (referrer_id.clone(), U128(fee))
            })
            .collect()
    }

    /// The fee of each referrer on top of `dex_amount_in`, rounded up, so that each fee is its
    /// fraction of the total input
    pub fn amounts_on_top_of(&self, dex_amount_in: U128) -> Vec<(AccountId, U128)> {
        let total_fee_fraction: FeeFraction =
            self.0.iter().map(|(_, fee_fraction)| *fee_fraction).sum();
        let dex_fraction = FULL_FEE_FRACTION
            .checked_sub(total_fee_fraction)
            .expect("Referral fees exceed 100%");
        self.0
            .iter()
            .map(|(referrer_id, fee_fraction)| {
                let fee = fraction_of(dex_amount_in.0, *fee_fraction, dex_fraction, true);
                (referrer_id.clone(), U128(fee))
            })
            .collect()
    }
}

/// `amount * numerator / denominator`
fn fraction_of(
    amount: u128,
    numerator: FeeFraction,
    denominator: FeeFraction,
    round_up: bool,
) -> u128 {
    let numerator = u128::from(numerator);
    let denominator = u128::from(denominator);
    expect!(
        numerator < denominator,
        "numerator must be less than denominator"
    );
    let whole_parts = amount
        .checked_div(denominator)
        .expect("Fraction of zero")
        .checked_mul(numerator)
        .expect("Overflow");
    let remainder_part = amount
        .checked_rem(denominator)
        .expect("Fraction of zero")
        .checked_mul(numerator)
        .expect("Overflow");
    let remainder_part_rounded = if round_up {
        remainder_part.div_ceil(denominator)
    } else {
        remainder_part
            .checked_div(denominator)
            .expect("Fraction of zero")
    };
    whole_parts
        .checked_add(remainder_part_rounded)
        .expect("Overflow")
}

#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[json])]
pub enum AccountOrDexId {
    Account(AccountId),
    Dex(DexId),
}

impl Display for AccountOrDexId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Account(account) => write!(f, "Account({account})"),
            Self::Dex(dex_id) => write!(f, "Dex({dex_id})"),
        }
    }
}

#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[json])]
pub enum SwapOperationAmount {
    Amount(SwapRequestAmount),
    OutputOfLastIn,
    EntireBalanceIn,
}

/// A dex that [`Operation::SwapAdvanced`] can swap on, with the message to pass to it
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[json])]
pub struct SwapOption {
    pub dex_id: DexId,
    pub message: Base64VecU8,
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
    /// Simulate the swap on each of `options` and swap on the one that gives
    /// the most output (for ExactIn) or takes the least input (for ExactOut).
    /// Options whose simulation fails are skipped, and the swap fails if it's
    /// worse than the simulation of the chosen option.
    SwapAdvanced {
        asset_in: AssetId,
        asset_out: AssetId,
        amount: SwapOperationAmount,
        /// Either minimum amount out (for ExactIn) or maximum amount in (for ExactOut)
        constraint: Option<U128>,
        options: Vec<SwapOption>,
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

#[near(serializers=[json])]
#[serde(untagged)]
pub enum DepositMessage {
    Operations(Vec<Operation>),
    Advanced {
        operations: Vec<Operation>,
        referrer: Option<AccountId>,
        #[serde(default)]
        referral_fees: ReferralFees,
    },
}

impl DepositMessage {
    pub fn operations(&self) -> Vec<Operation> {
        match self {
            Self::Operations(operations) => operations.clone(),
            Self::Advanced { operations, .. } => operations.clone(),
        }
    }

    pub fn referrer(&self) -> Option<AccountId> {
        match self {
            Self::Advanced { referrer, .. } => referrer.clone(),
            _ => None,
        }
    }

    pub fn referral_fees(&self) -> ReferralFees {
        match self {
            Self::Advanced { referral_fees, .. } => referral_fees.clone(),
            _ => ReferralFees::default(),
        }
    }
}

/// An event that a dex logged through the engine's `dex_event` host function,
/// compressed with the other dex events of a call in
/// [`IntearDexEvent::DexEventsBorsh`]. `event` is the borsh of the dex's own
/// event type.
#[near(serializers=[borsh])]
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct DexEventData {
    pub dex_id: DexId,
    pub event: Vec<u8>,
    pub referrer: Option<AccountId>,
    pub user: Option<AccountId>,
}

/// Balance of an asset that an account or dex has at the end of a call
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[json])]
pub struct BalanceUpdate {
    pub owner: AccountOrDexId,
    pub asset_id: AssetId,
    pub balance: U128,
}

#[allow(deprecated)]
mod deprecation_warning_workaround {
    use super::*;

    #[near(event_json(standard = "inteardex"))]
    #[derive(Deserialize)]
    #[cfg_attr(debug_assertions, derive(Debug))]
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
            referrer: Option<AccountId>,
            user: Option<AccountId>,
        },
        /// The events of dexes in a call: base64 of an lz4 block prefixed with its
        /// uncompressed length as a little-endian u32, which is what
        /// `lz4_flex::decompress_size_prepended` reads, of the borsh of a
        /// `Vec<DexEventData>`
        #[event_version("1.0.0")]
        DexEventsBorsh(String),
        #[event_version("1.0.0")]
        UserDeposit {
            account_id: AccountId,
            asset_id: AssetId,
            amount: U128,
        },
        #[event_version("1.0.0")]
        Withdraw {
            from: AccountOrDexId,
            to: AccountId,
            asset_id: AssetId,
            amount: U128,
        },
        /// The final balance of each owner and asset whose balance changed in
        /// a call, in the order they first changed
        #[event_version("2.0.0")]
        BalanceUpdates(Vec<BalanceUpdate>),
        /// Not logged anymore.
        #[deprecated]
        #[event_version("1.0.0")]
        UserBalanceUpdate {
            account_id: AccountId,
            asset_id: AssetId,
            balance: U128,
        },
        /// A dex's balance after it changed, one log per change. Logged until
        /// `balance_updates` replaced it, so only read, for backfilling.
        #[deprecated]
        #[event_version("1.0.0")]
        DexBalanceUpdate {
            dex_id: DexId,
            asset_id: AssetId,
            balance: U128,
        },
        #[event_version("1.0.0")]
        Swap {
            dex_id: DexId,
            request: SwapRequest,
            amount_in: U128,
            amount_out: U128,
            trader: AccountId,
            referrer: Option<AccountId>,
            #[serde(default)]
            referral_fees_paid: Vec<(AccountId, U128)>,
        },
    }
}
pub use deprecation_warning_workaround::*;
