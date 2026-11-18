//! Text copied from the diagnostics page. Only facts this computer knows:
//! no host name, certificate fingerprint, address or input content.
use super::*;

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

/// Header followed by the unchanged summary line.
pub(super) fn local_text(header: &str, summary: &str) -> String {
    format!("{header}\n{summary}")
}

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
        let text = local_text(&header(&app.diagnostic_facts()), "KVM-DIAG/5 end");
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
