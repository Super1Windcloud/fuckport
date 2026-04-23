use fuckport::error::AppResult;
use fuckport::process::ProcessCatalog;

fn main() -> AppResult<()> {
    let catalog = ProcessCatalog::load()?;

    for record in catalog.process_records().into_iter().take(10) {
        let ports = if record.ports.is_empty() {
            "-".to_string()
        } else {
            record
                .ports
                .iter()
                .map(|port| port.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };

        println!(
            "{:<7} {:<24} {:<12} {}",
            record.pid.as_u32(),
            record.name,
            ports,
            record.cmd
        );
    }

    Ok(())
}
