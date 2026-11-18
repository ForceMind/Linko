//! Text copied from the diagnostics page. Only facts this computer knows:
//! no host name, certificate fingerprint, address or input content.
use super::*;
use peerglide_platform::input_diagnostics::{is_held_line, is_summary, PEER_PREFIX};

const MAX_EVENTS: usize = 5;

/// Newest diagnostic lines of the current worker. Lines from the other
/// computer are shown as plain text only; nothing here is saved or sent.
#[derive(Clone, Default)]
pub(super) struct Latest {
    pub summary: Option<String>,
    pub held: Option<String>,
    pub events: Vec<String>,
    pub peer_summary: Option<String>,
    pub peer_held: Option<String>,
    pub peer_events: Vec<String>,
}

/// What one stderr line did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Seen {
    /// Not a diagnostic line; it goes to the log as before.
    Other,
    /// Stored and also kept in the log.
    Logged,
    /// Stored only, so per-second lines do not fill the log.
    Quiet,
}

fn push_event(events: &mut Vec<String>, line: &str) {
    if events.len() < MAX_EVENTS {
        events.push(line.to_owned());
    }
}

impl Latest {
    /// Every line is validated again here; the worker already checked
    /// lines from the other computer before repeating them.
    pub(super) fn observe(&mut self, line: &str) -> Seen {
        if let Some(peer) = line.strip_prefix(PEER_PREFIX) {
            if is_summary(peer) {
                self.peer_summary = Some(peer.to_owned());
            } else if is_held_line(peer, true) {
                self.peer_held = Some(peer.to_owned());
            } else if is_held_line(peer, false) {
                push_event(&mut self.peer_events, peer);
            }
            return Seen::Quiet;
        }
        if is_summary(line) {
            self.summary = Some(line.to_owned());
            Seen::Logged
        } else if is_held_line(line, true) {
            self.held = Some(line.to_owned());
            Seen::Quiet
        } else if is_held_line(line, false) {
            push_event(&mut self.events, line);
            Seen::Logged
        } else {
            Seen::Other
        }
    }

    pub(super) fn local_lines(&self) -> Vec<&str> {
        let mut lines = Vec::new();
        lines.extend(self.summary.as_deref());
        lines.extend(self.held.as_deref());
        lines.extend(self.events.iter().map(String::as_str));
        lines
    }

    pub(super) fn peer_lines(&self) -> Vec<&str> {
        let mut lines = Vec::new();
        lines.extend(self.peer_summary.as_deref());
        lines.extend(self.peer_held.as_deref());
        lines.extend(self.peer_events.iter().map(String::as_str));
        lines
    }
}

pub(super) struct Facts {
    pub version: &'static str,
    pub system: String,
    pub edge: &'static str,
    pub receive: bool,
    pub input: &'static str,
}

pub(super) fn header(facts: &Facts) -> String {
    let receive = if facts.receive { "已允许" } else { "关闭" };
    let lines = [
        format!("屏跃诊断说明 · 应用版本 {}", facts.version),
        format!("系统：{}", facts.system),
        format!("本机方向：另一台在本机{}", facts.edge),
        format!("本机接收控制：{receive}"),
        format!("本机键鼠：{}", facts.input),
    ];
    lines.join("\n")
}

/// Header followed by this computer's unchanged diagnostic lines.
pub(super) fn local_text(header: &str, latest: &Latest) -> String {
    let mut lines = vec![header];
    lines.extend(latest.local_lines());
    lines.join("\n")
}

/// One text with both computers. The header states only local facts.
pub(super) fn both_text(header: &str, latest: &Latest) -> String {
    let mut lines = vec![header, "本机："];
    lines.extend(latest.local_lines());
    lines.push("另一台：");
    let peer = latest.peer_lines();
    if peer.is_empty() {
        lines.push(NO_PEER);
    }
    lines.extend(peer);
    lines.join("\n")
}

pub(super) const NO_PEER: &str = "另一台未提供诊断";

impl App {
    pub(super) fn diagnostic_facts(&self) -> Facts {
        Facts {
            version: shell_ui::DISPLAY_VERSION,
            system: system_label(),
            edge: edge_label(self.displayed_edge()),
            receive: !self.routed_active && self.config.allow,
            input: self.input_label(),
        }
    }
}

fn system_label() -> String {
    let name = match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    match os_version().filter(|version| plain_version(version)) {
        Some(version) => format!("{name} {version}"),
        None => format!("{name}（版本未知）"),
    }
}

fn plain_version(text: &str) -> bool {
    let allowed = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+');
    !text.is_empty() && text.len() <= 32 && text.bytes().all(allowed)
}

#[cfg(target_os = "macos")]
fn os_version() -> Option<String> {
    use std::{ffi::c_void, ptr};
    #[link(name = "System")]
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const std::ffi::c_char,
            output: *mut c_void,
            length: *mut usize,
            new_value: *mut c_void,
            new_length: usize,
        ) -> i32;
    }
    let mut bytes = [0u8; 64];
    let mut length = bytes.len();
    // The product version only, for example 15.1.
    let status = unsafe {
        sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut length,
            ptr::null_mut(),
            0,
        )
    };
    if status != 0 || length == 0 || length > bytes.len() {
        return None;
    }
    let value = &bytes[..length];
    let value = value.strip_suffix(&[0]).unwrap_or(value);
    std::str::from_utf8(value).ok().map(str::to_owned)
}

#[cfg(windows)]
fn os_version() -> Option<String> {
    #[repr(C)]
    struct VersionInfo {
        _size: u32,
        major: u32,
        minor: u32,
        build: u32,
        _platform: u32,
        _service_pack: [u16; 128],
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        #[link_name = "RtlGetVersion"]
        fn rtl_get_version(info: *mut VersionInfo) -> i32;
    }
    let mut info = VersionInfo {
        _size: std::mem::size_of::<VersionInfo>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        _platform: 0,
        _service_pack: [0; 128],
    };
    // Unlike GetVersionEx, this is not limited by the application manifest.
    let status = unsafe { rtl_get_version(&mut info) };
    let (major, minor, build) = (info.major, info.minor, info.build);
    (status == 0).then(|| format!("{major}.{minor}.{build}"))
}

#[cfg(target_os = "linux")]
fn os_version() -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open("/proc/sys/kernel/osrelease").ok()?;
    let mut text = String::new();
    file.take(64).read_to_string(&mut text).ok()?;
    Some(text.trim().to_owned())
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn os_version() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copied_header_states_local_facts_without_identifiers() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.peer_name = "Studio-Mac".into();
        app.config.address = "192.0.2.7:47000".into();
        app.config.fingerprint = Some("ab12cd34ef56".into());
        app.config.edge = "left".into();
        app.config.allow = true;
        let latest = Latest {
            summary: Some("KVM-DIAG/5 end".into()),
            ..Latest::default()
        };
        let text = local_text(&header(&app.diagnostic_facts()), &latest);
        assert!(text.contains(shell_ui::DISPLAY_VERSION), "{text}");
        assert!(text.contains("本机方向：另一台在本机左侧"), "{text}");
        assert!(text.contains("本机接收控制：已允许"), "{text}");
        assert!(text.contains("本机键鼠：尚未开启键鼠会话"), "{text}");
        assert!(text.contains("系统："), "{text}");
        assert!(text.ends_with("\nKVM-DIAG/5 end"), "{text}");
        for private in ["Studio-Mac", "192.0.2.7", "47000", "ab12cd34ef56"] {
            assert!(!text.contains(private), "{text}");
        }
        app.routed_active = true;
        let routed = header(&app.diagnostic_facts());
        assert!(routed.contains("本机接收控制：关闭"), "{routed}");
        drop(app);
        fixture.close()
    }

    #[test]
    fn peer_lines_are_kept_apart_and_copied_with_both_ends() {
        use peerglide_platform::input_diagnostics::{HeldCause, InputDiagnostics, Phase};
        let d = InputDiagnostics::default();
        let summary = d.snapshot(Phase::Sample).to_string();
        let sample = d.held_report(HeldCause::Sample, 0, false, false);
        let sample = sample.to_string();
        let event = d.held_report(HeldCause::BlockGate, 1, false, false);
        let event = event.to_string();
        let peer_summary = format!("{PEER_PREFIX}{summary}");
        let peer_event = format!("{PEER_PREFIX}{event}");
        let forged = format!("{PEER_PREFIX}KVM-HELD/1 sample key=4");
        let mut latest = Latest::default();
        assert_eq!(latest.observe(&summary), Seen::Logged);
        assert_eq!(latest.observe(&sample), Seen::Quiet);
        assert_eq!(latest.observe(&event), Seen::Logged);
        assert_eq!(latest.observe(&peer_summary), Seen::Quiet);
        assert_eq!(latest.observe(&peer_event), Seen::Quiet);
        assert_eq!(latest.observe(&forged), Seen::Quiet);
        assert_eq!(latest.observe("ordinary log line"), Seen::Other);
        assert_eq!(latest.summary.as_deref(), Some(summary.as_str()));
        assert_eq!(latest.peer_summary.as_deref(), Some(summary.as_str()));
        assert_eq!(latest.peer_held, None);
        assert_eq!(latest.peer_events, [event.clone()]);

        let text = both_text("H", &latest);
        let local = format!("{summary}\n{sample}\n{event}");
        let peer = format!("{summary}\n{event}");
        assert_eq!(text, format!("H\n本机：\n{local}\n另一台：\n{peer}"));
        for _ in 0..10 {
            latest.observe(&event);
        }
        assert_eq!(latest.events.len(), MAX_EVENTS);

        let alone = Latest {
            summary: Some(summary),
            ..Latest::default()
        };
        let text = both_text("H", &alone);
        assert!(text.ends_with(&format!("另一台：\n{NO_PEER}")), "{text}");
        assert!(alone.peer_lines().is_empty());
    }

    #[test]
    fn reported_system_versions_are_plain_tokens() {
        assert!(plain_version("15.1"));
        assert!(plain_version("10.0.26100"));
        assert!(plain_version("6.8.0-45-generic"));
        assert!(!plain_version(""));
        assert!(!plain_version("15.1\nhost"));
        assert!(!plain_version("15 1"));
        assert!(!plain_version(&"9".repeat(33)));
    }
}
