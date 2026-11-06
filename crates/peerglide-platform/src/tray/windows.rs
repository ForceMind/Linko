//! One hidden UI-message owner, never an input hook or a privileged service.
use super::{Command, Mailbox};
use anyhow::{ensure, Context, Result};
use std::{
    mem::size_of,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::*},
    UI::{Shell::*, WindowsAndMessaging::*},
};

const CALLBACK: u32 = WM_APP + 61;
const ICON_ID: u32 = 1;
const SHOW_ID: usize = 1;
const STOP_ID: usize = 2;
const QUIT_ID: usize = 3;
// Windows SDK shellapi.h: NINF_KEY=0x1, NIN_KEYSELECT=(NIN_SELECT | NINF_KEY).
// windows-sys 0.61 exports NIN_SELECT but omits the compound macro.
// https://github.com/microsoft/win32metadata/blob/main/generation/WinSDK/RecompiledIdlHeaders/um/shellapi.h
const KEY_SELECT_NOTIFICATION: u32 = NIN_SELECT | 0x1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Notice {
    Show,
    Menu,
    Ignore,
}
fn notification(value: LPARAM) -> Notice {
    // Version 4 packs the notification in LOWORD and the icon ID in HIWORD.
    match (value as u32) & 0xffff {
        WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_SELECT | KEY_SELECT_NOTIFICATION => Notice::Show,
        WM_RBUTTONUP | WM_CONTEXTMENU => Notice::Menu,
        _ => Notice::Ignore,
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn native_error(operation: &'static str) -> anyhow::Error {
    // Read immediately at each failed API boundary; no user content is involved.
    let code = unsafe { GetLastError() };
    anyhow::anyhow!("{operation} failed ({code})")
}

pub struct Tray {
    stop: Arc<AtomicBool>,
    available: Arc<AtomicBool>,
    // The mutex protects the actual cross-thread HWND lifetime, not UI state.
    // Its owner clears it before DestroyWindow; raw handles never authorize input.
    window: Arc<Mutex<Option<usize>>>,
    event: HANDLE,
    thread: Option<JoinHandle<()>>,
}
impl Tray {
    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // SAFETY: our private wake event stays open until the owner is joined.
        unsafe {
            SetEvent(self.event);
        }
        {
            let window = self
                .window
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(window) = *window {
                let mut result = 0;
                // A sent close is also served by TrackPopupMenu's nested loop.
                // It cancels menu tracking; the event handles an ordinary idle
                // loop even if the posted-message queue is saturated.
                // SAFETY: the lifetime mutex pins this private owner's HWND.
                unsafe {
                    SendMessageTimeoutW(
                        window as HWND,
                        WM_CLOSE,
                        0,
                        0,
                        SMTO_ABORTIFHUNG | SMTO_BLOCK,
                        2000,
                        &mut result,
                    );
                }
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // SAFETY: the owner no longer waits on this owned event.
        unsafe {
            CloseHandle(self.event);
        }
    }
}

struct State {
    mailbox: Arc<Mailbox>,
    stop: Arc<AtomicBool>,
    available: Arc<AtomicBool>,
    taskbar_created: u32,
    menu: HMENU,
    icon: HICON,
}
fn icon_data(window: HWND, icon: HICON) -> NOTIFYICONDATAW {
    // SAFETY: the Win32 structure is plain C data; all unused fields are zero.
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = window;
    data.uID = ICON_ID;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    data.uCallbackMessage = CALLBACK;
    data.hIcon = icon;
    for (destination, value) in data.szTip.iter_mut().zip("屏跃".encode_utf16()) {
        *destination = value;
    }
    data
}
fn add_icon(window: HWND, state: &State) -> bool {
    let mut data = icon_data(window, state.icon);
    // Explorer/DPI broadcasts can invalidate all old registrations. Deleting
    // our exact private ID also handles a still-present registration cleanly.
    // SAFETY: initialized structure, own HWND/ID, borrowed shared stock icon.
    unsafe {
        Shell_NotifyIconW(NIM_DELETE, &mut data);
    }
    if unsafe { Shell_NotifyIconW(NIM_ADD, &mut data) } == 0 {
        return false;
    }
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    if unsafe { Shell_NotifyIconW(NIM_SETVERSION, &mut data) } == 0 {
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &mut data);
        }
        return false;
    }
    true
}
fn dispatch(state: &State, id: usize) {
    let command = match id {
        SHOW_ID => Command::Show,
        STOP_ID => Command::Stop,
        QUIT_ID => Command::Quit,
        _ => return,
    };
    state.mailbox.post(command);
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: CreateWindowEx passes our live Box<State> as lpCreateParams.
        let create = unsafe { &*(l as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        return 1;
    }
    if message == WM_CLOSE {
        // Do not destroy here: teardown owns the HWND lifetime mutex. EndMenu
        // permits a cross-thread Drop even while this window tracks its menu.
        unsafe {
            EndMenu();
            PostQuitMessage(0);
        }
        return 0;
    }
    if message == WM_NCDESTROY {
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
        }
        return unsafe { DefWindowProcW(window, message, w, l) };
    }
    let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const State;
    if !pointer.is_null() {
        // SAFETY: State is boxed before creation and outlives window destruction
        // and all nested menu callbacks. No mutable borrow survives an OS call.
        let state = unsafe { &*pointer };
        if message == state.taskbar_created {
            state.available.store(false, Ordering::Release);
            if !state.stop.load(Ordering::Acquire) {
                let added = add_icon(window, state);
                state.available.store(added, Ordering::Release);
                if !added {
                    state.mailbox.post(Command::Show);
                }
            }
            return 0;
        }
        if message == CALLBACK && !state.stop.load(Ordering::Acquire) {
            match notification(l) {
                Notice::Show => state.mailbox.post(Command::Show),
                Notice::Menu => {
                    let mut point = POINT { x: 0, y: 0 };
                    if unsafe { GetCursorPos(&mut point) } != 0 {
                        // Required by the native shortcut-menu contract so an
                        // outside click dismisses it. This never focuses a peer,
                        // shows a hidden app, or injects user input.
                        unsafe {
                            SetForegroundWindow(window);
                        }
                        let id = unsafe {
                            TrackPopupMenu(
                                state.menu,
                                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                                point.x,
                                point.y,
                                0,
                                window,
                                null(),
                            )
                        };
                        if !state.stop.load(Ordering::Acquire) {
                            dispatch(state, id as usize);
                        }
                        // Microsoft's menu pattern posts a benign follow-up so
                        // the next tray interaction is dismissed normally too.
                        unsafe {
                            PostMessageW(window, WM_NULL, 0, 0);
                        }
                    }
                }
                Notice::Ignore => (),
            }
            return 0;
        }
    }
    unsafe { DefWindowProcW(window, message, w, l) }
}

struct Owner {
    state: Box<State>,
    module: HINSTANCE,
    class: Vec<u16>,
    registered: bool,
    window: HWND,
    handle: Arc<Mutex<Option<usize>>>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.state.available.store(false, Ordering::Release);
        {
            let mut handle = self
                .handle
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            *handle = None;
            if !self.window.is_null() {
                let mut data = icon_data(self.window, self.state.icon);
                // SAFETY: delete our icon before destroying its still-owned HWND.
                unsafe {
                    Shell_NotifyIconW(NIM_DELETE, &mut data);
                    DestroyWindow(self.window);
                }
            }
        }
        // SAFETY: menu/class were created by this owner, never shared outside its
        // native thread. State/its callback pointer stay live through destruction.
        unsafe {
            DestroyMenu(self.state.menu);
            if self.registered {
                UnregisterClassW(self.class.as_ptr(), self.module);
            }
        }
        // LoadIcon's IDI_APPLICATION is a system-owned shared icon: no DestroyIcon.
    }
}

fn run(
    mailbox: Arc<Mailbox>,
    stop: Arc<AtomicBool>,
    available: Arc<AtomicBool>,
    handle: Arc<Mutex<Option<usize>>>,
    event: usize,
    ready: &mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    let menu = unsafe { CreatePopupMenu() };
    ensure!(!menu.is_null(), native_error("CreatePopupMenu"));
    let module = unsafe { GetModuleHandleW(null()) };
    if module.is_null() {
        let error = native_error("GetModuleHandleW");
        unsafe {
            DestroyMenu(menu);
        }
        return Err(error);
    }
    let taskbar_created = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
    let icon = unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
    let mut owner = Owner {
        state: Box::new(State {
            mailbox,
            stop,
            available,
            taskbar_created,
            menu,
            icon,
        }),
        module,
        class: wide(&format!("PeerGlide.Tray.{}", unsafe {
            GetCurrentThreadId()
        })),
        registered: false,
        window: null_mut(),
        handle,
    };
    ensure!(
        taskbar_created != 0 && !icon.is_null(),
        "无法准备屏跃托盘图标"
    );
    for (id, label) in [
        (SHOW_ID, "显示主窗口"),
        (STOP_ID, "停止协同"),
        (QUIT_ID, "退出屏跃"),
    ] {
        let label = wide(label);
        ensure!(
            unsafe { AppendMenuW(menu, MF_STRING, id, label.as_ptr()) } != 0,
            native_error("AppendMenuW")
        );
    }
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: module,
        lpszClassName: owner.class.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    ensure!(
        unsafe { RegisterClassW(&class) } != 0,
        native_error("RegisterClassW")
    );
    owner.registered = true;
    // A never-shown top-level UI-message window receives TaskbarCreated broadcasts;
    // HWND_MESSAGE would miss them. It installs no input/window-event hook.
    owner.window = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            owner.class.as_ptr(),
            wide("屏跃托盘").as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            module,
            (&*owner.state as *const State).cast(),
        )
    };
    ensure!(!owner.window.is_null(), native_error("CreateWindowExW"));
    *owner
        .handle
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(owner.window as usize);
    ensure!(!owner.state.stop.load(Ordering::Acquire), "托盘创建已取消");
    ensure!(add_icon(owner.window, &owner.state), "无法登记屏跃托盘图标");
    owner.state.available.store(true, Ordering::Release);
    let _ = ready.try_send(Ok(()));
    let event = event as HANDLE;
    let mut message: MSG = unsafe { std::mem::zeroed() };
    while !owner.state.stop.load(Ordering::Acquire) {
        let wait = unsafe {
            MsgWaitForMultipleObjectsEx(1, &event, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        if wait == WAIT_OBJECT_0 {
            break;
        }
        ensure!(
            wait != WAIT_FAILED,
            native_error("MsgWaitForMultipleObjectsEx")
        );
        for _ in 0..128 {
            if unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } == 0 {
                break;
            }
            if message.message == WM_QUIT {
                return Ok(());
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            if owner.state.stop.load(Ordering::Acquire) {
                break;
            }
        }
    }
    Ok(())
}

pub(super) fn create(mailbox: Arc<Mailbox>) -> Result<Tray> {
    // SAFETY: private non-inherited manual-reset event, closed after thread join.
    let event = unsafe { CreateEventW(null(), 1, 0, null()) };
    ensure!(!event.is_null(), native_error("CreateEventW"));
    let stop = Arc::new(AtomicBool::new(false));
    let available = Arc::new(AtomicBool::new(false));
    let window = Arc::new(Mutex::new(None));
    let (ready, received) = mpsc::sync_channel(1);
    let (thread_stop, thread_available, thread_window) =
        (stop.clone(), available.clone(), window.clone());
    let event_value = event as usize;
    let thread = match std::thread::Builder::new()
        .name("peerglide-tray".into())
        .spawn(move || {
            let result = run(
                mailbox.clone(),
                thread_stop.clone(),
                thread_available.clone(),
                thread_window,
                event_value,
                &ready,
            );
            thread_available.store(false, Ordering::Release);
            if let Err(error) = result {
                let _ = ready.try_send(Err(error));
            }
            // Never leave a hidden GUI without a recovery entry after native failure.
            if !thread_stop.load(Ordering::Acquire) {
                mailbox.post(Command::Show);
            }
        }) {
        Ok(thread) => thread,
        Err(error) => {
            unsafe {
                CloseHandle(event);
            }
            return Err(error).context("无法创建托盘线程");
        }
    };
    let tray = Tray {
        stop,
        available,
        window,
        event,
        thread: Some(thread),
    };
    received
        .recv_timeout(Duration::from_secs(3))
        .context("托盘初始化未完成")??;
    Ok(tray)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_keyboard_and_packed_version4_notifications_keep_their_actions() {
        assert_eq!(KEY_SELECT_NOTIFICATION, 0x0401);
        for event in [
            WM_LBUTTONUP,
            WM_LBUTTONDBLCLK,
            NIN_SELECT,
            KEY_SELECT_NOTIFICATION,
        ] {
            assert_eq!(notification(event as LPARAM), Notice::Show);
            assert_eq!(
                notification(((ICON_ID << 16) | event) as LPARAM),
                Notice::Show
            );
        }
        for event in [WM_RBUTTONUP, WM_CONTEXTMENU] {
            assert_eq!(
                notification(((ICON_ID << 16) | event) as LPARAM),
                Notice::Menu
            );
        }
        for event in [0, WM_MOUSEMOVE, WM_USER + 2, WM_USER + 6] {
            assert_eq!(
                notification(((ICON_ID << 16) | event) as LPARAM),
                Notice::Ignore,
                "unrelated/balloon/popup notifications cannot become Show or Menu"
            );
        }
    }
}
