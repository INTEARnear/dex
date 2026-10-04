use std::str::FromStr;

use super::percent::PercentArg;

/// How much of a liquidity position to remove
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareArg {
    All,
    /// Of a private pool, or of the signer's shares of a public pool
    Percent(PercentArg),
    RawShares(u128),
}

impl std::fmt::Display for ShareArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::All => write!(f, "all"),
            Self::Percent(percent) => write!(f, "{percent}"),
            Self::RawShares(shares) => write!(f, "{shares} raw-shares"),
        }
    }
}

impl FromStr for ShareArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "all" {
            return Ok(Self::All);
        }
        if s.ends_with('%') {
            return s.parse().map(Self::Percent);
        }
        match s.split_whitespace().collect::<Vec<_>>().as_slice() {
            [shares, "raw-shares"] => shares
                .parse()
                .map(Self::RawShares)
                .map_err(|_| format!("Invalid number of shares '{shares}'")),
            _ => Err(format!(
                "Invalid share '{s}'. Expected a percentage like 50%, all, or '<integer> raw-shares'"
            )),
        }
    }
}

impl interactive_clap::ToCli for ShareArg {
    type CliVariant = ShareArg;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_are_percentages_all_or_raw() {
        assert_eq!("all".parse(), Ok(ShareArg::All));
        assert_eq!("50%".parse(), Ok(ShareArg::Percent(PercentArg(500_000))));
        assert_eq!("12 raw-shares".parse(), Ok(ShareArg::RawShares(12)));
        assert!("12".parse::<ShareArg>().is_err());
    }
}
