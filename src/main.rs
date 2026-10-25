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
    let interactive_mode = should_use_interactive(&cli);

    let selection = if interactive_mode {
        pick_interactive(&catalog, cli.verbose)
    } else {
        let mut targets = parse_targets(&cli.targets);
        targets.extend(
            cli.directories
                .iter()
                .cloned()
                .map(fuckport::input::Target::Directory),
        );
        targets.extend(cli.files.iter().cloned().map(fuckport::input::Target::File));
        catalog.resolve_targets(&targets, cli.case_sensitive)
    };
    for warning in catalog.discovery_warnings() {
        eprintln!("Warning: {warning}");
    }
    let selected_pids = selection?;

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

fn should_use_interactive(cli: &Cli) -> bool {
    cli.interactive
        || (cli.targets.is_empty() && cli.directories.is_empty() && cli.files.is_empty())
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
    ) && backtrace.status() == BacktraceStatus::Captured
    {
        eprintln!("\nBacktrace:\n{backtrace}");
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fuckport::cli::Cli;

    use super::should_use_interactive;

    fn cli(
        targets: Vec<String>,
        directories: Vec<PathBuf>,
        files: Vec<PathBuf>,
        interactive: bool,
    ) -> Cli {
        Cli {
            targets,
            force: false,
            case_sensitive: false,
            directories,
            files,
            silent: false,
            verbose: false,
            interactive,
            force_after_timeout: 1_500,
            wait_for_exit: 5_000,
        }
    }

    #[test]
    fn defaults_to_interactive_without_targets_or_directories() {
        assert!(should_use_interactive(&cli(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            false,
        )));
    }

    #[test]
    fn directory_target_disables_default_interactive_mode() {
        assert!(!should_use_interactive(&cli(
            Vec::new(),
            vec![PathBuf::from(".")],
            Vec::new(),
            false,
        )));
    }

    #[test]
    fn file_target_disables_default_interactive_mode() {
        assert!(!should_use_interactive(&cli(
            Vec::new(),
            Vec::new(),
            vec![PathBuf::from("Cargo.toml")],
            false,
        )));
    }

    #[test]
    fn explicit_interactive_overrides_directory_targets() {
        assert!(should_use_interactive(&cli(
            Vec::new(),
            vec![PathBuf::from(".")],
            Vec::new(),
            true,
        )));
    }
}
