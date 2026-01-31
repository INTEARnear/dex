#![deny(clippy::arithmetic_side_effects)]

use std::{collections::HashMap, fmt, fmt::Display, str::FromStr};

#[cfg(feature = "json")]
use near_sdk::serde::{Deserialize, Deserializer, Serialize, Serializer};
use near_sdk::{
    AccountId, NearToken,
    json_types::{Base64VecU8, U128},
    near,
};

/// Request for a swap operation.
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[cfg_attr(not(feature = "json"), near(serializers=[borsh]))]
#[cfg_attr(feature = "json", near(serializers=[borsh, json]))]
pub struct SwapRequest {
    /// Custom message to be passed to the dex. For example,
    /// it could be the pool ID or route.
    pub message: Base64VecU8,
    /// The asset the user has requested to be swapped in.
    pub asset_in: AssetId,
    /// The asset the user has requested to be swapped out.
    pub asset_out: AssetId,
    /// The amount of the asset the user has requested to be
    /// swapped. The response `amount_in` or `amount_out`
    /// must match this amount.
    pub amount: SwapRequestAmount,
}

/// The swap operation was successful, release `amount_out`
/// to the user and take `amount_in` from the user.
///
/// To mark operation as unsuccessful and refund the attached
/// assets to the user, the dex must panic.
#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[borsh])]
pub struct SwapResponse {
    pub amount_in: U128,
    pub amount_out: U128,
}

#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[borsh])]
pub struct DexCallRequest {
    pub attached_assets: HashMap<AssetId, U128>,
    pub args: Vec<u8>,
}

#[derive(Clone, Default)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[borsh])]
pub struct DexCallResponse {
    pub asset_withdraw_requests: Vec<AssetWithdrawRequest>,
    pub add_storage_deposit: NearToken,
    pub response: Vec<u8>,
}

#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[borsh])]
pub struct AssetWithdrawRequest {
    pub asset_id: AssetId,
    pub amount: U128,
    pub withdrawal_type: AssetWithdrawalType,
}

#[derive(Clone)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[borsh])]
pub enum AssetWithdrawalType {
    ToInternalUserBalance(AccountId),
    ToInternalDexBalance(DexId),
    WithdrawUnderlyingAsset(AccountId),
}

#[derive(PartialEq, Eq, Hash, Clone, PartialOrd, Ord, Debug)]
#[near(serializers=[borsh])]
pub enum AssetId {
    Near,
    Nep141(AccountId),
    Nep245(AccountId, String),
    Nep171(AccountId, String),
}

impl Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Near => write!(f, "near"),
            Self::Nep141(contract_id) => write!(f, "nep141:{contract_id}"),
            Self::Nep245(contract_id, token_id) => write!(f, "nep245:{contract_id}:{token_id}"),
            Self::Nep171(contract_id, token_id) => write!(f, "nep171:{contract_id}:{token_id}"),
        }
    }
}

impl FromStr for AssetId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "near" => Ok(Self::Near),
            _ => match s.split_once(':') {
                Some(("nep141", contract_id)) => {
                    Ok(Self::Nep141(contract_id.parse().map_err(|e| {
                        format!("Invalid account id {contract_id}: {e}")
                    })?))
                }
                Some(("nep245", rest)) => {
                    if let Some((contract_id, token_id)) = rest.split_once(':') {
                        Ok(Self::Nep245(
                            contract_id
                                .parse()
                                .map_err(|e| format!("Invalid account id {contract_id}: {e}"))?,
                            token_id.to_string(),
                        ))
                    } else {
                        Err(format!("Invalid asset id: {s}"))
                    }
                }
                Some(("nep171", rest)) => {
                    if let Some((contract_id, token_id)) = rest.split_once(':') {
                        Ok(Self::Nep171(
                            contract_id
                                .parse()
                                .map_err(|e| format!("Invalid account id {contract_id}: {e}"))?,
                            token_id.to_string(),
                        ))
                    } else {
                        Err(format!("Invalid asset id: {s}"))
                    }
                }
                _ => Err(format!("Invalid asset id: {s}")),
            },
        }
    }
}

#[cfg(feature = "json")]
impl Serialize for AssetId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_string().serialize(serializer)
    }
}

#[cfg(feature = "json")]
impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_str(&s).unwrap())
    }
}

#[cfg(feature = "abi")]
impl near_sdk::schemars::JsonSchema for AssetId {
    fn schema_name() -> String {
        "AssetId".to_string()
    }
    fn json_schema(
        generator: &mut near_sdk::schemars::r#gen::SchemaGenerator,
    ) -> near_sdk::schemars::schema::Schema {
        <String as near_sdk::schemars::JsonSchema>::json_schema(generator)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[cfg_attr(not(feature = "json"), near(serializers=[borsh]))]
#[cfg_attr(feature = "json", near(serializers=[borsh, json]))]
pub enum SwapRequestAmount {
    ExactIn(U128),
    ExactOut(U128),
}

#[derive(PartialEq, Eq, Hash, Clone, PartialOrd, Ord)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[near(serializers=[borsh])]
pub struct DexId {
    pub deployer: AccountId,
    pub id: String,
}

impl Display for DexId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.deployer, self.id)
    }
}

impl FromStr for DexId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (deployer, id) = s.split_once('/').ok_or(format!("Invalid dex id: {s}"))?;
        Ok(Self {
            deployer: deployer
                .parse()
                .map_err(|e| format!("Invalid deployer id {deployer}: {e}"))?,
            id: id.to_string(),
        })
    }
}

#[cfg(feature = "json")]
impl Serialize for DexId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_string().serialize(serializer)
    }
}

#[cfg(feature = "json")]
impl<'de> Deserialize<'de> for DexId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_str(&s).unwrap())
    }
}

#[cfg(feature = "abi")]
impl near_sdk::schemars::JsonSchema for DexId {
    fn schema_name() -> String {
        "DexId".to_string()
    }
    fn json_schema(
        generator: &mut near_sdk::schemars::r#gen::SchemaGenerator,
    ) -> near_sdk::schemars::schema::Schema {
        <String as near_sdk::schemars::JsonSchema>::json_schema(generator)
    }
}

pub trait Dex {
    fn swap(&mut self, request: SwapRequest) -> SwapResponse;
}

#[macro_export]
macro_rules! expect {
    ($condition:expr, $message:literal $(, $fmt_args:expr)* $(,)?) => {
        if !$condition {
            ::near_sdk::env::panic_str(&::std::format!($message $(, $fmt_args)*));
        }
    };
    ($(let $pat:pat = $expr:expr),+, $message:literal $(, $fmt_args:expr)* $(,)?) => {
        let _panic_msg = ::std::format!($message $(, $fmt_args)*);
        $(
            let $pat = $expr else {
                ::near_sdk::env::panic_str(&_panic_msg);
            };
        )+;
    };
}
