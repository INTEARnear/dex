pub mod account_or_dex_id;
pub mod amount;
pub mod asset_amounts;
pub mod asset_ids;
pub mod dex_id;
pub mod fees;
pub mod funds;
pub mod near_amount;
pub mod percent;
pub mod pool_id;
pub mod share;

use std::str::FromStr;

use color_eyre::eyre::eyre;

/// Asks for a value until it parses, showing the parse error under the input.
/// `None` when the user cancels.
pub fn prompt<Value: FromStr<Err = String> + 'static>(
    message: &str,
) -> color_eyre::eyre::Result<Option<Value>> {
    let validator = |input: &str| {
        Ok(match Value::from_str(input) {
            Ok(_) => inquire::validator::Validation::Valid,
            Err(error) => inquire::validator::Validation::Invalid(error.into()),
        })
    };
    match inquire::Text::new(message)
        .with_validator(validator)
        .prompt()
    {
        Ok(input) => Value::from_str(&input)
            .map(Some)
            .map_err(|error| eyre!(error)),
        Err(
            inquire::error::InquireError::OperationCanceled
            | inquire::error::InquireError::OperationInterrupted,
        ) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Asks to pick one of `options`. `None` when the user cancels.
pub fn select<Value: std::fmt::Display>(
    message: &str,
    options: Vec<Value>,
) -> color_eyre::eyre::Result<Option<Value>> {
    match inquire::Select::new(message, options).prompt() {
        Ok(choice) => Ok(Some(choice)),
        Err(
            inquire::error::InquireError::OperationCanceled
            | inquire::error::InquireError::OperationInterrupted,
        ) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
