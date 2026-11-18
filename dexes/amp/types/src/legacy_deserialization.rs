use near_sdk::{
    PublicKeyHandle,
    borsh::{BorshDeserialize, io::Read},
    serde::Deserialize,
};

use crate::{
    Amplification, AssetWithBalance, FeeConfiguration, FeeFraction, PROTOCOL_FEE_RECEIVER_ID,
    PoolRateSource, PoolView, Price, TokenAmount, TotalShares,
};

/// The fees of a pool from before oracle fees, as the migration sets them
fn fees_before_oracle_fees(fraction: FeeFraction) -> FeeConfiguration {
    FeeConfiguration {
        fraction,
        oracle_fee_per_swap: (TokenAmount::ZERO, TokenAmount::ZERO),
        oracle_fee_receiver: PROTOCOL_FEE_RECEIVER_ID.to_owned(),
    }
}

/// The oracle of a pool from before rate sources, as the migration makes it one
#[derive(BorshDeserialize, Deserialize)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde")]
struct OracleBeforeRateSources {
    oracle_key: PublicKeyHandle,
    last_oracle_price: Option<Price>,
}

impl From<OracleBeforeRateSources> for PoolRateSource {
    fn from(oracle: OracleBeforeRateSources) -> Self {
        PoolRateSource::Oracle {
            key: oracle.oracle_key,
            last_price: oracle.last_oracle_price.unwrap_or(Price::from_units(0)),
        }
    }
}

impl BorshDeserialize for PoolView {
    fn deserialize_reader<R: Read>(reader: &mut R) -> near_sdk::borsh::io::Result<Self> {
        const MAX_TOTAL_SHARES_LENGTH: u64 = 17;

        let assets = BorshDeserialize::deserialize_reader(reader)?;
        let decimals = BorshDeserialize::deserialize_reader(reader)?;
        let amplification = BorshDeserialize::deserialize_reader(reader)?;
        let mut rate_source_or_oracle_key_tag = [0];
        reader.read_exact(&mut rate_source_or_oracle_key_tag)?;
        let mut reader = rate_source_or_oracle_key_tag.as_slice().chain(reader);
        if rate_source_or_oracle_key_tag[0] >= PoolRateSource::FIRST_BORSH_TAG {
            return Ok(PoolView {
                assets,
                decimals,
                amplification,
                rate_source: BorshDeserialize::deserialize_reader(&mut reader)?,
                fees: BorshDeserialize::deserialize_reader(&mut reader)?,
                total_shares: BorshDeserialize::deserialize_reader(&mut reader)?,
            });
        }
        let rate_source = OracleBeforeRateSources::deserialize_reader(&mut reader)?.into();
        let fee_fraction = FeeFraction::deserialize_reader(&mut reader)?;
        let mut bytes_after_fee_fraction = Vec::new();
        (&mut reader)
            .take(MAX_TOTAL_SHARES_LENGTH + 1)
            .read_to_end(&mut bytes_after_fee_fraction)?;
        if bytes_after_fee_fraction.len() as u64 <= MAX_TOTAL_SHARES_LENGTH {
            return Ok(PoolView {
                assets,
                decimals,
                amplification,
                rate_source,
                fees: fees_before_oracle_fees(fee_fraction),
                total_shares: TotalShares::try_from_slice(&bytes_after_fee_fraction)?,
            });
        }
        let mut rest = bytes_after_fee_fraction.as_slice().chain(reader);
        Ok(PoolView {
            assets,
            decimals,
            amplification,
            rate_source,
            fees: FeeConfiguration {
                fraction: fee_fraction,
                oracle_fee_per_swap: BorshDeserialize::deserialize_reader(&mut rest)?,
                oracle_fee_receiver: BorshDeserialize::deserialize_reader(&mut rest)?,
            },
            total_shares: TotalShares::deserialize_reader(&mut rest)?,
        })
    }
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub(crate) struct JsonPoolView {
    assets: (AssetWithBalance, AssetWithBalance),
    decimals: (u8, u8),
    amplification: Amplification,
    #[serde(flatten)]
    rate_source: JsonPoolRateSource,
    #[serde(flatten)]
    fees: JsonPoolFees,
    total_shares: TotalShares,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde", untagged)]
enum JsonPoolRateSource {
    V1(OracleBeforeRateSources),
    V2 { rate_source: PoolRateSource },
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde", untagged)]
enum JsonPoolFees {
    V1 { fee: FeeFraction },
    V2 { fees: FeeConfiguration },
}

impl From<JsonPoolView> for PoolView {
    fn from(pool: JsonPoolView) -> Self {
        PoolView {
            assets: pool.assets,
            decimals: pool.decimals,
            amplification: pool.amplification,
            rate_source: match pool.rate_source {
                JsonPoolRateSource::V1(oracle) => oracle.into(),
                JsonPoolRateSource::V2 { rate_source } => rate_source,
            },
            fees: match pool.fees {
                JsonPoolFees::V1 { fee } => fees_before_oracle_fees(fee),
                JsonPoolFees::V2 { fees } => fees,
            },
            total_shares: pool.total_shares,
        }
    }
}
