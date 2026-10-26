#[cfg(all(windows, feature = "taskbar-progress"))]
use windows::Win32::UI::Shell::{
    ITaskbarList3, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_ERROR, TBPF_PAUSED,
};
#[cfg(all(windows, feature = "taskbar-progress"))]
use windows::Win32::Foundation::HWND;
#[cfg(all(windows, feature = "taskbar-progress"))]
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
#[cfg(all(windows, feature = "taskbar-progress"))]
use windows::core::GUID;
#[cfg(all(windows, feature = "taskbar-progress"))]
use std::cell::RefCell;

#[cfg(all(windows, feature = "taskbar-progress"))]
const CLSID_TASKBARLIST: GUID = GUID::from_u128(0x56FDF344_FD6D_11d0_958A_006097C9A090);

#[cfg(all(windows, feature = "taskbar-progress"))]
thread_local! {
    static TASKBAR: RefCell<Option<ITaskbarList3>> = RefCell::new(None);
}

#[cfg(all(windows, feature = "taskbar-progress"))]
fn with_taskbar<F: FnOnce(&ITaskbarList3)>(f: F) {
    TASKBAR.with(|cell| {
        let mut tb = cell.borrow_mut();
        if tb.is_none() {
            unsafe {
                if let Ok(t) = CoCreateInstance(&CLSID_TASKBARLIST, None, CLSCTX_INPROC_SERVER) {
                    *tb = Some(t);
                }
            }
        }
        if let Some(ref t) = *tb {
            f(t);
        }
    });
}

#[cfg(all(windows, feature = "taskbar-progress"))]
pub fn set_taskbar_progress(hwnd: *mut std::ffi::c_void, current: u64, total: u64) {
    with_taskbar(|taskbar| {
        let hwnd = HWND(hwnd as _);
        if total == 0 {
            let _ = unsafe { taskbar.SetProgressState(hwnd, TBPF_NOPROGRESS) };
        } else {
            let _ = unsafe { taskbar.SetProgressState(hwnd, TBPF_NORMAL) };
            let _ = unsafe { taskbar.SetProgressValue(hwnd, current, total) };
        }
    });
}

#[cfg(all(windows, feature = "taskbar-progress"))]
pub fn set_taskbar_error(hwnd: *mut std::ffi::c_void) {
    with_taskbar(|taskbar| {
        let hwnd = HWND(hwnd as _);
        let _ = unsafe { taskbar.SetProgressState(hwnd, TBPF_ERROR) };
    });
}

#[cfg(all(windows, feature = "taskbar-progress"))]
pub fn set_taskbar_paused(hwnd: *mut std::ffi::c_void) {
    with_taskbar(|taskbar| {
        let hwnd = HWND(hwnd as _);
        let _ = unsafe { taskbar.SetProgressState(hwnd, TBPF_PAUSED) };
    });
}

#[cfg(all(windows, feature = "taskbar-progress"))]
pub fn clear_taskbar(hwnd: *mut std::ffi::c_void) {
    with_taskbar(|taskbar| {
        let hwnd = HWND(hwnd as _);
        let _ = unsafe { taskbar.SetProgressState(hwnd, TBPF_NOPROGRESS) };
    });
}

#[cfg(not(all(windows, feature = "taskbar-progress")))]
pub fn set_taskbar_progress(_hwnd: *mut std::ffi::c_void, _current: u64, _total: u64) {}

#[cfg(not(all(windows, feature = "taskbar-progress")))]
pub fn set_taskbar_error(_hwnd: *mut std::ffi::c_void) {}

#[cfg(not(all(windows, feature = "taskbar-progress")))]
pub fn set_taskbar_paused(_hwnd: *mut std::ffi::c_void) {}

#[cfg(not(all(windows, feature = "taskbar-progress")))]
pub fn clear_taskbar(_hwnd: *mut std::ffi::c_void) {}