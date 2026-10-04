#![deny(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod chain;
mod commands;
mod deployment;
mod display;
mod errors;
mod inputs;
mod outcome;
mod planning;

use clap::CommandFactory;
use color_eyre::Section;
use color_eyre::eyre::{Report, eyre};
use color_eyre::owo_colors::OwoColorize;
use interactive_clap::ToCliArgs;

type ConfigContext = (near_cli_rs::config::Config,);

#[derive(Debug, Clone, interactive_clap::InteractiveClap)]
#[interactive_clap(input_context = ConfigContext)]
#[interactive_clap(output_context = CmdContext)]
struct Cmd {
    /// Print view results as JSON
    #[interactive_clap(long)]
    json: bool,
    /// Quiet mode
    #[interactive_clap(long)]
    quiet: bool,
    /// TEACH-ME mode
    #[interactive_clap(long)]
    teach_me: bool,
    #[interactive_clap(subcommand)]
    top_level: commands::TopLevelCommand,
}

#[derive(Debug, Clone)]
pub struct GlobalContext {
    pub config: near_cli_rs::config::Config,
    pub output_format: display::OutputFormat,
    pub verbosity: near_cli_rs::Verbosity,
}

fn verbosity(quiet: bool, teach_me: bool) -> near_cli_rs::Verbosity {
    if quiet {
        near_cli_rs::Verbosity::Quiet
    } else if teach_me {
        near_cli_rs::Verbosity::TeachMe
    } else {
        near_cli_rs::Verbosity::Interactive
    }
}

#[derive(Debug, Clone)]
struct CmdContext(GlobalContext);

impl CmdContext {
    fn from_previous_context(
        previous_context: ConfigContext,
        scope: &<Cmd as interactive_clap::ToInteractiveClapContextScope>::InteractiveClapContextScope,
    ) -> color_eyre::eyre::Result<Self> {
        Ok(Self(GlobalContext {
            config: previous_context.0,
            output_format: if scope.json {
                display::OutputFormat::Json
            } else {
                display::OutputFormat::Table
            },
            verbosity: verbosity(scope.quiet, scope.teach_me),
        }))
    }
}

impl From<CmdContext> for GlobalContext {
    fn from(item: CmdContext) -> Self {
        item.0
    }
}

fn print_console_command(cli_cmd: &CliCmd) {
    if !cli_cmd.quiet {
        let console_command = shell_words::join(
            ["near", "intear-dex-management"]
                .into_iter()
                .map(String::from)
                .chain(cli_cmd.to_cli_args()),
        );
        eprintln!(
            "\nHere is your console command if you need to script it or re-run:\n    {}\n",
            console_command.yellow()
        );
    }
}

/// Names where the command stopped and what comes next, instead of
/// inquire's "not a TTY"
fn missing_arguments_error(cli_cmd: Option<&CliCmd>) -> Report {
    let given_arguments: Vec<String> = cli_cmd
        .map(|cli_cmd| cli_cmd.to_cli_args().into())
        .unwrap_or_default();
    let mut next_command = CliCmd::command();
    for argument in &given_arguments {
        if let Some(subcommand) = next_command.find_subcommand(argument) {
            next_command = subcommand.clone();
        }
    }
    let usage = next_command.render_usage().to_string();
    let missing_arguments = usage
        .strip_prefix("Usage: ")
        .and_then(|usage| usage.split_once(' '))
        .map_or(usage.as_str(), |(_command_name, arguments)| arguments);
    let given_command = shell_words::join(
        ["near", "intear-dex-management"]
            .into_iter()
            .map(String::from)
            .chain(given_arguments),
    );
    let next_subcommands = next_command
        .get_subcommands()
        .map(clap::Command::get_name)
        .filter(|subcommand_name| *subcommand_name != "help")
        .collect::<Vec<_>>();
    let suggestion = if next_subcommands.is_empty() {
        format!("Add what comes next: {missing_arguments}")
    } else {
        format!(
            "Add what comes next: {missing_arguments}, where COMMAND is {}",
            next_subcommands.join(" or ")
        )
    };
    eyre!("`{given_command}` needs more arguments, and they can't be asked for because stdin isn't a terminal")
        .suggestion(suggestion)
}

fn readable_error(error: Report, cli_cmd: Option<&CliCmd>) -> Report {
    // near-cli-rs wraps errors of our callbacks in Report::msg, which hides
    // their notes and suggestions
    let error = error.downcast::<Report>().unwrap_or_else(|error| error);
    let stdin_is_not_a_terminal = error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<inquire::InquireError>(),
            Some(inquire::InquireError::NotTTY)
        )
    });
    if stdin_is_not_a_terminal {
        return missing_arguments_error(cli_cmd);
    }
    // near-cli-rs fetches the nonce of the signing key before signing
    let signing_key_is_unknown = error.chain().any(|cause| {
        let cause = cause.to_string();
        cause.contains("Access key for public key") && cause.contains("does not exist")
    });
    if signing_key_is_unknown {
        return error.suggestion(
            "The signing key isn't one of the signer's access keys. Sign with a key the account has",
        );
    }
    outcome::explain_top_level_failure(error)
}

fn main() -> near_cli_rs::CliResult {
    inquire::set_global_render_config(near_cli_rs::get_global_render_config());
    let config = near_cli_rs::config::Config::get_config_toml()?;
    color_eyre::config::HookBuilder::default()
        .display_env_section(false)
        .display_location_section(false)
        .install()?;

    let cli = match Cmd::try_parse() {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };
    // Summaries before signing and results after sending are INFO events.
    // Planning prints the transaction about to be signed with long arguments
    // shortened, in place of near-cli-rs's copy.
    near_cli_rs::setup_tracing_with_extra_directives(
        verbosity(cli.quiet, cli.teach_me),
        &[
            "near_intear_dex_management=info",
            "near_cli_rs::network_for_transaction=warn",
        ],
    )?;

    match <Cmd as interactive_clap::FromCli>::from_cli(Some(cli), (config,)) {
        interactive_clap::ResultFromCli::Ok(cli_cmd)
        | interactive_clap::ResultFromCli::Cancel(Some(cli_cmd)) => {
            print_console_command(&cli_cmd);
            Ok(())
        }
        interactive_clap::ResultFromCli::Cancel(None) => {
            eprintln!("\nGoodbye!");
            Ok(())
        }
        interactive_clap::ResultFromCli::Back => {
            unreachable!("The top-level command has no back option")
        }
        interactive_clap::ResultFromCli::Err(optional_cli_cmd, error) => {
            if let Some(cli_cmd) = &optional_cli_cmd {
                print_console_command(cli_cmd);
            }
            Err(readable_error(error, optional_cli_cmd.as_ref()))
        }
    }
}
