use std::num::NonZeroU128;

use near_sdk::near;

use crate::{TokenAmount, mul_div};

/// 100% of the requested amount = 1000000
#[near(serializers=[borsh])]
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct RouteFraction(u32);

pub const FULL_ROUTE_FRACTION: RouteFraction = RouteFraction::new(1_000_000);

impl RouteFraction {
    pub const fn new(millionths: u32) -> Self {
        Self(millionths)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    pub fn add_up_to_full(fractions: impl IntoIterator<Item = Self>) -> bool {
        fractions
            .into_iter()
            .try_fold(0u32, |total, fraction| total.checked_add(fraction.0))
            == Some(FULL_ROUTE_FRACTION.0)
    }

    pub fn of(self, amount: TokenAmount) -> TokenAmount {
        let full = NonZeroU128::new(u128::from(FULL_ROUTE_FRACTION.0)).expect("Is 1_000_000");
        TokenAmount::new(mul_div(amount.get(), u128::from(self.0), full))
    }

    /// The parts of `amount` that routes of `fractions` swap. Fractions round down, so the first
    /// part also has what's left.
    pub fn split(
        amount: TokenAmount,
        fractions: impl IntoIterator<Item = Self>,
    ) -> Vec<TokenAmount> {
        let mut parts = fractions
            .into_iter()
            .map(|fraction| fraction.of(amount))
            .collect::<Vec<_>>();
        let unsplit_amount = parts
            .iter()
            .try_fold(amount, |unsplit_amount, part| {
                unsplit_amount.checked_sub(*part)
            })
            .expect("Routes exceed the requested amount");
        let first_part = parts.first_mut().expect("There are routes");
        *first_part = first_part.checked_add(unsplit_amount).expect("Overflow");
        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_route_also_swaps_what_rounding_leaves() {
        let third = RouteFraction::new(333_333);
        let rest = RouteFraction::new(FULL_ROUTE_FRACTION.get() - 2 * third.get());
        assert!(RouteFraction::add_up_to_full([rest, third, third]));
        assert_eq!(
            RouteFraction::split(TokenAmount::new(100), [rest, third, third]),
            vec![
                TokenAmount::new(34),
                TokenAmount::new(33),
                TokenAmount::new(33)
            ]
        );
    }
}
