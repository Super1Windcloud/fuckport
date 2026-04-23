use std::backtrace::BacktraceStatus;

use clap::Parser;

use fuckport::cli::Cli;
use fuckport::error::AppResult;
use fuckport::input::parse_targets;
use fuckport::interactive::pick_interactive;
use fuckport::killer::{KillOptions, kill_processes};
use fuckport::process::ProcessCatalog;

fn main() {
    let cli = Cli::parse();

    if let Err(error) = run(cli) {
        report_error(&error);
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> AppResult<()> {
    let mut catalog = ProcessCatalog::load()?;
    let interactive_mode = cli.interactive || cli.targets.is_empty();

    let selected_pids = if interactive_mode {
        pick_interactive(&catalog, cli.verbose)?
    } else {
        let targets = parse_targets(&cli.targets);
        catalog.resolve_targets(&targets, cli.case_sensitive)?
    };

    if interactive_mode && selected_pids.is_empty() {
        return Ok(());
    }

    let options = KillOptions {
        force: cli.force,
        silent: cli.silent,
        force_after_timeout: cli.force_after_timeout,
        wait_for_exit: cli.wait_for_exit,
    };

    kill_processes(&mut catalog, &selected_pids, &options)
}

fn report_error(error: &anyhow::Error) {
    eprintln!("Error: {error}");

    for cause in error.chain().skip(1) {
        eprintln!("Caused by: {cause}");
    }

    let backtrace = error.backtrace();
    if matches!(
        backtrace.status(),
        BacktraceStatus::Captured | BacktraceStatus::Disabled
    ) {
        if backtrace.status() == BacktraceStatus::Captured {
            eprintln!("\nBacktrace:\n{backtrace}");
        }
    }
}
