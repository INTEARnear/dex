//! The xyk contract's share and amount math, with its 256-bit intermediate
//! precision and rounding, and the bounds that slippage protection puts on
//! the results

use color_eyre::eyre::eyre;
use crypto_bigint::{CheckedMul, U256};
use xyk_dex_types::{FeeFraction, FULL_FEE_FRACTION};

fn u256_to_u128(value: U256) -> Option<u128> {
    if value.bits() > u128::BITS {
        return None;
    }
    let little_endian_bytes = value.to_le_bytes();
    let (low_bytes, _) = little_endian_bytes.split_first_chunk::<16>()?;
    Some(u128::from_le_bytes(*low_bytes))
}

/// `a * b / c`, rounded down
pub fn mul_div_floor(a: u128, b: u128, c: u128) -> color_eyre::eyre::Result<u128> {
    let product = Option::<U256>::from(U256::from(a).checked_mul(&U256::from(b)))
        .ok_or_else(|| eyre!("{a} * {b} overflows"))?;
    let quotient = Option::<U256>::from(product.checked_div(&U256::from(c)))
        .ok_or_else(|| eyre!("Division of {a} * {b} by zero"))?;
    u256_to_u128(quotient).ok_or_else(|| eyre!("{a} * {b} / {c} doesn't fit in 128 bits"))
}

/// The least that `expected` may become with `max_slippage`, rounded down,
/// in the user's favor
pub fn at_least_with_slippage(
    expected: u128,
    max_slippage: FeeFraction,
) -> color_eyre::eyre::Result<u128> {
    let kept_fraction = FULL_FEE_FRACTION
        .checked_sub(max_slippage)
        .ok_or_else(|| eyre!("Slippage over 100%"))?;
    mul_div_floor(
        expected,
        u128::from(kept_fraction),
        u128::from(FULL_FEE_FRACTION),
    )
}

/// The shares that adding `amounts` to a public pool with `reserves` and
/// `total_shares` mints. Like the contract, it leaves one unit of each amount
/// out, and the side that buys fewer shares decides.
pub fn shares_for_liquidity(
    amounts: (u128, u128),
    reserves: (u128, u128),
    total_shares: u128,
) -> color_eyre::eyre::Result<u128> {
    let shares_for = |amount: u128, reserve: u128| {
        mul_div_floor(
            amount
                .checked_sub(1)
                .ok_or_else(|| eyre!("Amounts must be more than zero"))?,
            total_shares,
            reserve,
        )
    };
    Ok(shares_for(amounts.0, reserves.0)?.min(shares_for(amounts.1, reserves.1)?))
}

/// What removing `shares` of `total_shares` takes out of each reserve
pub fn liquidity_for_shares(
    shares: u128,
    total_shares: u128,
    reserves: (u128, u128),
) -> color_eyre::eyre::Result<(u128, u128)> {
    if shares == total_shares {
        return Ok(reserves);
    }
    Ok((
        mul_div_floor(reserves.0, shares, total_shares)?,
        mul_div_floor(reserves.1, shares, total_shares)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn products_beyond_128_bits_divide_exactly() {
        assert_eq!(mul_div_floor(u128::MAX, 4, 8).unwrap(), u128::MAX / 2);
        assert_eq!(mul_div_floor(7, 3, 2).unwrap(), 10);
        assert!(mul_div_floor(1, 1, 0).is_err());
        assert!(mul_div_floor(u128::MAX, 2, 1).is_err());
    }

    #[test]
    fn slippage_bound_rounds_for_the_user() {
        assert_eq!(at_least_with_slippage(1_000, 5_000).unwrap(), 995);
        assert_eq!(at_least_with_slippage(999, 5_000).unwrap(), 994);
        assert_eq!(at_least_with_slippage(1_000, 0).unwrap(), 1_000);
    }

    #[test]
    fn shares_follow_the_scarcer_side() {
        // Twice the pool's ratio of the first asset buys no more shares
        assert_eq!(
            shares_for_liquidity((201, 101), (100, 100), 1_000).unwrap(),
            1_000
        );
        assert_eq!(
            liquidity_for_shares(250, 1_000, (100, 40)).unwrap(),
            (25, 10)
        );
        assert_eq!(
            liquidity_for_shares(1_000, 1_000, (101, 41)).unwrap(),
            (101, 41)
        );
    }
}
