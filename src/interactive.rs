use std::collections::BTreeSet;

use dialoguer::{MultiSelect, theme::ColorfulTheme};
use sysinfo::Pid;

use crate::error::AppResult;
use crate::process::{ProcessCatalog, ProcessRecord};

pub fn pick_interactive(catalog: &ProcessCatalog, verbose: bool) -> AppResult<BTreeSet<Pid>> {
    let records = catalog.process_records();
    if records.is_empty() {
        return Ok(BTreeSet::new());
    }

    let items = records
        .iter()
        .map(|record| format_record(record, verbose))
        .collect::<Vec<_>>();

    let selections = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select processes to kill")
        .items(&items)
        .interact_opt()
        .map_err(|error| format!("interactive selection failed: {error}"))?;

    Ok(selections
        .unwrap_or_default()
        .into_iter()
        .map(|index| records[index].pid)
        .collect())
}

fn format_record(record: &ProcessRecord, verbose: bool) -> String {
    let ports = record
        .ports
        .iter()
        .map(|port| format!(":{port}"))
        .collect::<Vec<_>>()
        .join(",");
    let ports = if ports.is_empty() {
        "-".to_string()
    } else {
        ports
    };

    if verbose && !record.cmd.is_empty() {
        return format!(
            "{:<7} {:<24} {:<14} {}",
            record.pid.as_u32(),
            truncate(&record.name, 24),
            ports,
            record.cmd
        );
    }

    format!(
        "{:<7} {:<24} {}",
        record.pid.as_u32(),
        truncate(&record.name, 24),
        ports
    )
}

fn truncate(value: &str, width: usize) -> String {
    let mut result = value.chars().take(width).collect::<String>();
    if value.chars().count() > width && width > 1 {
        result.pop();
        result.push('~');
    }
    result
}
