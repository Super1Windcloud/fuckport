use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::mem::size_of;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::path::Path;
use std::path::{Path as StdPath, PathBuf};
use std::thread;

use anyhow::{Context, bail};
use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, get_sockets_info};
use sysinfo::{
    MINIMUM_CPU_UPDATE_INTERVAL, Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_NAME_NORMALIZED, FILE_TYPE_DISK, GetFileType, GetFileVersionInfoSizeW,
    GetFileVersionInfoW, GetFinalPathNameByHandleW, VOLUME_NAME_DOS, VerQueryValueW,
};
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, STATUS_INFO_LENGTH_MISMATCH,
        STATUS_SUCCESS,
    },
    System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE},
};

#[cfg(windows)]
const SYSTEM_EXTENDED_HANDLE_INFORMATION: u32 = 64;

#[cfg(windows)]
unsafe extern "system" {
    fn NtQuerySystemInformation(
        system_information_class: u32,
        system_information: *mut c_void,
        system_information_length: u32,
        return_length: *mut u32,
    ) -> i32;
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct SystemHandleTableEntryInfoEx {
    object: *mut c_void,
    unique_process_id: usize,
    handle_value: usize,
    granted_access: u32,
    creator_back_trace_index: u16,
    object_type_index: u16,
    handle_attributes: u32,
    reserved: u32,
}

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
    port_error: Option<String>,
    pub(crate) initial_start_times: BTreeMap<Pid, u64>,
}

impl ProcessCatalog {
    pub fn load() -> AppResult<Self> {
        let mut system = System::new_all();
        refresh_processes(&mut system);

        let (pids_by_port, port_error) = match port_map() {
            Ok(map) => (map, None),
            Err(err) => (BTreeMap::new(), Some(err.to_string())),
        };
        let ports_by_pid = reverse_port_map(&pids_by_port);
        let current_pid = sysinfo::get_current_pid()
            .map_err(anyhow::Error::msg)
            .context("failed to read current pid")?;

        let mut initial_start_times = BTreeMap::new();
        for (pid, process) in system.processes() {
            initial_start_times.insert(*pid, process.start_time());
        }

        Ok(Self {
            system,
            pids_by_port,
            ports_by_pid,
            current_pid,
            port_error,
            initial_start_times,
        })
    }

    pub fn refresh(&mut self) {
        refresh_processes(&mut self.system);
    }

    pub fn refresh_pid(&mut self, pid: Pid) -> bool {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        self.system.process(pid).is_some()
    }

    pub fn initial_start_time(&self, pid: Pid) -> Option<u64> {
        self.initial_start_times.get(&pid).copied()
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
                    } else if let Some(ref err) = self.port_error {
                        bail!(
                            "cannot resolve port :{port} because socket enumeration failed: {err}"
                        );
                    }
                }
                Target::Name(name) => {
                    matches.extend(self.match_by_name(name, case_sensitive));
                }
                Target::Directory(directory) => {
                    matches.extend(self.match_by_directory(directory)?);
                }
                Target::File(file) => {
                    matches.extend(self.match_by_file(file)?);
                }
                Target::InvalidPort { input, reason } => {
                    bail!("invalid port target '{input}': {reason}");
                }
            }
        }

        matches.retain(|pid| is_killable_target(*pid, self.current_pid));

        if matches.is_empty() {
            bail!("no matching processes found");
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

    fn match_by_directory(&self, directory: &StdPath) -> AppResult<BTreeSet<Pid>> {
        let directory = canonical_directory(directory)?;
        let mut matches = directory_handle_owners(&directory)?;
        matches.extend(self.match_by_cwd(&directory));
        Ok(matches)
    }

    fn match_by_file(&self, file: &StdPath) -> AppResult<BTreeSet<Pid>> {
        let file = canonical_file(file)?;
        file_handle_owners(&file)
    }

    fn match_by_cwd(&self, directory: &StdPath) -> BTreeSet<Pid> {
        self.system
            .processes()
            .values()
            .filter_map(|process| {
                if process.pid() == self.current_pid {
                    return None;
                }

                let cwd = process.cwd()?;
                if cwd_matches_directory(cwd, directory) {
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
            .with_cwd(UpdateKind::OnlyIfNotSet)
            .with_cpu(),
    );
    thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_cwd(UpdateKind::OnlyIfNotSet)
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

fn file_name_value(path: &StdPath) -> Option<String> {
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
        GetFileVersionInfoW(
            wide_path.as_ptr(),
            0,
            size,
            buffer.as_mut_ptr().cast::<c_void>(),
        )
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

    let translations =
        unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), (length as usize) / 2) };

    translations
        .chunks_exact(2)
        .map(|chunk| {
            format!(
                r"\StringFileInfo\{:04x}{:04x}\FileDescription",
                chunk[0], chunk[1]
            )
        })
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
    .context("failed to enumerate sockets")?;

    let mut result = BTreeMap::<u16, BTreeSet<Pid>>::new();
    for socket in sockets {
        let port = match socket.protocol_socket_info {
            ProtocolSocketInfo::Tcp(tcp) => tcp.local_port,
            ProtocolSocketInfo::Udp(udp) => udp.local_port,
        };

        for pid in socket.associated_pids {
            if pid == 0 {
                continue;
            }

            result.entry(port).or_default().insert(Pid::from_u32(pid));
        }
    }

    Ok(result)
}

fn is_killable_target(pid: Pid, current_pid: Pid) -> bool {
    pid.as_u32() != 0 && pid != current_pid
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

fn canonical_directory(directory: &StdPath) -> AppResult<PathBuf> {
    let path = directory
        .canonicalize()
        .with_context(|| format!("failed to resolve directory {}", directory.display()))?;

    if !path.is_dir() {
        bail!("{} is not a directory", directory.display());
    }

    Ok(path)
}

fn canonical_file(file: &StdPath) -> AppResult<PathBuf> {
    let path = file
        .canonicalize()
        .with_context(|| format!("failed to resolve file {}", file.display()))?;

    if !path.is_file() {
        bail!("{} is not a file", file.display());
    }

    Ok(path)
}

fn cwd_matches_directory(cwd: &StdPath, directory: &StdPath) -> bool {
    cwd.canonicalize()
        .map(|cwd| cwd == directory || cwd.starts_with(directory))
        .unwrap_or(false)
}

#[cfg(windows)]
fn directory_handle_owners(directory: &StdPath) -> AppResult<BTreeSet<Pid>> {
    let mut matches = BTreeSet::new();

    for handle in system_handles()? {
        let process_id = handle.unique_process_id as u32;
        if process_id == 0 {
            continue;
        }

        if let Some(path) = duplicated_handle_path(process_id, handle.handle_value) {
            if path_matches_directory(&path, directory) {
                matches.insert(Pid::from_u32(process_id));
            }
        }
    }

    Ok(matches)
}

#[cfg(unix)]
fn directory_handle_owners(directory: &StdPath) -> AppResult<BTreeSet<Pid>> {
    let output = std::process::Command::new("lsof")
        .arg("-t")
        .arg("+D")
        .arg(directory)
        .output();

    parse_lsof_output(output)
}

#[cfg(unix)]
fn file_handle_owners(file: &StdPath) -> AppResult<BTreeSet<Pid>> {
    let output = std::process::Command::new("lsof")
        .arg("-t")
        .arg("--")
        .arg(file)
        .output();

    parse_lsof_output(output)
}

#[cfg(unix)]
fn parse_lsof_output(
    output_result: std::io::Result<std::process::Output>,
) -> AppResult<BTreeSet<Pid>> {
    let output = match output_result {
        Ok(out) => out,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("'lsof' command not found; cannot inspect open file handles on Unix");
        }
        Err(err) => {
            bail!("failed to execute 'lsof': {err}");
        }
    };

    let mut pids = BTreeSet::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if let Ok(pid) = line.parse::<u32>() {
            if pid != 0 {
                pids.insert(Pid::from_u32(pid));
            }
        }
    }
    Ok(pids)
}

#[cfg(not(any(windows, unix)))]
fn directory_handle_owners(_directory: &StdPath) -> AppResult<BTreeSet<Pid>> {
    Ok(BTreeSet::new())
}

#[cfg(not(any(windows, unix)))]
fn file_handle_owners(_file: &StdPath) -> AppResult<BTreeSet<Pid>> {
    Ok(BTreeSet::new())
}

#[cfg(windows)]
fn system_handles() -> AppResult<Vec<SystemHandleTableEntryInfoEx>> {
    let mut buffer = vec![0_u8; 1024 * 1024];

    loop {
        let mut return_length = 0_u32;
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_EXTENDED_HANDLE_INFORMATION,
                buffer.as_mut_ptr().cast::<c_void>(),
                buffer.len() as u32,
                &mut return_length,
            )
        };

        if status == STATUS_SUCCESS {
            break;
        }

        if status != STATUS_INFO_LENGTH_MISMATCH {
            bail!("NtQuerySystemInformation failed with NTSTATUS {status:#x}");
        }

        let next_len = (return_length as usize).max(buffer.len().saturating_mul(2));
        if next_len > 256 * 1024 * 1024 {
            bail!("system handle table is too large to scan");
        }
        buffer.resize(next_len, 0);
    }

    let handle_count = usize::from_ne_bytes(
        buffer[..size_of::<usize>()]
            .try_into()
            .expect("usize slice size is fixed"),
    );
    let entries_offset = size_of::<usize>() * 2;
    let entries_size = handle_count
        .checked_mul(size_of::<SystemHandleTableEntryInfoEx>())
        .context("system handle table size overflow")?;

    if buffer.len() < entries_offset + entries_size {
        bail!("system handle table response was truncated");
    }

    let entries = unsafe {
        std::slice::from_raw_parts(
            buffer.as_ptr().add(entries_offset) as *const SystemHandleTableEntryInfoEx,
            handle_count,
        )
    };

    Ok(entries.to_vec())
}

#[cfg(windows)]
fn duplicated_handle_path(process_id: u32, handle_value: usize) -> Option<PathBuf> {
    let source_process = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, process_id) };
    let source_process = OwnedHandle::new(source_process)?;

    let mut duplicated = std::ptr::null_mut::<c_void>();
    let duplicated_ok = unsafe {
        DuplicateHandle(
            source_process.raw(),
            handle_value as HANDLE,
            GetCurrentProcess(),
            &mut duplicated,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if duplicated_ok == 0 {
        return None;
    }

    let duplicated = OwnedHandle::new(duplicated)?;
    let file_type = unsafe { GetFileType(duplicated.raw()) };
    if file_type != FILE_TYPE_DISK {
        return None;
    }

    final_path_name(duplicated.raw())
}

#[cfg(windows)]
struct OwnedHandle {
    handle: HANDLE,
}

#[cfg(windows)]
impl OwnedHandle {
    fn new(handle: HANDLE) -> Option<Self> {
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    fn raw(&self) -> HANDLE {
        self.handle
    }
}

#[cfg(windows)]
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
fn final_path_name(handle: HANDLE) -> Option<PathBuf> {
    let flags = VOLUME_NAME_DOS | FILE_NAME_NORMALIZED;
    let mut buffer = vec![0_u16; 32_768];
    let written = unsafe {
        GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, flags)
    };

    if written == 0 {
        return None;
    }

    if written as usize >= buffer.len() {
        buffer.resize(written as usize + 1, 0);
        let written = unsafe {
            GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, flags)
        };
        if written == 0 || written as usize >= buffer.len() {
            return None;
        }
        buffer.truncate(written as usize);
    } else {
        buffer.truncate(written as usize);
    }

    let path = String::from_utf16_lossy(&buffer);
    Some(PathBuf::from(strip_windows_path_namespace(&path)))
}

#[cfg(windows)]
fn strip_windows_path_namespace(path: &str) -> String {
    if let Some(path) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{path}")
    } else if let Some(path) = path.strip_prefix(r"\\?\") {
        path.to_string()
    } else {
        path.to_string()
    }
}

#[cfg(windows)]
fn path_matches_directory(path: &StdPath, directory: &StdPath) -> bool {
    path == directory
        || path.starts_with(directory)
        || windows_path_string(path).is_some_and(|path| {
            windows_path_string(directory).is_some_and(|directory| {
                path == directory
                    || path
                        .strip_prefix(&directory)
                        .is_some_and(|rest| rest.starts_with('\\'))
            })
        })
}

#[cfg(windows)]
fn path_matches_file(path: &StdPath, file: &StdPath) -> bool {
    path == file
        || windows_path_string(path)
            .is_some_and(|path| windows_path_string(file).is_some_and(|file| path == file))
}

#[cfg(windows)]
fn windows_path_string(path: &StdPath) -> Option<String> {
    Some(
        strip_windows_path_namespace(&path.as_os_str().to_string_lossy())
            .replace('/', "\\")
            .to_lowercase(),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use sysinfo::Pid;

    use super::{ProcessCatalog, cwd_matches_directory, is_killable_target, name_matches};

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

    #[test]
    fn killable_target_excludes_system_and_current_processes() {
        assert!(!is_killable_target(Pid::from_u32(0), Pid::from_u32(42)));
        assert!(!is_killable_target(Pid::from_u32(42), Pid::from_u32(42)));
        assert!(is_killable_target(Pid::from_u32(43), Pid::from_u32(42)));
    }

    #[test]
    fn directory_matching_includes_descendants() {
        let root =
            std::env::temp_dir().join(format!("fuckport-directory-test-{}", std::process::id()));
        let child = root.join("child");
        fs::create_dir_all(&child).expect("failed to create test directory");

        let root = root.canonicalize().expect("failed to canonicalize root");
        assert!(cwd_matches_directory(&child, &root));

        fs::remove_dir_all(&root).expect("failed to clean test directory");
    }

    #[test]
    fn directory_matching_excludes_siblings() {
        let base = std::env::temp_dir().join(format!(
            "fuckport-directory-sibling-test-{}",
            std::process::id()
        ));
        let root = base.join("root");
        let sibling = base.join("root-sibling");
        fs::create_dir_all(&root).expect("failed to create root directory");
        fs::create_dir_all(&sibling).expect("failed to create sibling directory");

        let root = root.canonicalize().expect("failed to canonicalize root");
        assert!(!cwd_matches_directory(&sibling, &root));

        fs::remove_dir_all(&base).expect("failed to clean test directory");
    }

    #[test]
    fn invalid_port_target_returns_clear_error_in_resolve_targets() {
        use crate::input::Target;

        let catalog = ProcessCatalog::load().expect("failed to load catalog");
        let targets = vec![Target::InvalidPort {
            input: ":99999".to_string(),
            reason: "port number out of range (must be between 1 and 65535)".to_string(),
        }];

        let result = catalog.resolve_targets(&targets, false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid port target ':99999'"));
        assert!(err.contains("port number out of range"));
    }

    #[test]
    fn socket_enumeration_failure_reports_clear_error_for_port_targets() {
        use crate::input::Target;
        use std::collections::BTreeMap;

        let mut catalog = ProcessCatalog::load().expect("failed to load catalog");
        catalog.pids_by_port = BTreeMap::new();
        catalog.port_error = Some("permission denied accessing sockets".to_string());

        let targets = vec![Target::Port(8080)];
        let result = catalog.resolve_targets(&targets, false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cannot resolve port :8080"));
        assert!(err.contains("socket enumeration failed"));
    }

    #[cfg(unix)]
    #[test]
    fn file_handle_owners_finds_open_file_holder() {
        use super::file_handle_owners;
        use sysinfo::Pid;

        let temp_file =
            std::env::temp_dir().join(format!("fuckport-test-lsof-{}", std::process::id()));
        let file = std::fs::File::create(&temp_file).expect("failed to create temp file");

        let owners = file_handle_owners(&temp_file).expect("lsof execution failed");
        assert!(owners.contains(&Pid::from_u32(std::process::id())));

        drop(file);
        let _ = std::fs::remove_file(&temp_file);
    }
}
