//! Main-thread AppKit menu UI only. No input tap, hotkey or permission request.
use super::{Command, Mailbox};
use anyhow::{Context, Result};
use objc2::{
    define_class, msg_send,
    rc::{autoreleasepool, Retained},
    runtime::AnyObject,
    sel, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};
use std::sync::Arc;

struct MenuIvars {
    mailbox: Arc<Mailbox>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuIvars]
    struct PeerGlideTrayTarget;

    impl PeerGlideTrayTarget {
        #[unsafe(method(showMainWindow:))]
        fn show_main_window(&self, _sender: Option<&AnyObject>) {
            self.ivars().mailbox.post(Command::Show);
        }
        #[unsafe(method(stopCollaboration:))]
        fn stop_collaboration(&self, _sender: Option<&AnyObject>) {
            self.ivars().mailbox.post(Command::Stop);
        }
        #[unsafe(method(quitPeerGlide:))]
        fn quit_peerglide(&self, _sender: Option<&AnyObject>) {
            self.ivars().mailbox.post(Command::Quit);
        }
    }
    // SAFETY: the NSObject methods are inherited; all actions are main-thread
    // menu selectors with one nullable object argument and no return value.
    unsafe impl NSObjectProtocol for PeerGlideTrayTarget {}
);
impl PeerGlideTrayTarget {
    fn new(main: MainThreadMarker, mailbox: Arc<Mailbox>) -> Retained<Self> {
        let object = Self::alloc(main).set_ivars(MenuIvars { mailbox });
        // SAFETY: initialize our NSObject superclass after setting Rust ivars.
        unsafe { msg_send![super(object), init] }
    }
}

/// AppKit objects and the marker make this owner !Send/!Sync. Hold and drop it
/// on the application's main thread, whose normal AppKit loop serves the menu.
pub struct Tray {
    bar: Retained<NSStatusBar>,
    item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    items: [Retained<NSMenuItem>; 3],
    _target: Retained<PeerGlideTrayTarget>,
    _main: MainThreadMarker,
}
impl Tray {
    /// Local recovery-UI availability, never input authority or an OS grant.
    pub fn is_available(&self) -> bool {
        self.item.isVisible()
    }
}

pub(super) fn create(mailbox: Arc<Mailbox>) -> Result<Tray> {
    let main = MainThreadMarker::new().context("菜单栏项目必须在界面主线程创建")?;
    autoreleasepool(|_| {
        let target = PeerGlideTrayTarget::new(main, mailbox);
        let menu = NSMenu::new(main);
        menu.setAutoenablesItems(false);
        let entries = [
            ("显示主窗口", sel!(showMainWindow:)),
            ("停止协同", sel!(stopCollaboration:)),
            ("退出屏跃", sel!(quitPeerGlide:)),
        ];
        let items = entries.map(|(title, action)| {
            // SAFETY: action matches our retained target's object-argument
            // selector. An empty key equivalent installs no global shortcut.
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(main),
                    &NSString::from_str(title),
                    Some(action),
                    &NSString::new(),
                )
            };
            // SAFETY: target remains strongly retained by Tray. AppKit's target
            // property itself is weak; teardown detaches it before release.
            unsafe {
                item.setTarget(Some(&target));
            }
            item.setEnabled(true);
            menu.addItem(&item);
            item
        });
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);
        // From here even a failed button lookup removes the created status item.
        let tray = Tray {
            bar,
            item,
            menu,
            items,
            _target: target,
            _main: main,
        };
        let button = tray.item.button(main).context("无法创建屏跃菜单栏按钮")?;
        button.setTitle(&NSString::from_str("屏跃"));
        tray.item.setMenu(Some(&tray.menu));
        // Default behavior deliberately does not allow user removal. There must
        // remain a recovery entry while the main window is hidden.
        tray.item.setVisible(true);
        Ok(tray)
    })
}
impl Drop for Tray {
    fn drop(&mut self) {
        autoreleasepool(|_| {
            self.menu.cancelTracking();
            self.item.setMenu(None);
            for item in &self.items {
                // SAFETY: remove selectors/weak targets while our strong target
                // is still alive; no menu callback can refer to freed Rust ivars.
                unsafe {
                    item.setAction(None);
                    item.setTarget(None);
                }
            }
            self.bar.removeStatusItem(&self.item);
        });
    }
}
