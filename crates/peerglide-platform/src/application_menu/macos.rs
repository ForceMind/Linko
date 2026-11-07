//! AppKit menu owner; reviewed against winit 0.30.13's macOS delegate.
use super::{Mailbox, Registry, TerminationCheck};
use crate::tray::Command;
use anyhow::{Context, Result};
use objc2::{
    define_class, msg_send,
    rc::{autoreleasepool, Retained},
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSApplication, NSApplicationTerminateReply, NSEventModifierFlags, NSMenu, NSMenuItem,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSOperationQueue, NSString};
use std::{
    cell::RefCell,
    sync::{atomic::Ordering, Arc, Weak},
};

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

/// This method remains registered after Owner drops: Objective-C cannot remove
/// an added method. It contains no owner pointer and never permits termination.
/// An absent/stale registry simply cancels, rather than reaching freed state or
/// silently bypassing GUI recovery. No AppKit queries, waits, input or I/O here.
unsafe extern "C-unwind" fn should_terminate(
    delegate: *mut AnyObject,
    _selector: Sel,
    application: *mut NSApplication,
) -> NSApplicationTerminateReply {
    if MainThreadMarker::new().is_some() {
        let mailbox = REGISTRY
            .try_with(|registry| {
                registry
                    .try_borrow()
                    .ok()
                    .and_then(|registry| registry.mailbox(delegate as usize, application as usize))
            })
            .ok()
            .flatten();
        // Drop the TLS borrow before wake; a scheduling callback can re-enter.
        if let Some(mailbox) = mailbox {
            mailbox.post(Command::Quit);
        }
    }
    NSApplicationTerminateReply::TerminateCancel
}

fn bridge_imp() -> Imp {
    // SAFETY: class_addMethod below installs this exact C-unwind function with
    // the NSUInteger(id, SEL, NSApplication*) ABI. Both supported Mac targets
    // are 64-bit; the type encoding is Q@:@. No callback reference is retained.
    unsafe {
        std::mem::transmute::<
            unsafe extern "C-unwind" fn(
                *mut AnyObject,
                Sel,
                *mut NSApplication,
            ) -> NSApplicationTerminateReply,
            Imp,
        >(should_terminate)
    }
}

fn checked_delegate(app: &NSApplication) -> Result<Retained<AnyObject>> {
    // Retain the actual delegate on the main thread; never replace it, proxy it
    // or alter its instance/class/lifecycle implementation.
    let delegate = app.delegate().context("应用退出代理尚未初始化")?;
    let object: &AnyObject = (*delegate).as_ref();
    let class = object.class();
    anyhow::ensure!(
        cfg!(target_pointer_width = "64")
            && class.name() == c"WinitApplicationDelegate"
            && class
                .superclass()
                .is_some_and(|parent| parent.name() == c"NSObject"),
        "应用退出代理不是已核对的 winit 0.30.13 类型，不能启用原生协同"
    );
    for selector in [
        sel!(applicationDidFinishLaunching:),
        sel!(applicationWillTerminate:),
    ] {
        class
            .verify_sel::<(&NSNotification,), ()>(selector)
            .map_err(|_| anyhow::anyhow!("应用退出代理生命周期签名未知，不能启用原生协同"))?;
    }
    Ok(Retained::from(&*delegate))
}

fn check_bridge(class: &AnyClass) -> bool {
    class
        .instance_method(sel!(applicationShouldTerminate:))
        .is_some_and(|method| {
            std::ptr::fn_addr_eq(method.implementation(), bridge_imp())
                && class
                    .verify_sel::<(&NSApplication,), NSApplicationTerminateReply>(
                        sel!(applicationShouldTerminate:),
                    )
                    .is_ok()
        })
}

fn install_bridge(class: &AnyClass) -> Result<()> {
    let selector = sel!(applicationShouldTerminate:);
    if class.instance_method(selector).is_none() {
        // SAFETY: only add the missing selector to the checked existing class.
        // Never replace a foreign method. The SDK defines the return type as
        // NSUInteger; macOS x86_64/aarch64 both encode it as Q. The callback has
        // this exact ABI and is process-lifetime code with a weak TLS registry.
        let added = unsafe {
            objc2::ffi::class_addMethod(
                (class as *const AnyClass).cast_mut(),
                selector,
                bridge_imp(),
                c"Q@:@".as_ptr(),
            )
        };
        anyhow::ensure!(added.as_bool(), "无法安装应用退出保护，不能启用原生协同");
    }
    anyhow::ensure!(
        check_bridge(class),
        "应用退出保护存在未知冲突，不能启用原生协同"
    );
    Ok(())
}

struct MenuIvars {
    mailbox: Arc<Mailbox>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuIvars]
    struct PeerGlideApplicationMenuTarget;

    impl PeerGlideApplicationMenuTarget {
        #[unsafe(method(showMainWindow:))]
        fn show(&self, _sender: Option<&AnyObject>) {
            self.ivars().mailbox.post(Command::Show);
        }
        #[unsafe(method(stopCollaboration:))]
        fn stop(&self, _sender: Option<&AnyObject>) {
            self.ivars().mailbox.post(Command::Stop);
        }
        #[unsafe(method(quitPeerGlide:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            self.ivars().mailbox.post(Command::Quit);
        }
    }
    // SAFETY: inherit NSObject; all own actions accept one nullable object and
    // return void. The target is retained until selectors have been detached.
    unsafe impl NSObjectProtocol for PeerGlideApplicationMenuTarget {}
);
impl PeerGlideApplicationMenuTarget {
    fn new(main: MainThreadMarker, mailbox: Arc<Mailbox>) -> Retained<Self> {
        let object = Self::alloc(main).set_ivars(MenuIvars { mailbox });
        // SAFETY: initialize the NSObject superclass after installing Rust ivars.
        unsafe { msg_send![super(object), init] }
    }
}

/// Main-thread-only owner. Keep alive even when the optional status item is off.
pub struct Owner {
    app: Retained<NSApplication>,
    delegate: Retained<AnyObject>,
    menu: Retained<NSMenu>,
    app_menu: Retained<NSMenu>,
    services: Retained<NSMenu>,
    previous_menu: Option<Retained<NSMenu>>,
    previous_services: Option<Retained<NSMenu>>,
    items: Vec<Retained<NSMenuItem>>,
    target: Retained<PeerGlideApplicationMenuTarget>,
    generation: u64,
    _main: MainThreadMarker,
}
impl Owner {
    fn window_for_check(&self, title: &str) -> Result<Retained<objc2_app_kit::NSWindow>> {
        anyhow::ensure!(self.is_available(), "应用退出保护不可用");
        let windows = self.app.windows();
        let mut found = None;
        for index in 0..windows.count() {
            let window = windows.objectAtIndex(index);
            if window.title().to_string() == title {
                anyhow::ensure!(found.is_none(), "检查窗口不唯一");
                found = Some(window);
            }
        }
        found.context("未找到本应用的检查窗口")
    }
    /// Read only this application's exact-title NSWindow, not other apps.
    /// AppKit visibility does not prove pixels are unobscured or physical input.
    pub fn window_visible_for_check(&self, title: &str) -> Result<bool> {
        Ok(self.window_for_check(title)?.isVisible())
    }
    /// Send the real AppKit close request; the GUI still decides hide vs stop.
    /// Only use in an isolated NoNative diagnostic, never a user KVM session.
    pub fn request_window_close_for_check(&self, title: &str) -> Result<()> {
        let window = self.window_for_check(title)?;
        anyhow::ensure!(window.isVisible(), "检查窗口尚不可见");
        window.performClose(None);
        Ok(())
    }
    /// Checks the real menu/delegate/bridge; never evidence of input permission.
    pub fn is_available(&self) -> bool {
        let current_menu = self.app.mainMenu();
        let current_delegate = self.app.delegate();
        let expected_target: &AnyObject = &self.target;
        current_menu
            .as_deref()
            .is_some_and(|menu| std::ptr::eq(menu, &*self.menu))
            && current_delegate.as_deref().is_some_and(|delegate| {
                let object: &AnyObject = delegate.as_ref();
                std::ptr::eq(object, &*self.delegate) && check_bridge(object.class())
            })
            && REGISTRY.with(|registry| registry.borrow().contains(self.generation))
            && self.menu.numberOfItems() == 1
            && self
                .menu
                .itemAtIndex(0)
                .and_then(|root| root.submenu())
                .as_deref()
                .is_some_and(|menu| std::ptr::eq(menu, &*self.app_menu))
            && self.app_menu.numberOfItems() == 11
            && [
                (2, 1, sel!(showMainWindow:)),
                (3, 2, sel!(stopCollaboration:)),
                (10, 7, sel!(quitPeerGlide:)),
            ]
            .into_iter()
            .all(|(position, owned, selector)| {
                self.app_menu
                    .itemAtIndex(position)
                    .as_deref()
                    .is_some_and(|item| {
                        std::ptr::eq(item, &*self.items[owned])
                            && item.action() == Some(selector)
                            && item.isEnabled()
                            && item
                                .target()
                                .as_deref()
                                .is_some_and(|target| std::ptr::eq(target, expected_target))
                    })
            })
            && self.items[7].keyEquivalent().to_string() == "q"
            && self.items[7].keyEquivalentModifierMask() == NSEventModifierFlags::Command
    }

    /// Queue the real AppKit path outside the current winit callback. Only use
    /// in an isolated diagnostic process with no native work. Keep this Owner
    /// alive and prevent GUI Close while the returned ticket is Pending.
    /// Returned records only SDK return and its prior isRunning snapshot, not
    /// cleanup or physical Dock proof. A broken bridge can still abort/exit;
    /// the external supervisor must reject a missing final receipt.
    pub fn request_termination_for_check(&self) -> Result<TerminationCheck> {
        anyhow::ensure!(self.is_available(), "应用退出保护不可用，不能执行退出检查");
        let check = TerminationCheck::pending();
        let state = check.state.clone();
        let application = (&*self.app as *const NSApplication) as usize;
        let delegate = (&*self.delegate as *const AnyObject) as usize;
        let menu = (&*self.menu as *const NSMenu) as usize;
        let generation = self.generation;
        let mailbox = Arc::downgrade(&self.target.ivars().mailbox);
        // The queue copies the block. It captures only Send-safe integers,
        // atomic state and Weak<Mailbox>, never a Retained AppKit object or
        // MainThreadMarker. SDK objects are reacquired on the main queue.
        let operation: block2::RcBlock<dyn Fn()> = block2::RcBlock::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                queued_termination(application, delegate, menu, generation, &mailbox)
            }));
            let status = match result {
                Ok(Ok(false)) => 1,
                Ok(Ok(true)) => 2,
                _ => 3,
            };
            state.store(status, Ordering::Release);
        });
        // SAFETY: the copied block captures only Send-safe owned values listed
        // above. The main queue executes it on the GUI thread after this winit
        // callback returns; no Retained AppKit object crosses a thread boundary.
        unsafe {
            NSOperationQueue::mainQueue().addOperationWithBlock(&operation);
        }
        Ok(check)
    }
}

fn queued_termination(
    application: usize,
    delegate: usize,
    menu: usize,
    generation: u64,
    mailbox: &Weak<Mailbox>,
) -> Result<bool> {
    let main = MainThreadMarker::new().context("退出检查没有在主队列执行")?;
    let registered = || {
        REGISTRY
            .with(|registry| {
                let registry = registry.try_borrow().ok()?;
                if !registry.contains(generation) {
                    return None;
                }
                let current = registry.mailbox(delegate, application)?;
                let expected = mailbox.upgrade()?;
                Arc::ptr_eq(&current, &expected).then_some(())
            })
            .is_some()
    };
    anyhow::ensure!(registered(), "退出检查所有者已变化");
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(main);
        anyhow::ensure!(
            (&*app as *const NSApplication) as usize == application,
            "退出检查应用已变化"
        );
        let current_delegate = checked_delegate(&app)?;
        anyhow::ensure!(
            (&*current_delegate as *const AnyObject) as usize == delegate
                && check_bridge(current_delegate.class()),
            "退出检查代理保护已变化"
        );
        anyhow::ensure!(
            app.mainMenu()
                .as_deref()
                .is_some_and(|current| { (current as *const NSMenu) as usize == menu }),
            "退出检查菜单已变化"
        );
        let running = app.isRunning();
        // No TLS/RefCell borrow survives any AppKit query or this real call.
        // terminate: can run a nested run loop; the GUI must not close or tear
        // down its owner until the ticket is Returned/Failed. In-flight SDK
        // execution cannot be canceled or interrupted by dropping the ticket.
        anyhow::ensure!(registered(), "退出检查所有者已失效");
        app.terminate(None);
        Ok(running)
    })
}

fn item(
    main: MainThreadMarker,
    menu: &NSMenu,
    title: &str,
    key: &str,
    action: Sel,
    target: &AnyObject,
) -> Retained<NSMenuItem> {
    // SAFETY: every selector below is either an SDK NSApplication action or an
    // own target's void/object action. Owner holds both targets strongly.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(main),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    };
    // SAFETY: the matching target remains strongly retained by Owner.
    unsafe {
        item.setTarget(Some(target));
    }
    item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
    item.setEnabled(true);
    menu.addItem(&item);
    item
}

pub(super) fn install(mailbox: Arc<Mailbox>) -> Result<Owner> {
    let main = MainThreadMarker::new().context("应用菜单必须在界面主线程安装")?;
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(main);
        let delegate = checked_delegate(&app)?;
        install_bridge(delegate.class())?;
        let generation = REGISTRY
            .with(|registry| {
                registry.borrow_mut().attach(
                    (&*delegate as *const AnyObject) as usize,
                    (&*app as *const NSApplication) as usize,
                    &mailbox,
                )
            })
            .context("已有应用菜单所有者，不能重复安装")?;
        let target = PeerGlideApplicationMenuTarget::new(main, mailbox);
        let menu = NSMenu::new(main);
        let services = NSMenu::new(main);
        let app_menu = NSMenu::new(main);
        app_menu.setTitle(&NSString::from_str("屏跃"));
        app_menu.setAutoenablesItems(false);
        let root = NSMenuItem::new(main);
        root.setTitle(&NSString::from_str("屏跃"));
        root.setSubmenu(Some(&app_menu));
        menu.addItem(&root);
        let mut items = vec![item(
            main,
            &app_menu,
            "关于屏跃",
            "",
            sel!(orderFrontStandardAboutPanel:),
            &app,
        )];
        app_menu.addItem(&NSMenuItem::separatorItem(main));
        items.push(item(
            main,
            &app_menu,
            "显示主窗口",
            "",
            sel!(showMainWindow:),
            &target,
        ));
        items.push(item(
            main,
            &app_menu,
            "停止协同",
            "",
            sel!(stopCollaboration:),
            &target,
        ));
        app_menu.addItem(&NSMenuItem::separatorItem(main));
        let service_item = NSMenuItem::new(main);
        service_item.setTitle(&NSString::from_str("服务"));
        service_item.setSubmenu(Some(&services));
        app_menu.addItem(&service_item);
        items.push(service_item);
        items.push(item(main, &app_menu, "隐藏屏跃", "h", sel!(hide:), &app));
        let hide_others = item(
            main,
            &app_menu,
            "隐藏其他应用",
            "h",
            sel!(hideOtherApplications:),
            &app,
        );
        hide_others.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Option,
        );
        items.push(hide_others);
        items.push(item(
            main,
            &app_menu,
            "显示全部",
            "",
            sel!(unhideAllApplications:),
            &app,
        ));
        app_menu.addItem(&NSMenuItem::separatorItem(main));
        items.push(item(
            main,
            &app_menu,
            "退出屏跃",
            "q",
            sel!(quitPeerGlide:),
            &target,
        ));
        let owner = Owner {
            previous_menu: app.mainMenu(),
            previous_services: app.servicesMenu(),
            app,
            delegate,
            menu,
            app_menu,
            services,
            items,
            target,
            generation,
            _main: main,
        };
        owner.app.setServicesMenu(Some(&owner.services));
        owner.app.setMainMenu(Some(&owner.menu));
        anyhow::ensure!(owner.is_available(), "无法确认应用菜单退出保护");
        Ok(owner)
    })
}
impl Drop for Owner {
    fn drop(&mut self) {
        // Detach callback routing before touching targets or AppKit resources.
        REGISTRY.with(|registry| registry.borrow_mut().detach(self.generation));
        autoreleasepool(|_| {
            self.menu.cancelTracking();
            for item in &self.items {
                // SAFETY: clear weak targets/actions while strong targets live.
                unsafe {
                    item.setAction(None);
                    item.setTarget(None);
                }
            }
            if self
                .app
                .mainMenu()
                .as_deref()
                .is_some_and(|menu| std::ptr::eq(menu, &*self.menu))
            {
                self.app.setMainMenu(self.previous_menu.as_deref());
            }
            if self
                .app
                .servicesMenu()
                .as_deref()
                .is_some_and(|menu| std::ptr::eq(menu, &*self.services))
            {
                self.app.setServicesMenu(self.previous_services.as_deref());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::ClassType;

    // Runtime-only fixture classes: no NSApplication instance, menu, worker,
    // window, input capture or system termination is created by these tests.
    define_class!(
        #[unsafe(super(NSObject))]
        struct PeerGlideEmptyTerminationFixture;
        // SAFETY: only NSObject behavior is inherited by this fixture.
        unsafe impl NSObjectProtocol for PeerGlideEmptyTerminationFixture {}
    );
    define_class!(
        #[unsafe(super(NSObject))]
        struct PeerGlideForeignTerminationFixture;
        impl PeerGlideForeignTerminationFixture {
            #[unsafe(method(applicationShouldTerminate:))]
            fn foreign_termination(&self, _sender: &NSApplication) -> NSApplicationTerminateReply {
                NSApplicationTerminateReply::TerminateNow
            }
        }
        // SAFETY: inherit NSObject and add only the SDK-compatible method above.
        unsafe impl NSObjectProtocol for PeerGlideForeignTerminationFixture {}
    );

    #[test]
    fn missing_selector_is_added_once_and_has_the_checked_abi() {
        let class = PeerGlideEmptyTerminationFixture::class();
        assert!(class
            .instance_method(sel!(applicationShouldTerminate:))
            .is_none());
        install_bridge(class).unwrap();
        assert!(check_bridge(class));
        install_bridge(class).unwrap();
        // Invalid/unregistered instances cannot dispatch to any owner. Never
        // call NSApplication.terminate: or a real delegate in this regression.
        // SAFETY: callback treats both pointers as identity values only; it
        // never dereferences them, and no registration accepts these null IDs.
        let result = unsafe {
            should_terminate(
                std::ptr::null_mut(),
                sel!(applicationShouldTerminate:),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, NSApplicationTerminateReply::TerminateCancel);
    }
    #[test]
    fn existing_foreign_termination_implementation_is_never_overwritten() {
        let class = PeerGlideForeignTerminationFixture::class();
        let selector = sel!(applicationShouldTerminate:);
        let original = class.instance_method(selector).unwrap().implementation();
        assert!(install_bridge(class).is_err());
        assert!(std::ptr::fn_addr_eq(
            original,
            class.instance_method(selector).unwrap().implementation()
        ));
        assert!(!check_bridge(class));
    }
}
