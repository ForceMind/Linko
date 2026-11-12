//! Explicit local consent for one bounded, pinned system audio job.
use super::{data_access, App};
use anyhow::{ensure, Result};
use eframe::egui;
use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const PORT: u16 = 24874;
const STOP_TIMEOUT: Duration = Duration::from_secs(22);

#[derive(Clone, Copy)]
enum Mode {
    Playback,
    ShareSystem,
}

struct Job {
    child: Child,
    pin: data_access::Pin,
    stop_deadline: Option<Instant>,
    forced: bool,
}

pub(super) struct Panel {
    job: Option<Job>,
    address: String,
    volume: u8,
    status: &'static str,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            job: None,
            address: String::new(),
            volume: 100,
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
                    "音频会话结束。请检查两端操作、设备权限和系统音频格式。"
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
        let status = match mode {
            Mode::Playback => {
                command.args(["play-audio", "--volume", &self.volume.to_string()]);
                "音频接收任务运行中"
            }
            Mode::ShareSystem => {
                let address = data_access::send_address(&self.address, &app.config.address, PORT)?;
                command.arg("share-system-audio").arg(address.to_string());
                "系统音频发送任务运行中"
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
        ui.strong("系统音频");
        ui.label("在播放端先开启接收，再在另一台电脑发送系统音频。每次会话最长 10 分钟。");
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
        ui.add(
            egui::Slider::new(&mut self.volume, 0..=100)
                .suffix("%")
                .text("播放音量"),
        );
        ui.label("接收使用本机默认输出和 UDP 端口 24874。");
        if ui.button("在本机接收并播放").clicked() {
            let result = self.start(app, Mode::Playback);
            app.feedback(result, "已请求启动音频接收。");
        }
        ui.separator();
        ui.label("接收端 IP 地址和端口");
        ui.add(
            egui::TextEdit::singleline(&mut self.address)
                .char_limit(128)
                .hint_text("留空使用已保存的地址与端口 24874"),
        );
        if ui.button("发送本机系统音频").clicked() {
            let result = self.start(app, Mode::ShareSystem);
            app.feedback(result, "已请求启动系统音频发送。");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unpaired_profile_cannot_start_either_audio_direction() -> Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let app = App::load(fixture.path.clone());
        let mut panel = Panel::default();
        for mode in [Mode::Playback, Mode::ShareSystem] {
            assert!(panel.start(&app, mode).is_err());
            assert!(!panel.is_active());
        }
        panel.stop();
        drop(app);
        fixture.close()
    }
}
