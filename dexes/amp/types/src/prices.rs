use std::{fmt, str::FromStr};

use ethnum::U256;
use intear_dex_common::AssetId;
use near_sdk::{
    near,
    serde::{Deserialize, Deserializer, Serialize, Serializer},
};

/// A price with [`Price::DECIMALS`] decimals, of a whole token in the quote currency of an
/// exchange, or of a whole token of a pool's first asset in its second
#[near(serializers=[borsh])]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Price(u128);

impl Price {
    pub const DECIMALS: u32 = 18;
    pub const ONE: Self = Self(10u128.pow(Self::DECIMALS));

    pub const fn from_units(units: u128) -> Self {
        Self(units)
    }

    /// The price in units of 10^-[`Price::DECIMALS`]
    pub const fn units(self) -> u128 {
        self.0
    }

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// The price of a whole token of the asset this is the price of, in the asset `other` is the
    /// price of, rounded down, `None` if `other` is zero
    pub fn in_terms_of(self, other: Price) -> Option<Price> {
        let price = U256::from(self.0)
            .checked_mul(U256::from(Self::ONE.0))?
            .checked_div(U256::from(other.0))?;
        Some(Self(
            u128::try_from(price).expect("Price doesn't fit in 128 bits"),
        ))
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let whole = self.0.checked_div(Self::ONE.0).expect("ONE is not zero");
        let fraction = self.0.checked_rem(Self::ONE.0).expect("ONE is not zero");
        if fraction == 0 {
            return write!(f, "{whole}");
        }
        let fraction = format!("{fraction:0>width$}", width = Self::DECIMALS as usize);
        write!(f, "{whole}.{}", fraction.trim_end_matches('0'))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsePriceError {
    NotADecimal(String),
    TooManyDecimals(String),
    TooHigh(String),
}

impl fmt::Display for ParsePriceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADecimal(price) => write!(f, "{price} is not a decimal number"),
            Self::TooManyDecimals(price) => {
                write!(f, "{price} has more than {} decimals", Price::DECIMALS)
            }
            Self::TooHigh(price) => write!(f, "{price} is too high for a price"),
        }
    }
}

impl std::error::Error for ParsePriceError {}

impl FromStr for Price {
    type Err = ParsePriceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
        let is_digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
        if whole.is_empty() || !is_digits(whole) || !is_digits(fraction) {
            return Err(ParsePriceError::NotADecimal(s.to_string()));
        }
        if fraction.len() > Self::DECIMALS as usize {
            return Err(ParsePriceError::TooManyDecimals(s.to_string()));
        }
        let units = format!(
            "{whole}{fraction:0<width$}",
            width = Self::DECIMALS as usize
        )
        .parse::<u128>()
        .map_err(|_| ParsePriceError::TooHigh(s.to_string()))?;
        Ok(Self(units))
    }
}

impl Serialize for Price {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_string().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Price {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let price = String::deserialize(deserializer)?;
        Self::from_str(&price).map_err(near_sdk::serde::de::Error::custom)
    }
}

#[cfg(all(feature = "abi", not(target_arch = "wasm32")))]
impl near_sdk::schemars::JsonSchema for Price {
    fn schema_name() -> String {
        "Price".to_string()
    }

    fn json_schema(
        generator: &mut near_sdk::schemars::r#gen::SchemaGenerator,
    ) -> near_sdk::schemars::schema::Schema {
        <String as near_sdk::schemars::JsonSchema>::json_schema(generator)
    }
}

/// What swaps ask their pools' oracle for with an `OracleRequest` operation, as the borsh of
/// this: the prices of the assets of every pool they swap on
#[near(serializers=[borsh])]
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PricesRequest {
    pub asset_ids: Vec<AssetId>,
}

impl PricesRequest {
    /// Of the assets of pools of `asset_pairs`, each once, in the order the pools have them
    pub fn of_pools<'a>(asset_pairs: impl IntoIterator<Item = (&'a AssetId, &'a AssetId)>) -> Self {
        let mut asset_ids: Vec<AssetId> = Vec::new();
        for (asset_0, asset_1) in asset_pairs {
            for asset_id in [asset_0, asset_1] {
                if !asset_ids.contains(asset_id) {
                    asset_ids.push(asset_id.clone());
                }
            }
        }
        Self { asset_ids }
    }

    fn index_of(&self, asset_id: &AssetId) -> Result<usize, PricesError> {
        self.asset_ids
            .iter()
            .position(|requested| requested == asset_id)
            .ok_or_else(|| PricesError::AssetNotRequested {
                asset_id: asset_id.clone(),
            })
    }
}

/// What the oracle responds to a [`PricesRequest`] with, as the borsh of this
#[near(serializers=[borsh, json])]
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct PricesResponse {
    pub exchanges: Vec<ExchangePrices>,
}

#[near(serializers=[borsh, json])]
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
pub struct ExchangePrices {
    pub exchange: String,
    /// The prices of whole tokens of the requested assets, in their order, in the exchange's quote
    /// currency
    pub prices: Vec<Price>,
}

/// The fewest exchanges that the oracle's prices must come from
pub const MIN_EXCHANGES: usize = 3;
/// How far apart the prices of exchanges can be, in millionths of the lowest price: 0.5%
pub const MAX_PRICE_SPREAD_MILLIONTHS: u32 = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PricesError {
    AssetNotRequested {
        asset_id: AssetId,
    },
    NotEnoughExchanges {
        count: usize,
    },
    DuplicateExchange {
        exchange: String,
    },
    WrongPriceCount {
        exchange: String,
        count: usize,
        requested: usize,
    },
    ZeroPrice {
        exchange: String,
    },
    TooFarApart {
        lowest: Price,
        highest: Price,
    },
}

impl fmt::Display for PricesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AssetNotRequested { asset_id } => {
                write!(f, "{asset_id} isn't one of the requested assets")
            }
            Self::NotEnoughExchanges { count } => write!(
                f,
                "Oracle has prices of {count} exchanges, but at least {MIN_EXCHANGES} are needed"
            ),
            Self::DuplicateExchange { exchange } => {
                write!(f, "Oracle has more than one price of {exchange}")
            }
            Self::WrongPriceCount {
                exchange,
                count,
                requested,
            } => write!(
                f,
                "Oracle has {count} prices of {exchange}, but {requested} assets were requested"
            ),
            Self::ZeroPrice { exchange } => write!(f, "Oracle has a zero price of {exchange}"),
            Self::TooFarApart { lowest, highest } => {
                let max_spread_percent = Price::from_units(
                    u128::from(MAX_PRICE_SPREAD_MILLIONTHS)
                        .checked_mul(10u128.pow(Price::DECIMALS - 4))
                        .expect("The spread is a percentage"),
                );
                write!(
                    f,
                    "Prices of exchanges are more than {max_spread_percent}% apart: from {lowest} to {highest}"
                )
            }
        }
    }
}

impl std::error::Error for PricesError {}

impl ExchangePrices {
    /// The exchange's price of a whole token of the requested asset at `base_index` in the one at
    /// `quote_index`
    fn price(&self, base_index: usize, quote_index: usize) -> Result<Price, PricesError> {
        let zero_price = || PricesError::ZeroPrice {
            exchange: self.exchange.clone(),
        };
        let price = self.prices[base_index]
            .in_terms_of(self.prices[quote_index])
            .ok_or_else(zero_price)?;
        if price.is_zero() {
            return Err(zero_price());
        }
        Ok(price)
    }
}

impl PricesResponse {
    /// The median of the exchanges' prices of a whole token of `base` in `quote`, two of the
    /// assets of `request`, if there are at least [`MIN_EXCHANGES`] of them, and they are at most
    /// [`MAX_PRICE_SPREAD_MILLIONTHS`] apart
    pub fn median_price(
        &self,
        request: &PricesRequest,
        base: &AssetId,
        quote: &AssetId,
    ) -> Result<Price, PricesError> {
        let (base_index, quote_index) = (request.index_of(base)?, request.index_of(quote)?);
        if self.exchanges.len() < MIN_EXCHANGES {
            return Err(PricesError::NotEnoughExchanges {
                count: self.exchanges.len(),
            });
        }
        for (index, exchange_prices) in self.exchanges.iter().enumerate() {
            if self.exchanges[..index]
                .iter()
                .any(|earlier| earlier.exchange == exchange_prices.exchange)
            {
                return Err(PricesError::DuplicateExchange {
                    exchange: exchange_prices.exchange.clone(),
                });
            }
            if exchange_prices.prices.len() != request.asset_ids.len() {
                return Err(PricesError::WrongPriceCount {
                    exchange: exchange_prices.exchange.clone(),
                    count: exchange_prices.prices.len(),
                    requested: request.asset_ids.len(),
                });
            }
        }
        let mut prices = self
            .exchanges
            .iter()
            .map(|exchange_prices| exchange_prices.price(base_index, quote_index))
            .collect::<Result<Vec<_>, _>>()?;
        prices.sort();
        let (Some(lowest), Some(highest)) = (prices.first(), prices.last()) else {
            unreachable!("There are at least {MIN_EXCHANGES} prices");
        };
        let max_spread_denominator = U256::from(1_000_000u32);
        let max_highest = U256::from(lowest.0)
            .checked_mul(
                max_spread_denominator
                    .checked_add(U256::from(MAX_PRICE_SPREAD_MILLIONTHS))
                    .expect("Spread fits in u256"),
            )
            .expect("u128 times a million fits in u256");
        let highest_times_denominator = U256::from(highest.0)
            .checked_mul(max_spread_denominator)
            .expect("u128 times a million fits in u256");
        if highest_times_denominator > max_highest {
            return Err(PricesError::TooFarApart {
                lowest: *lowest,
                highest: *highest,
            });
        }
        let middle = prices.len() / 2;
        let median = if prices.len().is_multiple_of(2) {
            let (below, above) = (
                prices[middle.checked_sub(1).expect("Not empty")],
                prices[middle],
            );
            let half_of_difference = above.0.checked_sub(below.0).expect("Prices are sorted") / 2;
            Price(
                below
                    .0
                    .checked_add(half_of_difference)
                    .expect("The median is at most the highest price"),
            )
        } else {
            prices[middle]
        };
        Ok(median)
    }
}
