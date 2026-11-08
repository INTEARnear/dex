use std::{
    fmt,
    num::NonZeroU128,
    ops::{Index, IndexMut},
};

use near_sdk::{
    borsh::{
        BorshDeserialize, BorshSerialize,
        io::{Read, Write},
    },
    json_types::U128,
    near,
    serde::{Deserializer, Serializer},
};

use crate::{AssetId, TokenAmount, mul_div};

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub enum AssetIndex {
    First,
    Second,
}

impl AssetIndex {
    pub const BOTH: [AssetIndex; 2] = [AssetIndex::First, AssetIndex::Second];

    pub fn other(self) -> Self {
        match self {
            AssetIndex::First => AssetIndex::Second,
            AssetIndex::Second => AssetIndex::First,
        }
    }
}

/// Something of each asset of a pool, in the order of its assets
#[near(serializers=[borsh])]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct PoolAssets<T>(pub [T; 2]);

impl<T> PoolAssets<T> {
    pub fn each_ref(&self) -> PoolAssets<&T> {
        PoolAssets(self.0.each_ref())
    }
}

impl<T> Index<AssetIndex> for PoolAssets<T> {
    type Output = T;

    fn index(&self, asset_index: AssetIndex) -> &T {
        let [first, second] = &self.0;
        match asset_index {
            AssetIndex::First => first,
            AssetIndex::Second => second,
        }
    }
}

impl<T> IndexMut<AssetIndex> for PoolAssets<T> {
    fn index_mut(&mut self, asset_index: AssetIndex) -> &mut T {
        let [first, second] = &mut self.0;
        match asset_index {
            AssetIndex::First => first,
            AssetIndex::Second => second,
        }
    }
}

#[near(serializers=[borsh, json])]
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct AssetWithBalance {
    pub asset_id: AssetId,
    pub balance: TokenAmount,
}

#[near(serializers=[borsh])]
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct ProviderShares(u128);

impl ProviderShares {
    pub const ZERO: Self = Self(0);

    pub const fn new(shares: u128) -> Self {
        Self(shares)
    }

    pub const fn get(self) -> u128 {
        self.0
    }

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    /// The shares that adding `tokens` to a pool with `reserve` of them mints, rounded down
    pub fn minted_for(
        tokens: TokenAmount,
        total_shares: TotalShares,
        reserve: TokenAmount,
    ) -> Self {
        let reserve = NonZeroU128::new(reserve.get()).expect("Pool is empty");
        Self(mul_div(tokens.get(), total_shares.0, reserve))
    }

    /// What the shares own of `reserve`, rounded down
    pub fn worth_in(self, total_shares: TotalShares, reserve: TokenAmount) -> TokenAmount {
        TokenAmount::new(mul_div(reserve.get(), self.0, total_shares.non_zero()))
    }
}

impl fmt::Display for ProviderShares {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct TotalShares(u128);

impl TotalShares {
    pub const NONE: Self = Self(0);

    pub const fn new(shares: u128) -> Self {
        Self(shares)
    }

    pub const fn get(self) -> u128 {
        self.0
    }

    pub fn is_none(self) -> bool {
        self.0 == 0
    }

    pub fn minted(self, shares: ProviderShares) -> Option<Self> {
        self.0.checked_add(shares.0).map(Self)
    }

    pub fn burned(self, shares: ProviderShares) -> Option<Self> {
        self.0.checked_sub(shares.0).map(Self)
    }

    fn non_zero(self) -> NonZeroU128 {
        NonZeroU128::new(self.0).expect("Pool has no shares")
    }
}

impl BorshSerialize for TotalShares {
    fn serialize<W: Write>(&self, writer: &mut W) -> near_sdk::borsh::io::Result<()> {
        BorshSerialize::serialize(&NonZeroU128::new(self.0), writer)
    }
}

impl BorshDeserialize for TotalShares {
    fn deserialize_reader<R: Read>(reader: &mut R) -> near_sdk::borsh::io::Result<Self> {
        Ok(Self(
            Option::<NonZeroU128>::deserialize_reader(reader)?.map_or(0, NonZeroU128::get),
        ))
    }
}

impl near_sdk::serde::Serialize for TotalShares {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let shares = NonZeroU128::new(self.0).map(|shares| U128(shares.get()));
        near_sdk::serde::Serialize::serialize(&shares, serializer)
    }
}

impl<'de> near_sdk::serde::Deserialize<'de> for TotalShares {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let shares: Option<U128> = near_sdk::serde::Deserialize::deserialize(deserializer)?;
        Ok(Self(shares.map_or(0, |U128(shares)| shares)))
    }
}

#[cfg(all(feature = "abi", not(target_arch = "wasm32")))]
impl near_sdk::schemars::JsonSchema for TotalShares {
    fn schema_name() -> String {
        <Option<U128> as near_sdk::schemars::JsonSchema>::schema_name()
    }

    fn json_schema(
        generator: &mut near_sdk::schemars::r#gen::SchemaGenerator,
    ) -> near_sdk::schemars::schema::Schema {
        <Option<U128> as near_sdk::schemars::JsonSchema>::json_schema(generator)
    }
}

#[cfg(all(feature = "abi", not(target_arch = "wasm32")))]
impl near_sdk::borsh::BorshSchema for TotalShares {
    fn declaration() -> near_sdk::borsh::schema::Declaration {
        <Option<NonZeroU128> as near_sdk::borsh::BorshSchema>::declaration()
    }

    fn add_definitions_recursively(
        definitions: &mut std::collections::BTreeMap<
            near_sdk::borsh::schema::Declaration,
            near_sdk::borsh::schema::Definition,
        >,
    ) {
        <Option<NonZeroU128> as near_sdk::borsh::BorshSchema>::add_definitions_recursively(
            definitions,
        )
    }
}

#[near(serializers=[borsh])]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct PayoutGroupShares(u128);

impl PayoutGroupShares {
    pub const NONE: Self = Self(0);

    pub fn joined_by(self, shares: ProviderShares) -> Option<Self> {
        self.0.checked_add(shares.0).map(Self)
    }

    pub fn left_by(self, shares: ProviderShares) -> Option<Self> {
        self.0.checked_sub(shares.0).map(Self)
    }

    pub fn non_zero(self) -> Option<NonZeroU128> {
        NonZeroU128::new(self.0)
    }

    /// The group's part of `pool_fee`, in proportion to its shares, rounded down
    pub fn part_of(self, pool_fee: TokenAmount, total_shares: TotalShares) -> TokenAmount {
        TokenAmount::new(mul_div(pool_fee.get(), self.0, total_shares.non_zero()))
    }
}
