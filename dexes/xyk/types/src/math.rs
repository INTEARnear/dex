use std::num::NonZeroU128;

use ethnum::U256;

use intear_dex_common::mul_div;

use crate::{
    CurrentFees, FULL_FEE_FRACTION, ProviderShares, SwapReserves, TokenAmount, TotalShares,
};

/// Returns `a * b / (c + d)`, rounded down. Panics if the result doesn't fit into u128.
fn mul_div_sum(a: u128, b: u128, c: u128, d: u128) -> u128 {
    match c.checked_add(d).map(NonZeroU128::new) {
        Some(Some(sum)) => mul_div(a, b, sum),
        Some(None) => panic!("c + d is zero"),
        None => mul_div_u256_sum(a, b, c, d),
    }
}

/// Returns `(a * b + add) / c`, rounded down. Panics if the result doesn't fit into u128.
fn mul_add_div(a: u128, b: u128, add: u128, c: NonZeroU128) -> u128 {
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

/// The shares for `fee` put back into one side of a pool with `reserve` of its asset, rounded
/// down. After it goes in, the pool is worth twice its new reserve of that asset at its price, so
/// shares worth the fee own `fee / (2 * (reserve + fee))` of the pool.
pub fn shares_minted_for_reinvested(
    fee: TokenAmount,
    total_shares: TotalShares,
    reserve: TokenAmount,
) -> ProviderShares {
    let pool_value_before_minting = reserve
        .get()
        .checked_mul(2)
        .and_then(|double_reserve| double_reserve.checked_add(fee.get()))
        .and_then(NonZeroU128::new)
        .expect("Pool value overflow");
    ProviderShares::new(mul_div(
        fee.get(),
        total_shares.get(),
        pool_value_before_minting,
    ))
}

/// What a swap gets out of a pool with `reserves` for `amount_in_after_fees` of the asset put in,
/// rounded down. The input reserve must be positive.
pub fn exact_in_amount_out(
    amount_in_after_fees: TokenAmount,
    reserves: SwapReserves,
) -> TokenAmount {
    TokenAmount::new(mul_div_sum(
        amount_in_after_fees.get(),
        reserves.output.get(),
        reserves.input.get(),
        amount_in_after_fees.get(),
    ))
}

/// What a swap puts into a pool with `reserves` to get exactly `amount_out`, before `fees` are
/// taken from it: the least input that leaves enough after the fees, rounded up. `amount_out`
/// must be less than the output reserve.
pub fn exact_out_amount_in(
    amount_out: TokenAmount,
    reserves: SwapReserves,
    fees: &CurrentFees,
) -> TokenAmount {
    let output_reserve_left = reserves
        .output
        .checked_sub(amount_out)
        .and_then(|left| NonZeroU128::new(left.get()))
        .expect("Amount must be less than out balance");
    let amount_in_without_fees =
        mul_div(reserves.input.get(), amount_out.get(), output_reserve_left)
            .checked_add(1)
            .expect("Value must be less than 128 bits");
    let fee_denominator = FULL_FEE_FRACTION
        .checked_sub(fees.total_fee_fraction())
        .expect("Fee fraction somehow above 100%")
        .non_zero();
    let fee_denominator_minus_one = fee_denominator
        .get()
        .checked_sub(1)
        .expect("Fee fraction somehow equals 100%");
    TokenAmount::new(mul_add_div(
        amount_in_without_fees,
        u128::from(FULL_FEE_FRACTION.get()),
        u128::from(fee_denominator_minus_one),
        NonZeroU128::from(fee_denominator),
    ))
}
