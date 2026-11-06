//! Presentation of current facts. A label or button never grants input authority.
use super::worker_permissions::Readiness;
use super::{desktop_style::Tone, SessionMode, WorkerInputState};
use peerglide_protocol::worker::WorkerSwitchState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    AddDevice,
    StartInput,
    CheckPermissions,
    Connect,
    Recovery,
    None,
}
impl Action {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::AddDevice => "添加另一台电脑",
            Self::StartInput => "开启键鼠协同…",
            Self::CheckPermissions => "检查本机键鼠权限",
            Self::Connect => "连接电脑 · 仅通信",
            Self::Recovery => "查看恢复指引",
            Self::None => "等待当前操作完成",
        }
    }
}

pub(super) struct Snapshot {
    pub registered: bool,
    pub writable: bool,
    pub running: bool,
    pub connected: bool,
    pub failed: bool,
    pub stopping: bool,
    pub recovery_blocked: bool,
    pub terminal: bool,
    pub mode: Option<SessionMode>,
    pub input: Option<WorkerInputState>,
    pub native_supported: bool,
    pub pairing: bool,
    pub pairing_pending: bool,
    pub readiness: Readiness,
    pub switch_state: Option<WorkerSwitchState>,
    pub waiting_for_peer: bool,
    pub peer_wait_expired: bool,
}
pub(super) struct View {
    pub title: &'static str,
    pub detail: &'static str,
    pub tone: Tone,
    pub action: Action,
}
impl View {
    fn new(title: &'static str, detail: &'static str, tone: Tone, action: Action) -> Self {
        Self {
            title,
            detail,
            tone,
            action,
        }
    }
}

pub(super) fn present(s: Snapshot) -> View {
    if s.recovery_blocked || s.input == Some(WorkerInputState::RecoveryUnconfirmed) {
        return View::new(
            "输入恢复需要确认",
            "先松开两端的按键和鼠标按钮，再查看恢复指引。确认前不开始新的键鼠会话。",
            Tone::Danger,
            Action::Recovery,
        );
    }
    if s.stopping || s.running && s.terminal {
        return View::new(
            "正在结束协同",
            "正在等待输入清理和连接结束，请保留窗口。",
            Tone::Warning,
            Action::Recovery,
        );
    }
    if s.running && !s.registered {
        return View::new(
            "正在断开旧设备",
            "旧连接结束后可以重新添加电脑。",
            Tone::Warning,
            Action::Recovery,
        );
    }
    if !s.running {
        if !s.writable {
            return View::new(
                "这个窗口只读",
                "你的资料已保留。请到设置查看原因；停止和恢复仍可使用。",
                Tone::Warning,
                Action::None,
            );
        }
        if s.pairing {
            return View::new(
                "正在添加电脑",
                "请在配对窗口核验身份；不会开启键鼠或数据同步。",
                Tone::Accent,
                Action::None,
            );
        }
        if s.pairing_pending {
            return View::new(
                "登记等待对端回执",
                "本机记录已保留，尚不能开始协同。请重新核验或撤销待确认登记。",
                Tone::Warning,
                Action::AddDevice,
            );
        }
        if !s.registered {
            return View::new(
                "连接你的另一台电脑",
                "添加并核验一次，之后会记住这台电脑。",
                Tone::Accent,
                Action::AddDevice,
            );
        }
        if s.peer_wait_expired {
            return View::new("等待另一台电脑已超时", "配对已保留。请在另一台也开启协同，再重新发起；不会自动重试或恢复旧控制。", Tone::Warning,
                if s.native_supported { native_action(s.readiness) } else { Action::Connect });
        }
        if !s.failed && s.native_supported && s.readiness != Readiness::Ready {
            return permission_view(s.readiness, false);
        }
        return View::new(
            if s.failed {
                "上次连接没有完成"
            } else {
                "设备已记住，尚未连接"
            },
            if s.native_supported && s.readiness != Readiness::Ready {
                s.readiness.detail()
            } else if s.failed {
                "检查另一台电脑是否已打开，或到设置检查键鼠权限；详细原因在高级连接里。"
            } else {
                "按桌面上的位置摆好屏幕，再明确开启键鼠协同。"
            },
            if s.failed {
                Tone::Warning
            } else {
                Tone::Neutral
            },
            if s.native_supported {
                native_action(s.readiness)
            } else {
                Action::Connect
            },
        );
    }
    if s.mode == Some(SessionMode::Probe) {
        return View::new(
            "正在检查连接",
            "只检测两台电脑能否通信，不读取或控制键鼠。",
            Tone::Accent,
            Action::None,
        );
    }
    if s.mode == Some(SessionMode::Native) && s.input == Some(WorkerInputState::Recovering) {
        return View::new(
            "正在恢复本机控制",
            "请先松开按键和鼠标按钮，等待清理结果。",
            Tone::Warning,
            Action::Recovery,
        );
    }
    if s.connected
        && !s.failed
        && s.writable
        && s.native_supported
        && s.mode == Some(SessionMode::Communication)
        && s.readiness != Readiness::Ready
    {
        return permission_view(s.readiness, s.connected);
    }
    let next = if s.writable && s.native_supported && s.mode == Some(SessionMode::Communication) {
        native_action(s.readiness)
    } else {
        Action::None
    };
    if !s.connected {
        if s.failed {
            return View::new(
                "连接未完成，正在结束",
                "保留当前窗口，等连接结束后再检查权限或重试。",
                Tone::Warning,
                Action::None,
            );
        }
        if s.waiting_for_peer {
            return View::new("等待另一台电脑就绪", "请在另一台也开启协同。本次等待约一分钟，可随时停止；连接前不会捕获或转发输入。", Tone::Accent, Action::None);
        }
        return View::new(
            "正在等待两台电脑连接",
            "请保持另一台电脑的屏跃已打开。连接中断时不会继续转发旧输入。",
            Tone::Accent,
            next,
        );
    }
    if s.mode != Some(SessionMode::Native) {
        return View::new(
            "已连接，键鼠尚未开启",
            "连接不会自动授权控制。下一步可单独开启键鼠协同。",
            Tone::Good,
            next,
        );
    }
    if s.input == Some(WorkerInputState::Local) {
        let feedback = match s.switch_state {
            Some(WorkerSwitchState::Pending) => Some((
                "正在请求切到另一台电脑",
                "当前仍在本机，等待本次切换确认。",
                Tone::Accent,
            )),
            Some(WorkerSwitchState::Denied) => Some((
                "上次切换未被对端接受",
                "请在另一台检查接收控制、键鼠开启和恢复提示；鼠标移回屏幕内再重试。",
                Tone::Warning,
            )),
            Some(WorkerSwitchState::TimedOut) => Some((
                "上次切换没有完成",
                "本机未在期限内完成切换。先松开按键和鼠标按钮，将鼠标移回屏幕内再重试。",
                Tone::Warning,
            )),
            Some(WorkerSwitchState::PeerUnavailable) => Some((
                "对端未提供鼠标能力",
                "请在另一台检查键鼠是否已开启及平台支持情况。提供能力不等于允许接收控制。",
                Tone::Warning,
            )),
            _ => None,
        };
        if let Some((title, detail, tone)) = feedback {
            return View::new(title, detail, tone, Action::None);
        }
    }
    match s.input {
        Some(WorkerInputState::ControllingRemote) => View::new(
            "正在控制另一台电脑",
            "键盘跟随当前目标。Ctrl+Alt+Esc 可紧急恢复本机控制。",
            Tone::Accent,
            Action::None,
        ),
        Some(WorkerInputState::ReceivingRemote) => View::new(
            "另一台电脑正在控制本机",
            "本机首次物理动作只用于收回控制；松开按键和鼠标按钮后再继续操作。",
            Tone::Accent,
            Action::None,
        ),
        Some(WorkerInputState::Local) => View::new(
            "键鼠已开启 · 当前在本机",
            "移到设置的屏幕边缘可请求切换；是否允许控制由另一台电脑决定。",
            Tone::Good,
            Action::None,
        ),
        _ => View::new(
            "已连接，正在准备键鼠",
            "输入程序仍需确认权限与当前归属，尚未报告可以切换。",
            Tone::Warning,
            Action::None,
        ),
    }
}
fn native_action(readiness: Readiness) -> Action {
    match readiness {
        Readiness::Ready => Action::StartInput,
        Readiness::Checking => Action::None,
        _ => Action::CheckPermissions,
    }
}
fn permission_view(readiness: Readiness, connected: bool) -> View {
    View::new(
        if readiness == Readiness::Checking {
            "正在检查本机键鼠权限"
        } else if connected {
            "已连接，键鼠权限需要检查"
        } else {
            "开始前，先检查本机键鼠权限"
        },
        readiness.detail(),
        if matches!(readiness, Readiness::Unchecked | Readiness::Checking) {
            Tone::Accent
        } else {
            Tone::Warning
        },
        native_action(readiness),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Snapshot {
        Snapshot {
            registered: true,
            writable: true,
            running: false,
            connected: false,
            failed: false,
            stopping: false,
            recovery_blocked: false,
            terminal: false,
            mode: None,
            input: None,
            native_supported: true,
            pairing: false,
            pairing_pending: false,
            readiness: Readiness::Ready,
            switch_state: None,
            waiting_for_peer: false,
            peer_wait_expired: false,
        }
    }
    #[test]
    fn pairing_busy_and_pending_are_not_mislabeled_as_readonly_or_ready() {
        let mut s = snapshot();
        s.pairing = true;
        let view = present(s);
        assert_eq!(view.title, "正在添加电脑");
        assert_eq!(view.action, Action::None);
        let mut s = snapshot();
        s.pairing_pending = true;
        let view = present(s);
        assert_eq!(view.title, "登记等待对端回执");
        assert_eq!(view.action, Action::AddDevice);
        let mut s = snapshot();
        s.pairing_pending = true;
        s.writable = false;
        assert_eq!(present(s).title, "这个窗口只读");
    }
    #[test]
    fn dead_or_disconnected_process_cannot_present_stale_remote_control() {
        for input in [
            WorkerInputState::ControllingRemote,
            WorkerInputState::ReceivingRemote,
        ] {
            let mut s = snapshot();
            s.input = Some(input);
            s.connected = true;
            s.mode = Some(SessionMode::Native);
            let view = present(s);
            assert_eq!(view.action, Action::StartInput);
            assert_eq!(view.title, "设备已记住，尚未连接");
            let mut s = snapshot();
            s.running = true;
            s.input = Some(input);
            s.mode = Some(SessionMode::Native);
            assert_eq!(present(s).title, "正在等待两台电脑连接");
        }
    }
    #[test]
    fn authentication_does_not_imply_input_or_remote_receiving_permission() {
        let mut s = snapshot();
        s.running = true;
        s.connected = true;
        s.mode = Some(SessionMode::Communication);
        s.input = Some(WorkerInputState::Disabled);
        let view = present(s);
        assert_eq!(view.title, "已连接，键鼠尚未开启");
        assert_eq!(view.action, Action::StartInput);
        let mut s = snapshot();
        s.running = true;
        s.connected = true;
        s.mode = Some(SessionMode::Native);
        s.input = Some(WorkerInputState::Unavailable);
        assert_eq!(present(s).title, "已连接，正在准备键鼠");
    }
    #[test]
    fn recovery_and_terminal_receipt_keep_the_start_action_unavailable() {
        for state in [
            WorkerInputState::Local,
            WorkerInputState::Disabled,
            WorkerInputState::ControllingRemote,
        ] {
            let mut s = snapshot();
            s.running = true;
            s.connected = true;
            s.mode = Some(SessionMode::Native);
            s.terminal = true;
            s.input = Some(state);
            assert_eq!(present(s).action, Action::Recovery);
        }
        let mut s = snapshot();
        s.recovery_blocked = true;
        assert_eq!(present(s).action, Action::Recovery);
        let mut s = snapshot();
        s.input = Some(WorkerInputState::RecoveryUnconfirmed);
        assert_eq!(present(s).action, Action::Recovery);
    }
    #[test]
    fn probe_readonly_and_unsupported_platform_never_offer_native_start() {
        let mut s = snapshot();
        s.running = true;
        s.mode = Some(SessionMode::Probe);
        assert_eq!(present(s).action, Action::None);
        let mut s = snapshot();
        s.writable = false;
        assert_eq!(present(s).action, Action::None);
        let mut s = snapshot();
        s.native_supported = false;
        assert_eq!(present(s).action, Action::Connect);
        let mut s = snapshot();
        s.registered = false;
        assert_eq!(present(s).action, Action::AddDevice);
    }
    #[test]
    fn connection_failure_is_visible_without_opening_technical_diagnostics() {
        let mut s = snapshot();
        s.failed = true;
        assert_eq!(present(s).title, "上次连接没有完成");
        let mut s = snapshot();
        s.failed = true;
        s.running = true;
        s.mode = Some(SessionMode::Native);
        assert_eq!(present(s).title, "连接未完成，正在结束");
        let mut s = snapshot();
        s.failed = true;
        s.recovery_blocked = true;
        assert_eq!(present(s).action, Action::Recovery);
    }
    #[test]
    fn local_permission_hints_never_override_recovery_failure_or_active_input() {
        for readiness in [
            Readiness::Unchecked,
            Readiness::Checking,
            Readiness::Accessibility,
            Readiness::InputMonitoring,
            Readiness::InputDesktop,
            Readiness::SecureInput,
            Readiness::Unknown,
            Readiness::Unsupported,
            Readiness::Failed,
        ] {
            let mut s = snapshot();
            s.readiness = readiness;
            let view = present(s);
            assert_eq!(
                view.action,
                if readiness == Readiness::Checking {
                    Action::None
                } else {
                    Action::CheckPermissions
                }
            );
            assert_eq!(view.detail, readiness.detail());
            let mut s = snapshot();
            s.readiness = readiness;
            s.recovery_blocked = true;
            assert_eq!(present(s).action, Action::Recovery);
            let mut s = snapshot();
            s.readiness = readiness;
            s.failed = true;
            assert_eq!(present(s).title, "上次连接没有完成");
            let mut s = snapshot();
            s.readiness = readiness;
            s.running = true;
            s.failed = true;
            s.mode = Some(SessionMode::Communication);
            assert_eq!(present(s).title, "连接未完成，正在结束");
            let mut s = snapshot();
            s.readiness = readiness;
            s.running = true;
            s.connected = true;
            s.mode = Some(SessionMode::Native);
            s.input = Some(WorkerInputState::ControllingRemote);
            assert_eq!(present(s).title, "正在控制另一台电脑");
        }
    }
    #[test]
    fn switch_feedback_only_describes_a_connected_native_local_route() {
        for switch in [
            WorkerSwitchState::Pending,
            WorkerSwitchState::Denied,
            WorkerSwitchState::TimedOut,
            WorkerSwitchState::PeerUnavailable,
        ] {
            let mut s = snapshot();
            s.running = true;
            s.connected = true;
            s.mode = Some(SessionMode::Native);
            s.input = Some(WorkerInputState::Local);
            s.switch_state = Some(switch);
            let view = present(s);
            assert_eq!(view.action, Action::None);
            assert!(!view.title.contains("已开启"));
            let mut s = snapshot();
            s.switch_state = Some(switch);
            assert_eq!(present(s).title, "设备已记住，尚未连接");
            let mut s = snapshot();
            s.running = true;
            s.connected = true;
            s.mode = Some(SessionMode::Native);
            s.input = Some(WorkerInputState::ControllingRemote);
            s.switch_state = Some(switch);
            assert_eq!(present(s).title, "正在控制另一台电脑");
            let mut s = snapshot();
            s.running = true;
            s.connected = true;
            s.mode = Some(SessionMode::Native);
            s.input = Some(WorkerInputState::RecoveryUnconfirmed);
            s.switch_state = Some(switch);
            assert_eq!(present(s).action, Action::Recovery);
        }
    }
    #[test]
    fn first_wait_and_expiry_are_not_authentication_or_recovery() {
        let mut s = snapshot(); s.running = true; s.waiting_for_peer = true;
        s.mode = Some(SessionMode::Native); s.input = Some(WorkerInputState::Disabled);
        let view = present(s);
        assert_eq!(view.title, "等待另一台电脑就绪");
        assert_eq!(view.action, Action::None);
        let mut s = snapshot(); s.peer_wait_expired = true; s.failed = true;
        assert_eq!(present(s).title, "等待另一台电脑已超时");
        let mut s = snapshot(); s.peer_wait_expired = true; s.recovery_blocked = true;
        assert_eq!(present(s).action, Action::Recovery);
        let mut s = snapshot(); s.peer_wait_expired = true; s.registered = false;
        assert_eq!(present(s).action, Action::AddDevice);
    }
}
