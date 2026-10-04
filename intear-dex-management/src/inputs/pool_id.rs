use std::str::FromStr;

use xyk_dex_types::PoolId;

#[derive(Debug, Clone, Copy)]
pub struct PoolIdArg(pub PoolId);

impl std::fmt::Display for PoolIdArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for PoolIdArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PoolId::from_str(s)
            .map(Self)
            .map_err(|_| format!("Invalid pool id {s}: expected a pool number, e.g. 42"))
    }
}

impl interactive_clap::ToCli for PoolIdArg {
    type CliVariant = PoolIdArg;
}

/// Comma-separated pool ids
#[derive(Debug, Clone)]
pub struct PoolIdListArg(pub Vec<PoolId>);

impl std::fmt::Display for PoolIdListArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pool_ids = self.0.iter().map(PoolId::to_string).collect::<Vec<_>>();
        write!(f, "{}", pool_ids.join(","))
    }
}

impl FromStr for PoolIdListArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut pool_ids = Vec::new();
        for pool_id in s.split(',') {
            let PoolIdArg(pool_id) = PoolIdArg::from_str(pool_id.trim())?;
            if pool_ids.contains(&pool_id) {
                return Err(format!("Pool {pool_id} is listed twice"));
            }
            pool_ids.push(pool_id);
        }
        Ok(Self(pool_ids))
    }
}

impl interactive_clap::ToCli for PoolIdListArg {
    type CliVariant = PoolIdListArg;
}
