use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::path::Path;
use std::thread;

use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, get_sockets_info};
use sysinfo::{MINIMUM_CPU_UPDATE_INTERVAL, Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};

use crate::error::AppResult;
use crate::input::Target;

#[derive(Clone, Debug)]
pub struct ProcessRecord {
    pub pid: Pid,
    pub app_name: String,
    pub name: String,
    pub cmd: String,
    pub cpu_usage: f32,
    pub memory_bytes: u64,
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
                app_name: app_name_for_process(process),
                name: process.name().to_string_lossy().into_owned(),
                cmd: join_cmd(process.cmd()),
                cpu_usage: process.cpu_usage(),
                memory_bytes: process.memory(),
                ports: self
                    .ports_by_pid
                    .get(&process.pid())
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect::<Vec<_>>();

        records.sort_by(|left, right| {
            right
                .cpu_usage
                .partial_cmp(&left.cpu_usage)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.app_name.cmp(&right.app_name))
                .then(left.name.cmp(&right.name))
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
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_cpu(),
    );
    thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_cpu(),
    );
}

fn join_cmd(parts: &[OsString]) -> String {
    parts
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

fn app_name_for_process(process: &sysinfo::Process) -> String {
    if let Some(exe) = process.exe() {
        #[cfg(windows)]
        if let Some(description) = windows_file_description(exe) {
            return description;
        }

        if let Some(name) = file_name_value(exe) {
            return name;
        }
    }

    let process_name = process.name().to_string_lossy().trim().to_string();
    if !process_name.is_empty() {
        return process_name;
    }

    String::from("<unknown>")
}

fn file_name_value(path: &std::path::Path) -> Option<String> {
    if let Some(name) = path.file_name() {
        let value = name.to_string_lossy().trim().to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }

    path.file_stem().and_then(|stem| {
        let value = stem.to_string_lossy().trim().to_string();
        if value.is_empty() { None } else { Some(value) }
    })
}

#[cfg(windows)]
fn windows_file_description(path: &Path) -> Option<String> {
    let mut wide_path = path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide_path.push(0);

    let mut handle = 0;
    let size = unsafe { GetFileVersionInfoSizeW(wide_path.as_ptr(), &mut handle) };
    if size == 0 {
        return None;
    }

    let mut buffer = vec![0_u8; size as usize];
    let loaded = unsafe {
        GetFileVersionInfoW(wide_path.as_ptr(), 0, size, buffer.as_mut_ptr().cast::<c_void>())
    };
    if loaded == 0 {
        return None;
    }

    let mut queries = version_translation_queries(&buffer);
    queries.push(wide_query(r"\StringFileInfo\040904b0\FileDescription"));
    queries.push(wide_query(r"\StringFileInfo\040904e4\FileDescription"));

    for query in queries {
        if let Some(value) = query_version_value(&buffer, &query) {
            return Some(value);
        }
    }

    None
}

#[cfg(windows)]
fn version_translation_queries(buffer: &[u8]) -> Vec<Vec<u16>> {
    let mut pointer = std::ptr::null_mut::<c_void>();
    let mut length = 0_u32;
    let query = wide_query(r"\VarFileInfo\Translation");

    let found = unsafe {
        VerQueryValueW(
            buffer.as_ptr().cast::<c_void>(),
            query.as_ptr(),
            &mut pointer,
            &mut length,
        )
    };
    if found == 0 || pointer.is_null() || length < 4 {
        return Vec::new();
    }

    let translations = unsafe {
        std::slice::from_raw_parts(pointer.cast::<u16>(), (length as usize) / 2)
    };

    translations
        .chunks_exact(2)
        .map(|chunk| format!(r"\StringFileInfo\{:04x}{:04x}\FileDescription", chunk[0], chunk[1]))
        .map(|query| wide_query(&query))
        .collect()
}

#[cfg(windows)]
fn query_version_value(buffer: &[u8], query: &[u16]) -> Option<String> {
    let mut pointer = std::ptr::null_mut::<c_void>();
    let mut length = 0_u32;
    let found = unsafe {
        VerQueryValueW(
            buffer.as_ptr().cast::<c_void>(),
            query.as_ptr(),
            &mut pointer,
            &mut length,
        )
    };
    if found == 0 || pointer.is_null() || length == 0 {
        return None;
    }

    let text = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), length as usize) };
    let value = String::from_utf16_lossy(text)
        .trim_end_matches('\0')
        .trim()
        .to_string();

    if value.is_empty() { None } else { Some(value) }
}

#[cfg(windows)]
fn wide_query(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
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
