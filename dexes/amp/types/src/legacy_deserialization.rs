use near_sdk::{
    PublicKeyHandle,
    borsh::{BorshDeserialize, io::Read},
    serde::Deserialize,
};

use crate::{
    Amplification, AssetWithBalance, FeeConfiguration, FeeFraction, PROTOCOL_FEE_RECEIVER_ID,
    PoolView, Price, TokenAmount, TotalShares,
};

/// The fees of a pool from before oracle fees, as the migration sets them
fn fees_before_oracle_fees(fraction: FeeFraction) -> FeeConfiguration {
    FeeConfiguration {
        fraction,
        oracle_fee_per_swap: (TokenAmount::ZERO, TokenAmount::ZERO),
        oracle_fee_receiver: PROTOCOL_FEE_RECEIVER_ID.to_owned(),
    }
}

#[derive(BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
struct BorshPoolViewUntilFees {
    assets: (AssetWithBalance, AssetWithBalance),
    decimals: (u8, u8),
    amplification: Amplification,
    oracle_key: PublicKeyHandle,
    last_oracle_price: Option<Price>,
    fee_fraction: FeeFraction,
}

impl BorshDeserialize for PoolView {
    fn deserialize_reader<R: Read>(reader: &mut R) -> near_sdk::borsh::io::Result<Self> {
        const MAX_TOTAL_SHARES_LENGTH: u64 = 17;

        let BorshPoolViewUntilFees {
            assets,
            decimals,
            amplification,
            oracle_key,
            last_oracle_price,
            fee_fraction,
        } = BorshPoolViewUntilFees::deserialize_reader(reader)?;
        let mut bytes_after_fee_fraction = Vec::new();
        reader
            .take(MAX_TOTAL_SHARES_LENGTH + 1)
            .read_to_end(&mut bytes_after_fee_fraction)?;
        if bytes_after_fee_fraction.len() as u64 <= MAX_TOTAL_SHARES_LENGTH {
            return Ok(PoolView {
                assets,
                decimals,
                amplification,
                oracle_key,
                last_oracle_price,
                fees: fees_before_oracle_fees(fee_fraction),
                total_shares: TotalShares::try_from_slice(&bytes_after_fee_fraction)?,
            });
        }
        let mut rest = bytes_after_fee_fraction.as_slice().chain(reader);
        Ok(PoolView {
            assets,
            decimals,
            amplification,
            oracle_key,
            last_oracle_price,
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
    oracle_key: PublicKeyHandle,
    last_oracle_price: Option<Price>,
    #[serde(flatten)]
    fees: JsonPoolFees,
    total_shares: TotalShares,
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
            oracle_key: pool.oracle_key,
            last_oracle_price: pool.last_oracle_price,
            fees: match pool.fees {
                JsonPoolFees::V1 { fee } => fees_before_oracle_fees(fee),
                JsonPoolFees::V2 { fees } => fees,
            },
            total_shares: pool.total_shares,
        }
    }
}
