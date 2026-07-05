use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
    os::windows::process::CommandExt,
    process::Command,
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM},
        Security::{
            AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_DEBUG_NAME,
            SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
        },
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            Threading::{
                GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
                PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
            },
        },
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
        },
    },
    core::{BOOL, PCWSTR},
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TERMINATE_EXIT_CODE: u32 = 0x4E59;
const TERMINATE_WAIT_MS: u32 = 1500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessKillLevel {
    CloseWindows,
    Terminate,
    Privileged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSnapshotEntry {
    pub pid: u32,
    pub parent_pid: u32,
    pub exe_name: String,
}

pub fn kill_process(pid: u32, level: ProcessKillLevel, process_tree: bool) -> Result<(), String> {
    ensure_target_pid(pid)?;

    match level {
        ProcessKillLevel::CloseWindows => close_process_windows(pid, process_tree).map(|_| ()),
        ProcessKillLevel::Terminate => {
            if process_tree {
                terminate_process_tree(pid)
            } else {
                terminate_process(pid)
            }
        }
        ProcessKillLevel::Privileged => {
            if process_tree {
                force_terminate_process_tree(pid)
            } else {
                force_terminate_process(pid)
            }
        }
    }
}

pub fn close_process_windows(pid: u32, process_tree: bool) -> Result<usize, String> {
    ensure_target_pid(pid)?;

    let pids = selected_process_ids(pid, process_tree);
    if pids.is_empty() {
        return Err("no process ids were selected".to_owned());
    }

    let mut context = CloseWindowContext {
        pids: pids.into_iter().collect(),
        sent: 0,
    };
    unsafe {
        let _ = EnumWindows(
            Some(enum_close_windows_proc),
            LPARAM((&mut context as *mut CloseWindowContext) as isize),
        );
    }

    Ok(context.sent)
}

pub fn terminate_process(pid: u32) -> Result<(), String> {
    ensure_target_pid(pid)?;
    terminate_process_unchecked(pid)
}

pub fn terminate_process_tree(root_pid: u32) -> Result<(), String> {
    ensure_target_pid(root_pid)?;
    terminate_processes(&selected_process_ids(root_pid, true))
}

pub fn privileged_terminate_process(pid: u32) -> Result<(), String> {
    ensure_target_pid(pid)?;
    let _ = enable_debug_privilege();
    terminate_process_unchecked(pid)
}

pub fn privileged_terminate_process_tree(root_pid: u32) -> Result<(), String> {
    ensure_target_pid(root_pid)?;
    let _ = enable_debug_privilege();
    terminate_processes(&selected_process_ids(root_pid, true))
}

pub fn taskkill_process(pid: u32, process_tree: bool) -> Result<(), String> {
    ensure_target_pid(pid)?;

    let mut command = Command::new("taskkill");
    command.arg("/PID").arg(pid.to_string()).arg("/F");
    if process_tree {
        command.arg("/T");
    }
    command.creation_flags(CREATE_NO_WINDOW);

    let output = command
        .output()
        .map_err(|error| format!("failed to start taskkill.exe: {error}"))?;
    if output.status.success() || process_target_is_gone(pid, process_tree) {
        Ok(())
    } else {
        Err(format!(
            "taskkill.exe exited with {}{}",
            output.status,
            command_output_details(&output.stdout, &output.stderr)
        ))
    }
}

pub fn force_terminate_process(pid: u32) -> Result<(), String> {
    privileged_terminate_process(pid).or_else(|error| {
        taskkill_process(pid, false).map_err(|taskkill_error| {
            format!("{error}; taskkill fallback failed: {taskkill_error}")
        })
    })
}

pub fn force_terminate_process_tree(root_pid: u32) -> Result<(), String> {
    privileged_terminate_process_tree(root_pid).or_else(|error| {
        taskkill_process(root_pid, true).map_err(|taskkill_error| {
            format!("{error}; taskkill fallback failed: {taskkill_error}")
        })
    })
}

pub fn collect_process_tree(root_pid: u32) -> Vec<u32> {
    let entries = snapshot_processes();
    let existing = entries
        .iter()
        .map(|entry| entry.pid)
        .collect::<HashSet<_>>();
    let mut children = HashMap::<u32, Vec<u32>>::new();
    for entry in entries {
        children
            .entry(entry.parent_pid)
            .or_default()
            .push(entry.pid);
    }

    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    append_process_tree(root_pid, &children, &existing, &mut visited, &mut ordered);
    ordered
}

pub fn process_tree_root_for_termination(pid: u32) -> u32 {
    let process_entries = snapshot_processes()
        .into_iter()
        .map(|entry| (entry.pid, entry))
        .collect::<HashMap<_, _>>();

    process_tree_root_for_termination_from_snapshot(
        pid,
        unsafe { GetCurrentProcessId() },
        &process_entries,
    )
}

pub fn snapshot_processes() -> Vec<ProcessSnapshotEntry> {
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Vec::new();
    };
    let snapshot = OwnedHandle(snapshot);

    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    let mut entries = Vec::new();
    if unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_err() {
        return entries;
    }

    loop {
        entries.push(ProcessSnapshotEntry {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            exe_name: wide_z_to_string(&entry.szExeFile),
        });

        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
            break;
        }
    }

    entries
}

pub fn process_name(pid: u32) -> Option<String> {
    snapshot_processes()
        .into_iter()
        .find(|entry| entry.pid == pid)
        .map(|entry| entry.exe_name)
}

pub fn process_exists(pid: u32) -> bool {
    snapshot_processes()
        .into_iter()
        .any(|entry| entry.pid == pid)
}

pub fn is_process_named(pid: u32, name: &str) -> bool {
    process_name(pid).is_some_and(|exe_name| exe_name.eq_ignore_ascii_case(name))
}

pub fn is_protected_process(pid: u32) -> bool {
    process_name(pid).is_some_and(|exe_name| is_protected_process_name(&exe_name))
}

pub fn is_protected_process_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "system"
            | "idle"
            | "registry"
            | "secure system"
            | "smss.exe"
            | "csrss.exe"
            | "wininit.exe"
            | "winlogon.exe"
            | "services.exe"
            | "lsass.exe"
            | "lsaiso.exe"
            | "fontdrvhost.exe"
            | "dwm.exe"
            | "ctfmon.exe"
            | "sihost.exe"
            | "explorer.exe"
            | "shellexperiencehost.exe"
            | "startmenuexperiencehost.exe"
            | "searchhost.exe"
            | "searchindexer.exe"
            | "textinputhost.exe"
            | "applicationframehost.exe"
            | "runtimebroker.exe"
    )
}

fn selected_process_ids(root_pid: u32, process_tree: bool) -> Vec<u32> {
    if process_tree {
        collect_process_tree(root_pid)
    } else {
        vec![root_pid]
    }
}

fn terminate_processes(pids: &[u32]) -> Result<(), String> {
    let current_pid = unsafe { GetCurrentProcessId() };
    let mut failures = Vec::new();
    for pid in pids.iter().copied().filter(|pid| *pid != current_pid) {
        if is_protected_process(pid) {
            continue;
        }
        if let Err(error) = terminate_process_unchecked(pid) {
            failures.push(format!("{pid}: {error}"));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn terminate_process_unchecked(pid: u32) -> Result<(), String> {
    let access = PROCESS_TERMINATE | PROCESS_SYNCHRONIZE;
    let handle = match unsafe { OpenProcess(access, false, pid) } {
        Ok(handle) => handle,
        Err(_error) if !process_exists(pid) => return Ok(()),
        Err(error) => return Err(format!("OpenProcess failed: {error}")),
    };
    let handle = OwnedHandle(handle);

    if let Err(error) = unsafe { TerminateProcess(handle.0, TERMINATE_EXIT_CODE) } {
        if !process_exists(pid) {
            return Ok(());
        }
        return Err(format!("TerminateProcess failed: {error}"));
    }
    unsafe {
        let _ = WaitForSingleObject(handle.0, TERMINATE_WAIT_MS);
    }

    Ok(())
}

fn enable_debug_privilege() -> Result<(), String> {
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    }
    .map_err(|error| format!("OpenProcessToken failed: {error}"))?;
    let token = OwnedHandle(token);

    let mut luid = Default::default();
    unsafe { LookupPrivilegeValueW(PCWSTR::null(), SE_DEBUG_NAME, &mut luid) }
        .map_err(|error| format!("LookupPrivilegeValueW failed: {error}"))?;

    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    unsafe {
        AdjustTokenPrivileges(
            token.0,
            false,
            Some(&privileges as *const TOKEN_PRIVILEGES),
            size_of::<TOKEN_PRIVILEGES>() as u32,
            None,
            None,
        )
    }
    .map_err(|error| format!("AdjustTokenPrivileges failed: {error}"))?;

    Ok(())
}

fn ensure_target_pid(pid: u32) -> Result<(), String> {
    if pid == 0 {
        return Err("refusing to terminate pid 0".to_owned());
    }
    if pid == unsafe { GetCurrentProcessId() } {
        return Err("refusing to terminate the current process".to_owned());
    }
    if let Some(name) = process_name(pid)
        && is_protected_process_name(&name)
    {
        return Err(format!(
            "refusing to terminate protected Windows process {name} (pid {pid})"
        ));
    }

    Ok(())
}

fn process_target_is_gone(pid: u32, process_tree: bool) -> bool {
    if process_tree {
        collect_process_tree(pid).is_empty()
    } else {
        !process_exists(pid)
    }
}

fn command_output_details(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();

    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!(": {stdout}"),
        (true, false) => format!(": {stderr}"),
        (false, false) => format!(": {stderr}; stdout: {stdout}"),
    }
}

fn process_tree_root_for_termination_from_snapshot(
    pid: u32,
    current_pid: u32,
    process_entries: &HashMap<u32, ProcessSnapshotEntry>,
) -> u32 {
    let mut selected_pid = pid;
    let mut cursor_pid = pid;
    let mut visited = HashSet::new();

    for _ in 0..16 {
        if !visited.insert(cursor_pid) {
            break;
        }

        let Some(entry) = process_entries.get(&cursor_pid) else {
            break;
        };
        let parent_pid = entry.parent_pid;
        if parent_pid == 0 || parent_pid == current_pid || parent_pid == cursor_pid {
            break;
        }

        let Some(parent) = process_entries.get(&parent_pid) else {
            break;
        };
        if is_protected_process_name(&parent.exe_name) {
            break;
        }

        if is_command_or_script_host_name(&parent.exe_name) {
            selected_pid = parent_pid;
            cursor_pid = parent_pid;
        } else {
            break;
        }
    }

    selected_pid
}

fn is_command_or_script_host_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "cmd.exe"
            | "powershell.exe"
            | "pwsh.exe"
            | "wscript.exe"
            | "cscript.exe"
            | "mshta.exe"
            | "conhost.exe"
    )
}

fn append_process_tree(
    pid: u32,
    children: &HashMap<u32, Vec<u32>>,
    existing: &HashSet<u32>,
    visited: &mut HashSet<u32>,
    ordered: &mut Vec<u32>,
) {
    if !visited.insert(pid) {
        return;
    }

    if let Some(child_pids) = children.get(&pid) {
        for child in child_pids {
            append_process_tree(*child, children, existing, visited, ordered);
        }
    }
    if existing.contains(&pid) {
        ordered.push(pid);
    }
}

struct CloseWindowContext {
    pids: HashSet<u32>,
    sent: usize,
}

unsafe extern "system" fn enum_close_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let context = unsafe { &mut *(lparam.0 as *mut CloseWindowContext) };
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid != 0 && context.pids.contains(&pid) && unsafe { IsWindowVisible(hwnd).as_bool() } {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        context.sent += 1;
    }

    true.into()
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if self.0.is_invalid() {
            return;
        }

        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn wide_z_to_string(buffer: &[u16]) -> String {
    let len = buffer
        .iter()
        .position(|ch| *ch == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_entries(entries: &[(u32, u32, &str)]) -> HashMap<u32, ProcessSnapshotEntry> {
        entries
            .iter()
            .map(|(pid, parent_pid, exe_name)| {
                (
                    *pid,
                    ProcessSnapshotEntry {
                        pid: *pid,
                        parent_pid: *parent_pid,
                        exe_name: (*exe_name).to_owned(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn termination_root_climbs_to_command_host_parent() {
        let entries = process_entries(&[
            (10, 0, "explorer.exe"),
            (20, 10, "powershell.exe"),
            (30, 20, "conhost.exe"),
            (40, 30, "child.exe"),
        ]);

        assert_eq!(
            process_tree_root_for_termination_from_snapshot(40, 99, &entries),
            20
        );
    }

    #[test]
    fn termination_root_stops_at_protected_parent() {
        let entries = process_entries(&[(10, 0, "explorer.exe"), (20, 10, "app.exe")]);

        assert_eq!(
            process_tree_root_for_termination_from_snapshot(20, 99, &entries),
            20
        );
    }

    #[test]
    fn termination_root_does_not_climb_to_regular_parent() {
        let entries = process_entries(&[(10, 0, "launcher.exe"), (20, 10, "app.exe")]);

        assert_eq!(
            process_tree_root_for_termination_from_snapshot(20, 99, &entries),
            20
        );
    }

    #[test]
    fn termination_root_stops_at_current_process() {
        let entries = process_entries(&[(10, 0, "cmd.exe"), (20, 10, "app.exe")]);

        assert_eq!(
            process_tree_root_for_termination_from_snapshot(20, 10, &entries),
            20
        );
    }
}
