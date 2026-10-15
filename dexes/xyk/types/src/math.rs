use std::num::{NonZeroU32, NonZeroU128};

use ethnum::U256;

use crate::{CurrentFees, FULL_FEE_FRACTION, FeeFraction, SharesBalance};

/// Returns `a * b / c`, rounded down. Panics if the result doesn't fit into u128.
pub fn mul_div(a: u128, b: u128, c: NonZeroU128) -> u128 {
    mul_add_div(a, b, 0, c)
}

/// Returns `a * b / (c + d)`, rounded down. Panics if the result doesn't fit into u128.
pub fn mul_div_sum(a: u128, b: u128, c: u128, d: u128) -> u128 {
    match c.checked_add(d).map(NonZeroU128::new) {
        Some(Some(sum)) => mul_div(a, b, sum),
        Some(None) => panic!("c + d is zero"),
        None => mul_div_u256_sum(a, b, c, d),
    }
}

/// Returns `(a * b + add) / c`, rounded down. Panics if the result doesn't fit into u128.
pub fn mul_add_div(a: u128, b: u128, add: u128, c: NonZeroU128) -> u128 {
    match a
        .checked_mul(b)
        .and_then(|product| product.checked_add(add))
    {
        Some(dividend) => dividend / c,
        None => mul_add_div_u256(a, b, add, c),
    }
}

/// `(a * b + add) / c` in 256 bits, which u128 * u128 + u128 can't overflow
fn mul_add_div_u256(a: u128, b: u128, add: u128, c: NonZeroU128) -> u128 {
    // using NonZeroU128
    #[allow(clippy::arithmetic_side_effects)]
    let quotient = (U256::from(a) * U256::from(b) + U256::from(add)) / U256::from(c.get());
    u128::try_from(quotient).expect("Value must be less than 128 bits")
}

/// `a * b / (c + d)` for a sum past u128, which only u128::MAX-sized reserves add up to
#[cold]
#[inline(never)]
fn mul_div_u256_sum(a: u128, b: u128, c: u128, d: u128) -> u128 {
    // u128 + u128 can't overflow u256, and isn't 0 if it overflowed u128
    #[allow(clippy::arithmetic_side_effects)]
    let quotient = U256::from(a) * U256::from(b) / (U256::from(c) + U256::from(d));
    u128::try_from(quotient).expect("Value must be less than 128 bits")
}

pub fn shares_to_tokens(
    shares: SharesBalance,
    total_shares: SharesBalance,
    total_tokens: NonZeroU128,
) -> u128 {
    mul_div(total_tokens.get(), shares.get(), total_shares)
}

pub fn tokens_to_shares(
    tokens: u128,
    total_shares: SharesBalance,
    total_tokens: NonZeroU128,
) -> u128 {
    mul_div(tokens, total_shares.get(), total_tokens)
}

/// The fee of one receiver on `amount_in`, rounded down
pub fn fee_amount(amount_in: u128, fee_fraction: FeeFraction) -> u128 {
    mul_div(
        amount_in,
        fee_fraction as u128,
        NonZeroU128::new(FULL_FEE_FRACTION as u128).unwrap(),
    )
}

/// What a swap gets out of a pool with `in_balance` and `out_balance` of its assets for
/// `amount_in_after_fees` of the asset put in, rounded down. `in_balance` must be positive.
pub fn exact_in_amount_out(
    amount_in_after_fees: u128,
    in_balance: u128,
    out_balance: u128,
) -> u128 {
    mul_div_sum(
        amount_in_after_fees,
        out_balance,
        in_balance,
        amount_in_after_fees,
    )
}

/// What a swap puts into a pool with `in_balance` and `out_balance` of its assets to get exactly
/// `amount_out`, before `fees` are taken from it: the least input that leaves enough after the
/// fees, rounded up. `amount_out` must be less than `out_balance`.
pub fn exact_out_amount_in(
    amount_out: u128,
    in_balance: u128,
    out_balance: u128,
    fees: &CurrentFees,
) -> u128 {
    let out_balance_left = out_balance
        .checked_sub(amount_out)
        .filter(|left| *left > 0)
        .expect("Amount must be less than out balance")
        .try_into()
        .expect("Amount must be less than out balance");
    let amount_in_without_fees = mul_div(in_balance, amount_out, out_balance_left)
        .checked_add(1)
        .expect("Value must be less than 128 bits");
    let fee_denominator: NonZeroU32 = FULL_FEE_FRACTION
        .checked_sub(fees.total_fee_fraction())
        .expect("Fee fraction somehow above 100%")
        .try_into()
        .expect("Fee fraction somehow equals 100%");
    let fee_denominator_minus_one = fee_denominator
        .get()
        .checked_sub(1)
        .expect("Fee fraction somehow equals 100%");
    mul_add_div(
        amount_in_without_fees,
        FULL_FEE_FRACTION as u128,
        fee_denominator_minus_one as u128,
        NonZeroU128::from(fee_denominator),
    )
}

/// What a launch pool gives of its quote asset for a fee of `fee_amount` of the launched asset,
/// which it swaps into itself after the trade that collected it, when it has `in_balance` of the
/// launched asset and `out_balance` of the quote asset. Neither balance is left zero by a trade.
pub fn fee_converted_to_quote_asset(fee_amount: u128, in_balance: u128, out_balance: u128) -> u128 {
    mul_div_sum(fee_amount, out_balance, in_balance, fee_amount)
}
