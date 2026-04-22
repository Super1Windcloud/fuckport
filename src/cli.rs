use clap::{ArgAction, Parser};

#[derive(Debug, Parser)]
#[command(
    name = "fuckport",
    version,
    about = "Kill processes by PID, name, or port."
)]
pub struct Cli {
    /// One or more targets: PID (`1234`), name (`chrome`), or port (`:3000`)
    #[arg(value_name = "TARGET")]
    pub targets: Vec<String>,

    /// Force kill immediately
    #[arg(short, long, action = ArgAction::SetTrue)]
    pub force: bool,

    /// Case-sensitive name matching
    #[arg(short = 'c', long, action = ArgAction::SetTrue)]
    pub case_sensitive: bool,

    /// Suppress per-process success output
    #[arg(short, long, action = ArgAction::SetTrue)]
    pub silent: bool,

    /// Show extra process details
    #[arg(short, long, action = ArgAction::SetTrue)]
    pub verbose: bool,

    /// Open interactive selector. Also implied when no targets are passed.
    #[arg(short, long, action = ArgAction::SetTrue)]
    pub interactive: bool,

    /// Milliseconds to wait before escalating to force kill
    #[arg(long, default_value_t = 1500, visible_alias = "wait-ms")]
    pub force_after_timeout: u64,

    /// Milliseconds to wait for the process to disappear before reporting failure
    #[arg(long, default_value_t = 5000)]
    pub wait_for_exit: u64,
}
