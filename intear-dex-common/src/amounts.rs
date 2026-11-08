use std::{
    fmt,
    num::{NonZeroU32, NonZeroU128},
};

use ethnum::U256;
use near_sdk::{json_types::U128, near};

#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct TokenAmount(U128);

impl TokenAmount {
    pub const ZERO: Self = Self(U128(0));

    pub const fn new(amount: u128) -> Self {
        Self(U128(amount))
    }

    pub const fn get(self) -> u128 {
        self.0.0
    }

    pub fn is_zero(self) -> bool {
        self.get() == 0
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.get().checked_add(other.get()).map(Self::new)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.get().checked_sub(other.get()).map(Self::new)
    }
}

impl fmt::Display for TokenAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.get())
    }
}

/// 100% = 1000000
#[near(serializers=[borsh, json])]
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct FeeFraction(u32);

pub const FULL_FEE_FRACTION: FeeFraction = FeeFraction::new(1_000_000);

impl FeeFraction {
    pub const ZERO: Self = Self(0);

    pub const fn new(millionths: u32) -> Self {
        Self(millionths)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    /// This fraction of `amount`, rounded down. The fraction must be less than 100%.
    pub fn of(self, amount: TokenAmount) -> TokenAmount {
        TokenAmount::new(fraction_of(amount.get(), self, FULL_FEE_FRACTION, false))
    }

    pub fn non_zero(self) -> NonZeroU32 {
        NonZeroU32::new(self.0).expect("Fee fraction is zero")
    }
}

impl fmt::Display for FeeFraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// `a * b / c`, rounded down. Panics if the result doesn't fit into u128.
pub fn mul_div(a: u128, b: u128, c: NonZeroU128) -> u128 {
    match a.checked_mul(b) {
        Some(product) => product / c,
        None => {
            // u128 * u128 can't overflow u256, and c isn't zero
            #[allow(clippy::arithmetic_side_effects)]
            let quotient = U256::from(a) * U256::from(b) / U256::from(c.get());
            u128::try_from(quotient).expect("Value must be less than 128 bits")
        }
    }
}

/// `amount * numerator / denominator`
pub fn fraction_of(
    amount: u128,
    numerator: FeeFraction,
    denominator: FeeFraction,
    round_up: bool,
) -> u128 {
    let numerator = u128::from(numerator.get());
    let denominator = u128::from(denominator.get());
    crate::expect!(
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
