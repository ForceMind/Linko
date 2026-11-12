//! Worker-owned X11 pointer and keyboard injection. No product caller or capture hook yet.
use crate::ReceivePoint;
use anyhow::{ensure, Context, Result};
use x11rb::{
    connection::Connection as _,
    protocol::{
        xproto::{
            ConnectionExt as _, Window, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT,
            KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};

#[path = "x11_keymap.rs"]
mod keymap;
use keymap::KeyMap;

fn submit_owned(slot: &mut bool, down: bool, submit: impl FnOnce() -> Result<()>) -> Result<bool> {
    if !down && !*slot {
        return Ok(false);
    }
    if down {
        *slot = true;
    }
    submit()?;
    *slot = down;
    Ok(true)
}

struct Buttons {
    owned: [bool; 3],
}

impl Buttons {
    fn new() -> Self {
        Self { owned: [false; 3] }
    }

    /// Keep uncertain submissions owned until a matching release succeeds.
    fn apply(
        &mut self,
        button: u8,
        down: bool,
        submit: impl FnOnce(u8, bool) -> Result<()>,
    ) -> Result<bool> {
        let index = button
            .checked_sub(1)
            .map(usize::from)
            .filter(|index| *index < self.owned.len())
            .context("unsupported X11 button")?;
        submit_owned(&mut self.owned[index], down, || {
            submit([1, 3, 2][index], down)
        })
    }

    fn release(&mut self, mut submit: impl FnMut(u8, bool) -> Result<()>) -> Result<()> {
        let mut failure = None;
        for button in 1..=3 {
            if let Err(error) = self.apply(button, false, &mut submit) {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

/// Owns the X11 connection and its submitted buttons. The local worker must
/// stop and reconcile this owner before discarding it or accepting a new lease.
pub struct Injector {
    connection: RustConnection,
    root: Window,
    width: u16,
    height: u16,
    buttons: Buttons,
    keyboard: Option<KeyMap>,
    owned_keys: [bool; 256],
}

impl Injector {
    pub fn open_after_explicit_consent() -> Result<Self> {
        let (connection, screen) = local_connection()?;
        let version = connection
            .xtest_get_version(2, 2)
            .map_err(|_| anyhow::anyhow!("X11 XTEST extension unavailable"))?
            .reply()
            .map_err(|_| anyhow::anyhow!("X11 XTEST version unavailable"))?;
        ensure!(version.major_version >= 2, "unsupported X11 XTEST version");
        let root = connection
            .setup()
            .roots
            .get(screen)
            .context("local X11 root unavailable")?
            .root;
        let geometry = connection
            .get_geometry(root)?
            .reply()
            .map_err(|_| anyhow::anyhow!("local X11 root geometry unavailable"))?;
        ensure!(
            coordinates(
                ReceivePoint { x: 0.0, y: 0.0 },
                geometry.width,
                geometry.height
            )
            .is_some(),
            "X11 root exceeds XTEST coordinate limits"
        );
        let keyboard = KeyMap::load(&connection).ok();
        Ok(Self {
            connection,
            root,
            width: geometry.width,
            height: geometry.height,
            buttons: Buttons::new(),
            keyboard,
            owned_keys: [false; 256],
        })
    }

    pub fn pointer(&self, point: ReceivePoint) -> Result<()> {
        self.pointer_if(point, || Ok(true)).map(|_| ())
    }

    /// Recheck local ownership after the geometry reply, before submitting XTEST.
    pub fn pointer_if(&self, point: ReceivePoint, admit: impl FnOnce() -> Result<bool>) -> Result<bool> {
        let (x, y) =
            coordinates(point, self.width, self.height).context("invalid X11 pointer position")?;
        // Refuse a stale geometry instead of interpreting a position in a new root.
        let geometry = self.connection.get_geometry(self.root)?.reply()?;
        ensure!(
            geometry.width == self.width && geometry.height == self.height,
            "X11 root geometry changed"
        );
        if !admit()? {
            return Ok(false);
        }
        post(&self.connection, self.root, MOTION_NOTIFY_EVENT, 0, x, y)?;
        Ok(true)
    }

    pub fn button(&mut self, button: u8, down: bool) -> Result<bool> {
        let root = self.root;
        let connection = &self.connection;
        self.buttons.apply(button, down, |button, down| {
            post_button(connection, root, button, down)
        })
    }

    pub fn key_usages(&self) -> impl Iterator<Item = u16> + '_ {
        self.keyboard.iter().flat_map(|keyboard| keyboard.usages())
    }

    pub fn key(&mut self, usage: u16, down: bool) -> Result<bool> {
        let code = self
            .keyboard
            .as_ref()
            .and_then(|keyboard| keyboard.code(usage))
            .context("unsupported X11 keyboard position")?;
        let connection = &self.connection;
        let root = self.root;
        submit_owned(&mut self.owned_keys[usize::from(code)], down, || {
            post_key(connection, root, code, down)
        })
    }

    pub fn release_owned(&mut self) -> Result<()> {
        let mut result = self.release_buttons();
        for (code, slot) in self.owned_keys.iter_mut().enumerate() {
            let release = submit_owned(slot, false, || {
                post_key(&self.connection, self.root, code as u8, false)
            });
            if let Err(error) = release {
                if result.is_ok() {
                    result = Err(error);
                }
            }
        }
        result
    }

    pub fn release_buttons(&mut self) -> Result<()> {
        let root = self.root;
        let connection = &self.connection;
        self.buttons
            .release(|button, down| post_button(connection, root, button, down))
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        let _ = self.release_owned();
    }
}

pub(super) fn local_connection() -> Result<(RustConnection, usize)> {
    ensure!(
        std::env::var_os("WAYLAND_DISPLAY").is_none()
            && std::env::var("XDG_SESSION_TYPE").ok().as_deref() != Some("wayland"),
        "X11 injection is unavailable in a Wayland session"
    );
    let display = std::env::var("DISPLAY").context("local X11 display unavailable")?;
    ensure!(
        local_display(&display),
        "only a local X11 display is supported"
    );
    x11rb::connect(Some(&display))
        .map_err(|_| anyhow::anyhow!("could not connect to the local X11 display"))
}

fn local_display(display: &str) -> bool {
    let Some(name) = display.strip_prefix(':') else {
        return false;
    };
    let (number, screen) = name.split_once('.').unwrap_or((name, "0"));
    [number, screen].into_iter().all(|field| {
        !field.is_empty()
            && field.bytes().all(|byte| byte.is_ascii_digit())
            && field.parse::<u16>().is_ok()
    })
}

fn coordinates(point: ReceivePoint, width: u16, height: u16) -> Option<(i16, i16)> {
    if !point.valid() {
        return None;
    }
    let last_x = i16::try_from(width.checked_sub(1)?).ok()?;
    let last_y = i16::try_from(height.checked_sub(1)?).ok()?;
    Some((
        (point.x * f64::from(last_x)).round() as i16,
        (point.y * f64::from(last_y)).round() as i16,
    ))
}

fn post_key(connection: &RustConnection, root: Window, key: u8, down: bool) -> Result<()> {
    let kind = if down {
        KEY_PRESS_EVENT
    } else {
        KEY_RELEASE_EVENT
    };
    post(connection, root, kind, key, 0, 0)
}

fn post_button(connection: &RustConnection, root: Window, button: u8, down: bool) -> Result<()> {
    let kind = if down {
        BUTTON_PRESS_EVENT
    } else {
        BUTTON_RELEASE_EVENT
    };
    post(connection, root, kind, button, 0, 0)
}

fn post(
    connection: &RustConnection,
    root: Window,
    kind: u8,
    detail: u8,
    x: i16,
    y: i16,
) -> Result<()> {
    connection
        .xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, root, x, y, 0)
        .map_err(|_| anyhow::anyhow!("X11 input submission failed"))?
        .check()
        .map_err(|_| anyhow::anyhow!("X11 input result unavailable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_display_selection_rejects_tcp_and_unbounded_values() {
        for name in [":0", ":42.1", ":99.0"] {
            assert!(local_display(name));
        }
        for name in ["", "server:0", ":65536", ":1.", ":+1", ":2.3.4"] {
            assert!(!local_display(name));
        }
    }

    #[test]
    fn pointer_uses_exact_root_bounds_without_wrap() {
        assert_eq!(
            coordinates(ReceivePoint { x: 1.0, y: 0.5 }, 1920, 1081),
            Some((1919, 540))
        );
        assert!(coordinates(
            ReceivePoint {
                x: f64::NAN,
                y: 0.0
            },
            1920,
            1080
        )
        .is_none());
        assert!(coordinates(ReceivePoint { x: 1.0, y: 1.0 }, 32769, 1080).is_none());
        assert!(coordinates(ReceivePoint { x: 1.0, y: 1.0 }, 0, 1080).is_none());
    }

    #[test]
    fn uncertain_owned_key_releases_only_after_confirmed_submission() -> Result<()> {
        let mut owned = false;
        assert!(!submit_owned(&mut owned, false, || unreachable!())?);
        assert!(submit_owned(&mut owned, true, || anyhow::bail!("test down error")).is_err());
        assert!(owned);
        assert!(submit_owned(&mut owned, false, || anyhow::bail!("test up error")).is_err());
        assert!(owned);
        assert!(submit_owned(&mut owned, false, || Ok(()))?);
        assert!(!owned);
        Ok(())
    }

    #[test]
    fn cleanup_releases_only_owned_buttons_and_retains_uncertain_release() -> Result<()> {
        let mut buttons = Buttons::new();
        assert!(!buttons.apply(1, false, |_, _| unreachable!())?);
        assert!(buttons
            .apply(2, true, |button, down| {
                assert_eq!((button, down), (3, true));
                anyhow::bail!("test unknown submission")
            })
            .is_err());
        assert!(buttons.apply(3, true, |button, down| {
            assert_eq!((button, down), (2, true));
            Ok(())
        })?);
        let mut released = Vec::new();
        assert!(buttons
            .release(|button, down| {
                released.push((button, down));
                ensure!(button != 3, "test release failed");
                Ok(())
            })
            .is_err());
        assert_eq!(released, [(3, false), (2, false)]);
        released.clear();
        buttons.release(|button, down| {
            released.push((button, down));
            Ok(())
        })?;
        assert_eq!(released, [(3, false)]);
        buttons.release(|_, _| unreachable!())?;
        Ok(())
    }
}
