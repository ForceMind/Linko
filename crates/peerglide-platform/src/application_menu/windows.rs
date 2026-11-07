//! Queries only the current GUI thread's own primary window. No input hook.
use anyhow::{ensure, Result};
use windows_sys::Win32::{Foundation::*, System::Threading::*, UI::WindowsAndMessaging::*};

struct Search<'a> {
    title: &'a [u16],
    window: HWND,
    count: usize,
}
unsafe extern "system" fn visit(window: HWND, value: LPARAM) -> i32 {
    // SAFETY: EnumThreadWindows synchronously borrows this stack-owned Search.
    let search = unsafe { &mut *(value as *mut Search<'_>) };
    let mut process = 0;
    if unsafe { GetWindowThreadProcessId(window, &mut process) } != unsafe { GetCurrentThreadId() }
        || process != unsafe { GetCurrentProcessId() }
    { return 1; }
    if unsafe { GetWindowTextLengthW(window) } as usize != search.title.len() { return 1; }
    let mut title = [0u16; 257];
    let length = unsafe { GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32) };
    if length > 0 && title[..length as usize] == *search.title {
        search.count += 1;
        search.window = window;
    }
    1
}
fn own_window(title: &str) -> Result<HWND> {
    let title: Vec<_> = title.encode_utf16().collect();
    ensure!(!title.is_empty() && title.len() <= 256, "检查窗口标题无效");
    let mut search = Search { title: &title, window: std::ptr::null_mut(), count: 0 };
    // SAFETY: enumerate only this event-loop thread; callbacks return before
    // Search drops. No HWND/ID is exported or treated as input authorization.
    unsafe { EnumThreadWindows(GetCurrentThreadId(), Some(visit), (&mut search as *mut Search<'_>) as LPARAM); }
    ensure!(search.count == 1 && !search.window.is_null(), "未找到唯一的本应用检查窗口");
    Ok(search.window)
}
pub(super) fn visible(title: &str) -> Result<bool> {
    let window = own_window(title)?;
    // Same GUI thread retains the window through this synchronous observation.
    Ok(unsafe { IsWindowVisible(window) } != 0)
}
pub(super) fn close(title: &str) -> Result<()> {
    let window = own_window(title)?;
    ensure!(unsafe { IsWindowVisible(window) } != 0, "检查窗口尚不可见");
    // Queue the normal winit close request; never destroy/hide the HWND here.
    ensure!(unsafe { PostMessageW(window, WM_CLOSE, 0, 0) } != 0, "无法请求关闭检查窗口");
    Ok(())
}
