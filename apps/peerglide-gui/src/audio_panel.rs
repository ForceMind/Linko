//! Explicit local consent for one bounded, pinned audio job.
use super::{data_access, App};
use anyhow::{ensure, Result};
use eframe::egui;
use std::{
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

const STOP_TIMEOUT: Duration = Duration::from_secs(22);

#[derive(Clone, Copy)]
enum Mode {
    Playback,
    Share,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    System,
    Microphone,
}

impl Source {
    fn port(self) -> u16 {
        match self {
            Self::System => 24874,
            Self::Microphone => 24875,
        }
    }

    fn command(self, mode: Mode) -> &'static str {
        match (self, mode) {
            (Self::System, Mode::Playback) => "play-audio",
            (Self::System, Mode::Share) => "share-system-audio",
            (Self::Microphone, Mode::Playback) => "play-microphone",
            (Self::Microphone, Mode::Share) => "share-microphone",
        }
    }
}

struct Job {
    child: Child,
    pin: data_access::Pin,
    stop_deadline: Option<Instant>,
    forced: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct OutputDevice {
    id: String,
    name: String,
}

#[cfg(windows)]
fn output_devices() -> Result<Vec<OutputDevice>> {
    Ok(peerglide_platform::windows_audio::output_devices()
        .into_iter()
        .map(|device| OutputDevice {
            id: device.id.to_string(),
            name: device.name,
        })
        .collect())
}

#[cfg(target_os = "macos")]
fn output_devices() -> Result<Vec<OutputDevice>> {
    Ok(peerglide_platform::macos_audio::output_devices()?
        .into_iter()
        .map(|device| OutputDevice {
            id: device.uid,
            name: device.name,
        })
        .collect())
}

pub(super) struct Panel {
    job: Option<Job>,
    source: Source,
    system_address: String,
    microphone_address: String,
    volume: u8,
    output: Option<OutputDevice>,
    devices: Vec<OutputDevice>,
    device_result: Option<mpsc::Receiver<Result<Vec<OutputDevice>>>>,
    device_status: Option<&'static str>,
    status: &'static str,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            job: None,
            source: Source::System,
            system_address: String::new(),
            microphone_address: String::new(),
            volume: 100,
            output: None,
            devices: Vec::new(),
            device_result: None,
            device_status: None,
            status: "音频共享已停止",
        }
    }
}

impl Panel {
    pub(super) fn is_active(&self) -> bool {
        self.job.is_some()
    }

    pub(super) fn stop(&mut self) {
        if let Some(job) = self.job.as_mut() {
            job.child.stdin.take();
            job.stop_deadline
                .get_or_insert_with(|| Instant::now() + STOP_TIMEOUT);
            self.status = "正在停止音频并释放系统资源";
        }
    }

    pub(super) fn tick(&mut self, app: &App) {
        self.poll_devices();
        if self.job.as_ref().is_some_and(|job| !job.pin.matches(app)) {
            self.stop();
        }
        let Some(job) = self.job.as_mut() else {
            return;
        };
        match job.child.try_wait() {
            Ok(Some(exit)) => {
                self.status = if job.forced {
                    "音频进程已结束，系统资源释放未确认"
                } else if exit.success() {
                    "音频共享已停止"
                } else {
                    "音频会话结束。请检查两端操作、设备权限和音频格式。"
                };
                self.job = None;
            }
            Ok(None) => {
                if job
                    .stop_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                    && !job.forced
                {
                    job.forced = job.child.kill().is_ok();
                }
            }
            Err(_) => {
                self.stop();
                self.status = "暂时无法确认音频进程状态";
            }
        }
    }

    fn query_devices(&mut self, app: &App) -> Result<()> {
        ensure!(self.device_result.is_none(), "正在读取播放设备");
        let (send, receive) = mpsc::sync_channel(1);
        let wake = app.wake_ui.clone();
        std::thread::Builder::new()
            .name("linko-audio-devices".into())
            .spawn(move || {
                let _ = send.send(output_devices());
                wake();
            })?;
        self.device_result = Some(receive);
        self.device_status = Some("正在读取播放设备");
        Ok(())
    }

    fn poll_devices(&mut self) {
        let Some(result) = self.device_result.as_ref() else {
            return;
        };
        let devices = match result.try_recv() {
            Ok(Ok(devices)) => devices,
            Err(mpsc::TryRecvError::Empty) => return,
            Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                self.devices.clear();
                self.device_result = None;
                self.device_status = Some("读取播放设备失败。请选择系统默认或重试。");
                return;
            }
        };
        self.device_result = None;
        self.device_status = devices.is_empty().then_some("没有找到可选播放设备");
        self.devices = devices;
    }

    fn selected_output(&self) -> Result<Option<&str>> {
        let Some(output) = self.output.as_ref() else {
            return Ok(None);
        };
        ensure!(
            self.device_result.is_none() && self.devices.contains(output),
            "选中的播放设备已不可用或正在刷新，请重新选择"
        );
        Ok(Some(&output.id))
    }

    fn output_controls(&mut self, app: &mut App, ui: &mut egui::Ui) {
        let selection = self
            .output
            .as_ref()
            .map_or("系统默认", |device| {
                if self.devices.contains(device) {
                    device.name.as_str()
                } else {
                    "已选设备不可用"
                }
            })
            .to_owned();
        ui.horizontal_wrapped(|ui| {
            ui.label("播放设备");
            egui::ComboBox::from_id_salt("audio-output-device")
                .selected_text(selection)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.output, None, "系统默认");
                    for device in &self.devices {
                        ui.selectable_value(&mut self.output, Some(device.clone()), &device.name);
                    }
                });
            if ui
                .add_enabled(
                    self.device_result.is_none(),
                    egui::Button::new("刷新播放设备"),
                )
                .clicked()
            {
                if let Err(error) = self.query_devices(app) {
                    app.feedback(Err(error), "");
                }
            }
        });
        if let Some(status) = self.device_status {
            ui.label(status);
        }
    }

    fn start(&mut self, app: &App, mode: Mode) -> Result<()> {
        ensure!(!self.is_active(), "请先停止当前音频会话");
        let (pin, identity, peer) = data_access::Pin::load(app)?;
        let executable = std::env::current_exe()?.with_file_name(if cfg!(windows) {
            "peerglide.exe"
        } else {
            "peerglide"
        });
        ensure!(executable.is_file(), "缺少同目录的 peerglide 通信程序");
        let mut command = Command::new(executable);
        command.arg(self.source.command(mode));
        let status = match mode {
            Mode::Playback => {
                command.args(["--volume", &self.volume.to_string()]);
                if let Some(device) = self.selected_output()? {
                    command.args(["--device", device]);
                }
                match self.source {
                    Source::System => "系统音频接收任务运行中",
                    Source::Microphone => "麦克风接收任务运行中",
                }
            }
            Mode::Share => {
                let target = match self.source {
                    Source::System => &self.system_address,
                    Source::Microphone => &self.microphone_address,
                };
                let address =
                    data_access::send_address(target, &app.config.address, self.source.port())?;
                command.arg(address.to_string());
                match self.source {
                    Source::System => "系统音频发送任务运行中",
                    Source::Microphone => "麦克风发送任务运行中",
                }
            }
        };
        command
            .arg("--identity")
            .arg(app.identity_dir())
            .arg("--peer-cert")
            .arg(app.dir.join("trusted-peer.crt.der"))
            .arg("--managed")
            .args(["--expected-local-fingerprint", &identity.fingerprint()])
            .args([
                "--expected-peer-fingerprint",
                &super::transport::fingerprint(&peer),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        self.job = Some(Job {
            child: command.spawn()?,
            pin,
            stop_deadline: None,
            forced: false,
        });
        self.status = status;
        Ok(())
    }

    pub(super) fn ui(&mut self, app: &mut App, ui: &mut egui::Ui) {
        ui.label(self.status);
        if self.is_active() {
            if ui.button("停止音频").clicked() {
                self.stop();
            }
            return;
        }
        if !data_access::ready(app) {
            ui.label("完成配对后可开启音频共享。");
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.source, Source::System, "系统音频");
            ui.selectable_value(&mut self.source, Source::Microphone, "麦克风");
        });
        ui.label("先在播放端开启接收。每次会话最长 10 分钟。");
        ui.add(
            egui::Slider::new(&mut self.volume, 0..=100)
                .suffix("%")
                .text("播放音量"),
        );
        self.output_controls(app, ui);
        let port = self.source.port();
        ui.label(format!("接收使用 UDP 端口 {port}。"));
        if ui.button("在本机接收并播放").clicked() {
            let result = self.start(app, Mode::Playback);
            app.feedback(result, "已请求启动音频接收。");
        }
        ui.separator();
        ui.label("接收端 IP 地址和端口");
        let (target, button) = match self.source {
            Source::System => (&mut self.system_address, "发送本机系统音频"),
            Source::Microphone => (&mut self.microphone_address, "发送本机麦克风"),
        };
        ui.add(
            egui::TextEdit::singleline(target)
                .char_limit(128)
                .hint_text(format!("留空使用已保存的地址与端口 {port}")),
        );
        if self.source == Source::Microphone {
            ui.label("首次使用麦克风时，系统可能请求录音许可。");
        }
        if ui.button(button).clicked() {
            let result = self.start(app, Mode::Share);
            app.feedback(result, "已请求启动音频发送。");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_and_system_audio_use_distinct_commands_and_ports() {
        assert_eq!(Source::System.port(), 24874);
        assert_eq!(Source::Microphone.port(), 24875);
        assert_eq!(
            Source::Microphone.command(Mode::Playback),
            "play-microphone"
        );
        assert_eq!(Source::Microphone.command(Mode::Share), "share-microphone");
    }

    #[test]
    fn losing_the_selected_device_does_not_select_the_system_default() -> Result<()> {
        let mut panel = Panel::default();
        assert_eq!(panel.selected_output()?, None);
        panel.devices = vec![OutputDevice {
            id: "7".into(),
            name: "Headphones".into(),
        }];
        panel.output = panel.devices.first().cloned();
        assert_eq!(panel.selected_output()?, Some("7"));
        panel.devices[0].name = "Speakers".into();
        assert!(panel.selected_output().is_err());
        panel.devices.clear();
        assert!(panel.selected_output().is_err());
        Ok(())
    }

    #[test]
    fn an_unpaired_profile_cannot_start_either_audio_direction() -> Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let app = App::load(fixture.path.clone());
        let mut panel = Panel::default();
        for source in [Source::System, Source::Microphone] {
            panel.source = source;
            for mode in [Mode::Playback, Mode::Share] {
                assert!(panel.start(&app, mode).is_err());
                assert!(!panel.is_active());
            }
        }
        panel.stop();
        drop(app);
        fixture.close()
    }
}
