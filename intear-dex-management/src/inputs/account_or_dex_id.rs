use std::str::FromStr;

use intear_dex_types::{AccountOrDexId, DexId};
use near_primitives::types::AccountId;

#[derive(Clone)]
pub struct AccountOrDexIdArg(pub AccountOrDexId);

impl std::fmt::Display for AccountOrDexIdArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            AccountOrDexId::Account(account_id) => write!(f, "{account_id}"),
            AccountOrDexId::Dex(dex_id) => write!(f, "{dex_id}"),
        }
    }
}

impl std::fmt::Debug for AccountOrDexIdArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

impl FromStr for AccountOrDexIdArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let account_or_dex_id = if s.contains('/') {
            DexId::from_str(s).map(AccountOrDexId::Dex)
        } else {
            AccountId::from_str(s)
                .map(AccountOrDexId::Account)
                .map_err(|error| format!("Invalid account id {s}: {error}"))
        };
        account_or_dex_id.map(Self).map_err(|error| {
            format!("{error}. Expected an account (alice.near) or a dex (slimedragon.near/xyk)")
        })
    }
}

impl interactive_clap::ToCli for AccountOrDexIdArg {
    type CliVariant = AccountOrDexIdArg;
}
