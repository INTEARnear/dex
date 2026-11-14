use ethnum::U256;

use intear_dex_common::SwapRequestAmount;

use crate::{
    Amplification, AssetIndex, FULL_FEE_FRACTION, FeeFraction, PoolAssets, Price, SwapFees,
    TokenAmount,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapAmounts {
    pub amount_in: TokenAmount,
    pub amount_out: TokenAmount,
    pub oracle_fee: TokenAmount,
    pub pool_fee: TokenAmount,
    pub protocol_fee: TokenAmount,
}

impl SwapAmounts {
    pub fn amount_in_after_fees(&self) -> TokenAmount {
        self.amount_in
            .checked_sub(self.oracle_fee)
            .and_then(|amount| amount.checked_sub(self.pool_fee))
            .and_then(|amount| amount.checked_sub(self.protocol_fee))
            .expect("Fee exceeds 100%")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapError {
    ZeroAmount,
    AmountOutNotLessThanBalance,
    AmountInNotMoreThanFixedFee,
}

impl std::fmt::Display for SwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroAmount => write!(f, "Amount must be greater than 0"),
            Self::AmountOutNotLessThanBalance => write!(f, "Amount must be less than out balance"),
            Self::AmountInNotMoreThanFixedFee => {
                write!(f, "Amount in must be more than the pool's fixed oracle fee")
            }
        }
    }
}

const COINS: U256 = U256::new(2);
/// Newton's method steps
const MAX_STEPS: usize = 255;

/// A pool with its assets rated at the oracle's prices, which swaps on its StableSwap curve
/// trade at while the pool is balanced. Amounts are rated in values: units of 10^-[`Price::DECIMALS`] of a whole
/// token of the second asset.
#[derive(Clone, Copy)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct RatedPool {
    pub balances: PoolAssets<TokenAmount>,
    pub decimals: PoolAssets<u8>,
    /// The price of a whole token of the first asset in the second
    pub price: Price,
    pub amplification: Amplification,
}

impl RatedPool {
    /// The amounts of a swap of `amount` of the asset at `in_index` with `fees`, which are parts of
    /// the input after `fixed_fee`: an exact output takes as much more input as the fees need
    pub fn swap_amounts(
        &self,
        in_index: AssetIndex,
        amount: SwapRequestAmount,
        fees: SwapFees,
        oracle_fee: TokenAmount,
    ) -> Result<SwapAmounts, SwapError> {
        let (SwapRequestAmount::ExactIn(requested_amount)
        | SwapRequestAmount::ExactOut(requested_amount)) = amount;
        if requested_amount.is_zero() {
            return Err(SwapError::ZeroAmount);
        }
        let amount_in = match amount {
            SwapRequestAmount::ExactIn(amount_in) => amount_in,
            SwapRequestAmount::ExactOut(amount_out) => {
                if amount_out >= self.balances[in_index.other()] {
                    return Err(SwapError::AmountOutNotLessThanBalance);
                }
                self.exact_out_amount_in(in_index, amount_out, fees.total())
                    .checked_add(oracle_fee)
                    .expect("Amount must be less than u128::MAX")
            }
        };
        let Some(amount_in_after_oracle_fee) = amount_in
            .checked_sub(oracle_fee)
            .filter(|amount| !amount.is_zero())
        else {
            return Err(SwapError::AmountInNotMoreThanFixedFee);
        };
        let mut amounts = SwapAmounts {
            amount_in,
            amount_out: TokenAmount::ZERO,
            oracle_fee,
            pool_fee: fees.pool.of(amount_in_after_oracle_fee),
            protocol_fee: fees.protocol.of(amount_in_after_oracle_fee),
        };
        amounts.amount_out = match amount {
            SwapRequestAmount::ExactIn(_) => {
                self.exact_in_amount_out(in_index, amounts.amount_in_after_fees())
            }
            SwapRequestAmount::ExactOut(amount_out) => amount_out,
        };
        Ok(amounts)
    }

    /// What a swap gets out of the other asset for `amount_in_after_fees` of the asset at
    /// `in_index`, rounded down. Both balances must be worth more than zero.
    pub fn exact_in_amount_out(
        &self,
        in_index: AssetIndex,
        amount_in_after_fees: TokenAmount,
    ) -> TokenAmount {
        let out_index = in_index.other();
        let values = self.values();
        let invariant = invariant(values, self.amplification);
        let input_after_swap = values[in_index]
            .checked_add(self.value_of(in_index, amount_in_after_fees, false))
            .expect("Value overflow");
        let output_after_swap =
            balance_keeping_invariant(input_after_swap, invariant, self.amplification);
        // Less 1 for the rounding of the invariant, as in Curve
        let value_out = values[out_index]
            .saturating_sub(output_after_swap)
            .saturating_sub(U256::ONE);
        self.amount_of(out_index, value_out, false)
    }

    /// What a swap puts in of the asset at `in_index` to get exactly `amount_out` of the other,
    /// before a `total_fee` is taken from it: the least input that leaves enough after the fee,
    /// rounded up. Both balances must be worth more than zero.
    pub fn exact_out_amount_in(
        &self,
        in_index: AssetIndex,
        amount_out: TokenAmount,
        total_fee: FeeFraction,
    ) -> TokenAmount {
        let out_index = in_index.other();
        let values = self.values();
        let invariant = invariant(values, self.amplification);
        let output_after_swap = values[out_index]
            .checked_sub(self.value_of(out_index, amount_out, true))
            .filter(|output_after_swap| *output_after_swap != U256::ZERO)
            .expect("Amount must be less than out balance");
        let input_after_swap =
            balance_keeping_invariant(output_after_swap, invariant, self.amplification);
        // More by 1 for the rounding of the invariant, as in Curve
        let value_in = input_after_swap
            .saturating_sub(values[in_index])
            .checked_add(U256::ONE)
            .expect("Value overflow");
        let amount_in_without_fees = self.amount_of(in_index, value_in, true);
        let fee_denominator = FULL_FEE_FRACTION
            .checked_sub(total_fee)
            .filter(|fee_denominator| fee_denominator.get() != 0)
            .expect("Fee fraction somehow 100% or more");
        let amount_in = mul_div(
            U256::from(amount_in_without_fees.get()),
            U256::from(FULL_FEE_FRACTION.get()),
            U256::from(fee_denominator.get()),
            true,
        );
        TokenAmount::new(u128::try_from(amount_in).expect("Amount must be less than u128::MAX"))
    }

    fn values(&self) -> PoolAssets<U256> {
        let values = PoolAssets(
            AssetIndex::BOTH
                .map(|asset_index| self.value_of(asset_index, self.balances[asset_index], false)),
        );
        assert!(
            values.0.iter().all(|value| *value != U256::ZERO),
            "Pool is empty"
        );
        values
    }

    /// The value of a whole token of the asset at `asset_index`
    fn token_value(&self, asset_index: AssetIndex) -> U256 {
        U256::from(match asset_index {
            AssetIndex::First => self.price.units(),
            AssetIndex::Second => Price::ONE.units(),
        })
    }

    /// The smallest units in a whole token of the asset at `asset_index`
    fn token_units(&self, asset_index: AssetIndex) -> U256 {
        U256::from(10u128.pow(u32::from(self.decimals[asset_index])))
    }

    fn value_of(&self, asset_index: AssetIndex, amount: TokenAmount, round_up: bool) -> U256 {
        mul_div(
            U256::from(amount.get()),
            self.token_value(asset_index),
            self.token_units(asset_index),
            round_up,
        )
    }

    fn amount_of(&self, asset_index: AssetIndex, value: U256, round_up: bool) -> TokenAmount {
        let amount = mul_div(
            value,
            self.token_units(asset_index),
            self.token_value(asset_index),
            round_up,
        );
        TokenAmount::new(u128::try_from(amount).expect("Amount must be less than 128 bits"))
    }
}

/// `D` of the StableSwap invariant of Curve
fn invariant(values: PoolAssets<U256>, amplification: Amplification) -> U256 {
    let [value_0, value_1] = values.0;
    let sum = value_0.checked_add(value_1).expect("Value overflow");
    let smaller_value = value_0.min(value_1);
    let larger_value = value_0.max(value_1);
    let ann = U256::from(amplification.get())
        .checked_mul(COINS)
        .expect("Amplification fits in u256");
    let mut invariant = sum;
    for _ in 0..MAX_STEPS {
        let invariant_product = invariant
            .checked_mul(invariant)
            .and_then(|product| product.checked_div(smaller_value.checked_mul(COINS)?))
            .and_then(|product| product.checked_mul(invariant))
            .and_then(|product| product.checked_div(larger_value.checked_mul(COINS)?))
            .expect("Invariant overflow");
        let previous_invariant = invariant;
        let numerator = ann
            .checked_mul(sum)
            .and_then(|numerator| numerator.checked_add(invariant_product.checked_mul(COINS)?))
            .and_then(|numerator| numerator.checked_mul(invariant))
            .expect("Invariant overflow");
        let denominator = ann
            .checked_sub(U256::ONE)
            .and_then(|ann_minus_one| ann_minus_one.checked_mul(invariant))
            .and_then(|denominator| {
                denominator.checked_add(
                    COINS
                        .checked_add(U256::ONE)?
                        .checked_mul(invariant_product)?,
                )
            })
            .expect("Invariant overflow");
        invariant = numerator
            .checked_div(denominator)
            .expect("Invariant denominator is positive");
        if invariant.abs_diff(previous_invariant) <= U256::ONE {
            return invariant;
        }
    }
    panic!("StableSwap invariant didn't converge");
}

/// The value of one balance that keeps `invariant` with the other balance at `other_value`,
/// found with Newton's method as Curve's `get_y` does
fn balance_keeping_invariant(
    other_value: U256,
    invariant: U256,
    amplification: Amplification,
) -> U256 {
    let ann = U256::from(amplification.get())
        .checked_mul(COINS)
        .expect("Amplification fits in u256");
    let c = invariant
        .checked_mul(invariant)
        .and_then(|c| c.checked_div(other_value.checked_mul(COINS)?))
        .and_then(|c| c.checked_mul(invariant))
        .and_then(|c| c.checked_div(ann.checked_mul(COINS)?))
        .expect("Invariant overflow");
    let b = invariant
        .checked_div(ann)
        .and_then(|invariant_over_ann| other_value.checked_add(invariant_over_ann))
        .expect("Value overflow");
    let mut balance = invariant;
    for _ in 0..MAX_STEPS {
        let previous_balance = balance;
        let numerator = balance
            .checked_mul(balance)
            .and_then(|numerator| numerator.checked_add(c))
            .expect("Value overflow");
        let denominator = balance
            .checked_mul(COINS)
            .and_then(|denominator| denominator.checked_add(b))
            .and_then(|denominator| denominator.checked_sub(invariant))
            .filter(|denominator| *denominator != U256::ZERO)
            .expect("Balance denominator is positive");
        balance = numerator
            .checked_div(denominator)
            .expect("Balance denominator is positive");
        if balance.abs_diff(previous_balance) <= U256::ONE {
            return balance;
        }
    }
    panic!("StableSwap balance didn't converge");
}

/// `a * b / c`, rounded up or down. `c` must not be zero.
fn mul_div(a: U256, b: U256, c: U256, round_up: bool) -> U256 {
    let product = a.checked_mul(b).expect("Value overflow");
    let quotient = product.checked_div(c).expect("Dividing by zero");
    let has_remainder = product.checked_rem(c).expect("Dividing by zero") != U256::ZERO;
    if round_up && has_remainder {
        quotient.checked_add(U256::ONE).expect("Value overflow")
    } else {
        quotient
    }
}
