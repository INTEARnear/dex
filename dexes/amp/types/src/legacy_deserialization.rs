use near_sdk::{
    AccountId, PublicKeyHandle,
    borsh::{BorshDeserialize, io::Read},
    serde::Deserialize,
};

use crate::{
    Amplification, AssetWithBalance, FeeFraction, PoolRateSource, PoolView, Price, TokenAmount,
    TotalShares,
};

/// The fees of a pool while swaps paid a fixed fee to its oracle, before the engine took the fees
/// of oracles
#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct FeesWithOracleFee {
    fraction: FeeFraction,
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
        let rate_source = if rate_source_or_oracle_key_tag[0] >= PoolRateSource::FIRST_BORSH_TAG {
            PoolRateSource::deserialize_reader(&mut reader)?
        } else {
            OracleBeforeRateSources::deserialize_reader(&mut reader)?.into()
        };
        let fee = FeeFraction::deserialize_reader(&mut reader)?;
        let mut bytes_after_fee = Vec::new();
        (&mut reader)
            .take(MAX_TOTAL_SHARES_LENGTH + 1)
            .read_to_end(&mut bytes_after_fee)?;
        let total_shares = if bytes_after_fee.len() as u64 <= MAX_TOTAL_SHARES_LENGTH {
            TotalShares::try_from_slice(&bytes_after_fee)?
        } else {
            // The fixed fee of the pool's oracle and its receiver
            let mut rest = bytes_after_fee.as_slice().chain(reader);
            <((TokenAmount, TokenAmount), AccountId)>::deserialize_reader(&mut rest)?;
            TotalShares::deserialize_reader(&mut rest)?
        };
        Ok(PoolView {
            assets,
            decimals,
            amplification,
            rate_source,
            fee,
            total_shares,
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
    fee: JsonPoolFee,
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
enum JsonPoolFee {
    Fee { fee: FeeFraction },
    WithOracleFee { fees: FeesWithOracleFee },
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
            fee: match pool.fee {
                JsonPoolFee::Fee { fee } => fee,
                JsonPoolFee::WithOracleFee { fees } => fees.fraction,
            },
            total_shares: pool.total_shares,
        }
    }
}
