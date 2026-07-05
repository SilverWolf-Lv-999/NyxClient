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
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
                TerminateProcess, WaitForSingleObject,
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

    let status = command
        .status()
        .map_err(|error| format!("failed to start taskkill.exe: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("taskkill.exe exited with {status}"))
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
    let mut children = HashMap::<u32, Vec<u32>>::new();
    for entry in entries {
        children
            .entry(entry.parent_pid)
            .or_default()
            .push(entry.pid);
    }

    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    append_process_tree(root_pid, &children, &mut visited, &mut ordered);
    ordered
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

pub fn is_process_named(pid: u32, name: &str) -> bool {
    process_name(pid).is_some_and(|exe_name| exe_name.eq_ignore_ascii_case(name))
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
    let access = PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
    let handle = unsafe { OpenProcess(access, false, pid) }
        .map_err(|error| format!("OpenProcess failed: {error}"))?;
    let handle = OwnedHandle(handle);

    unsafe { TerminateProcess(handle.0, TERMINATE_EXIT_CODE) }
        .map_err(|error| format!("TerminateProcess failed: {error}"))?;
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
    if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
        Err("refusing to terminate the current process".to_owned())
    } else {
        Ok(())
    }
}

fn append_process_tree(
    pid: u32,
    children: &HashMap<u32, Vec<u32>>,
    visited: &mut HashSet<u32>,
    ordered: &mut Vec<u32>,
) {
    if !visited.insert(pid) {
        return;
    }

    if let Some(child_pids) = children.get(&pid) {
        for child in child_pids {
            append_process_tree(*child, children, visited, ordered);
        }
    }
    ordered.push(pid);
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
