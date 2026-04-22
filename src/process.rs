use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, get_sockets_info};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::error::AppResult;
use crate::input::Target;

#[derive(Clone, Debug)]
pub struct ProcessRecord {
    pub pid: Pid,
    pub name: String,
    pub cmd: String,
    pub ports: BTreeSet<u16>,
}

pub struct ProcessCatalog {
    system: System,
    pids_by_port: BTreeMap<u16, BTreeSet<Pid>>,
    ports_by_pid: BTreeMap<Pid, BTreeSet<u16>>,
    current_pid: Pid,
}

impl ProcessCatalog {
    pub fn load() -> AppResult<Self> {
        let mut system = System::new_all();
        refresh_processes(&mut system);

        let pids_by_port = port_map()?;
        let ports_by_pid = reverse_port_map(&pids_by_port);
        let current_pid = sysinfo::get_current_pid()
            .map_err(|error| format!("failed to read current pid: {error}"))?;

        Ok(Self {
            system,
            pids_by_port,
            ports_by_pid,
            current_pid,
        })
    }

    pub fn refresh(&mut self) {
        refresh_processes(&mut self.system);
    }

    pub fn system(&self) -> &System {
        &self.system
    }

    pub fn current_pid(&self) -> Pid {
        self.current_pid
    }

    pub fn process_records(&self) -> Vec<ProcessRecord> {
        let mut records = self
            .system
            .processes()
            .values()
            .map(|process| ProcessRecord {
                pid: process.pid(),
                name: process.name().to_string_lossy().into_owned(),
                cmd: join_cmd(process.cmd()),
                ports: self
                    .ports_by_pid
                    .get(&process.pid())
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect::<Vec<_>>();

        records.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.pid.as_u32().cmp(&right.pid.as_u32()))
        });
        records
    }

    pub fn resolve_targets(
        &self,
        targets: &[Target],
        case_sensitive: bool,
    ) -> AppResult<BTreeSet<Pid>> {
        let mut matches = BTreeSet::new();

        for target in targets {
            match target {
                Target::Pid(pid) => {
                    if self.system.process(*pid).is_some() {
                        matches.insert(*pid);
                    }
                }
                Target::Port(port) => {
                    if let Some(pids) = self.pids_by_port.get(port) {
                        matches.extend(pids.iter().copied());
                    }
                }
                Target::Name(name) => {
                    matches.extend(self.match_by_name(name, case_sensitive));
                }
            }
        }

        matches.remove(&self.current_pid);

        if matches.is_empty() {
            return Err("no matching processes found".to_string());
        }

        Ok(matches)
    }

    fn match_by_name(&self, needle: &str, case_sensitive: bool) -> BTreeSet<Pid> {
        self.system
            .processes()
            .values()
            .filter_map(|process| {
                if process.pid() == self.current_pid {
                    return None;
                }

                let name = process.name().to_string_lossy();
                let cmd = join_cmd(process.cmd());
                if name_matches(&name, &cmd, needle, case_sensitive) {
                    Some(process.pid())
                } else {
                    None
                }
            })
            .collect()
    }
}

fn refresh_processes(system: &mut System) {
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::OnlyIfNotSet),
    );
}

fn join_cmd(parts: &[OsString]) -> String {
    parts
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

fn port_map() -> AppResult<BTreeMap<u16, BTreeSet<Pid>>> {
    let sockets = get_sockets_info(
        AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6,
        ProtocolFlags::TCP | ProtocolFlags::UDP,
    )
    .map_err(|error| format!("failed to enumerate sockets: {error}"))?;

    let mut result = BTreeMap::<u16, BTreeSet<Pid>>::new();
    for socket in sockets {
        let port = match socket.protocol_socket_info {
            ProtocolSocketInfo::Tcp(tcp) => tcp.local_port,
            ProtocolSocketInfo::Udp(udp) => udp.local_port,
        };

        for pid in socket.associated_pids {
            result.entry(port).or_default().insert(Pid::from_u32(pid));
        }
    }

    Ok(result)
}

fn reverse_port_map(port_map: &BTreeMap<u16, BTreeSet<Pid>>) -> BTreeMap<Pid, BTreeSet<u16>> {
    let mut result = BTreeMap::<Pid, BTreeSet<u16>>::new();
    for (port, pids) in port_map {
        for pid in pids {
            result.entry(*pid).or_default().insert(*port);
        }
    }
    result
}

fn name_matches(name: &str, cmd: &str, needle: &str, case_sensitive: bool) -> bool {
    let smart_case = case_sensitive || needle.chars().any(|char| char.is_uppercase());
    let query = if smart_case {
        needle.to_string()
    } else {
        needle.to_lowercase()
    };
    let haystack_name = if smart_case {
        name.to_string()
    } else {
        name.to_lowercase()
    };
    let haystack_cmd = if smart_case {
        cmd.to_string()
    } else {
        cmd.to_lowercase()
    };

    haystack_name.contains(&query) || haystack_cmd.contains(&query)
}

#[cfg(test)]
mod tests {
    use super::name_matches;

    #[test]
    fn name_matching_is_case_insensitive_by_default() {
        assert!(name_matches("node", "node server.js", "node", false));
    }

    #[test]
    fn explicit_case_sensitive_matching_respects_case() {
        assert!(name_matches("Node", "Node server.js", "Node", true));
        assert!(!name_matches("node", "node server.js", "Node", true));
    }

    #[test]
    fn smart_case_becomes_sensitive_for_uppercase_queries() {
        assert!(name_matches("MyApp", "MyApp --watch", "MyA", false));
        assert!(!name_matches("myapp", "myapp --watch", "MyA", false));
    }

    #[test]
    fn command_line_is_part_of_the_search_space() {
        assert!(name_matches(
            "python",
            "python -m http.server 8000",
            "http.server",
            false
        ));
    }
}
