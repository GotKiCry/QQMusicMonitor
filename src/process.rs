use anyhow::{anyhow, Result};
use std::mem::size_of;
use winapi::shared::minwindef::{DWORD, MAX_PATH};
use winapi::um::handleapi::CloseHandle;
use winapi::um::processthreadsapi::OpenProcess;
use winapi::um::tlhelp32::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, Process32FirstW, Process32NextW,
    MODULEENTRY32W, PROCESSENTRY32W, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32, TH32CS_SNAPPROCESS,
};
use winapi::um::winnt::{HANDLE, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};

/// 根据进程名查找进程ID (PID)
pub fn get_pid_by_name(target_name: &str) -> Result<DWORD> {
    let mut process_entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    process_entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() {
        return Err(anyhow!("Failed to create process snapshot"));
    }

    if unsafe { Process32FirstW(snapshot, &mut process_entry) } == 0 {
        unsafe { CloseHandle(snapshot) };
        return Err(anyhow!("Failed to get first process"));
    }

    loop {
        let process_name = {
            let mut buffer = [0u16; MAX_PATH];
            let len = process_entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(MAX_PATH);
            buffer[..len].copy_from_slice(&process_entry.szExeFile[..len]);
            String::from_utf16_lossy(&buffer[..len])
        };

        if process_name.eq_ignore_ascii_case(target_name) {
            unsafe { CloseHandle(snapshot) };
            println!("✓ Found process '{}' with PID: {}", target_name, process_entry.th32ProcessID);
            return Ok(process_entry.th32ProcessID);
        }

        if unsafe { Process32NextW(snapshot, &mut process_entry) } == 0 {
            break;
        }
    }

    unsafe { CloseHandle(snapshot) };
    Err(anyhow!("Process '{}' not found", target_name))
}

/// 根据PID获取进程句柄
pub fn get_process_handle(pid: DWORD) -> Result<HANDLE> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if handle.is_null() {
        Err(anyhow!("Failed to open process with PID {} (might need administrator privileges)", pid))
    } else {
        println!("✓ Successfully opened process handle for PID: {}", pid);
        Ok(handle)
    }
}

/// 获取进程中指定模块的基地址 (使用Toolhelp快照)
pub fn get_module_base_address(pid: DWORD, module_name: &str) -> Result<usize> {
    let mut module_entry: MODULEENTRY32W = unsafe { std::mem::zeroed() };
    module_entry.dwSize = size_of::<MODULEENTRY32W>() as u32;

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) };
    if snapshot.is_null() {
        return Err(anyhow!("Failed to create module snapshot for PID {} (might need administrator privileges)", pid));
    }

    if unsafe { Module32FirstW(snapshot, &mut module_entry) } == 0 {
        unsafe { CloseHandle(snapshot) };
        return Err(anyhow!("Failed to get first module for PID {}", pid));
    }

    loop {
        let current_module_name = {
            let mut buffer = [0u16; 256]; // szModule is [u16; 256]
            let len = module_entry
                .szModule
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(256);
            buffer[..len].copy_from_slice(&module_entry.szModule[..len]);
            String::from_utf16_lossy(&buffer[..len])
        };

        if current_module_name.eq_ignore_ascii_case(module_name) {
            unsafe { CloseHandle(snapshot) };
            let base_addr = module_entry.modBaseAddr as usize;
            println!("✓ Found module '{}' at base address: {:#X}", module_name, base_addr);
            return Ok(base_addr);
        }

        if unsafe { Module32NextW(snapshot, &mut module_entry) } == 0 {
            break;
        }
    }

    unsafe { CloseHandle(snapshot) };
    Err(anyhow!(
        "Module '{}' not found in process with PID {}",
        module_name,
        pid
    ))
}

