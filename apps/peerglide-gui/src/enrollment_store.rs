//! Recoverable local enrollment, not network authentication. Only local settings
//! and public certificates enter the journal; private keys are never copied.
use super::{config_store, transport, Config};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_JOURNAL: usize = 1024 * 1024;
const NAME: &str = "enrollment-pending.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    previous_settings: Option<Vec<u8>>,
    next_settings: Vec<u8>,
    previous_peer: Option<Vec<u8>>,
    next_peer: Option<Vec<u8>>,
    local_certificate: Option<Vec<u8>>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    journal_version: u16,
    proposal: Proposal,
    // Accidental-corruption check only. A local digest is not authentication.
    checksum: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    JournalDurable,
    PeerReplaced,
    SettingsReplaced,
}

fn path(settings: &Path) -> Result<PathBuf> {
    anyhow::ensure!(
        settings.file_name().and_then(|s| s.to_str()) == Some("settings.json"),
        "设备登记只能提交本资料的设置"
    );
    Ok(settings.parent().context("资料目录不可用")?.join(NAME))
}
pub(super) fn ensure_idle(settings: &Path) -> Result<()> {
    if settings.file_name().and_then(|s| s.to_str()) == Some("settings.json") {
        match fs::symlink_metadata(path(settings)?) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error).context("无法检查上次设备登记"),
            Ok(_) => anyhow::bail!("上次设备登记尚未完成；请关闭后重新打开屏跃恢复，原文件已保留"),
        }
    }
    Ok(())
}
fn read_bounded(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("无法读取登记文件"),
    };
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= limit as u64,
        "登记文件不是受限普通文件，原件已保留"
    );
    let file = fs::File::open(path)?;
    anyhow::ensure!(file.metadata()?.is_file(), "登记文件不是普通文件");
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= limit, "登记文件过大，原件已保留");
    Ok(Some(bytes))
}
fn public_certificate(path: &Path) -> Result<Option<Vec<u8>>> {
    let bytes = read_bounded(path, transport::MAX_CERTIFICATE_BYTES)?;
    if let Some(bytes) = &bytes {
        transport::validate_certificate(bytes)?;
    }
    Ok(bytes)
}
fn validate(proposal: &Proposal) -> Result<()> {
    let next: Config = config_store::decode(&proposal.next_settings)?;
    if let Some(previous) = &proposal.previous_settings {
        let _: Config = config_store::decode(previous)?;
    }
    for cert in [
        &proposal.previous_peer,
        &proposal.next_peer,
        &proposal.local_certificate,
    ]
    .into_iter()
    .flatten()
    {
        transport::validate_certificate(cert)?;
    }
    let fingerprint = proposal.next_peer.as_deref().map(transport::fingerprint);
    anyhow::ensure!(
        next.fingerprint == fingerprint && !next.allow && !next.native,
        "登记收尾记录与证书或关闭授权状态不一致，原件已保留"
    );
    if let Some(peer) = &proposal.next_peer {
        let local = proposal
            .local_certificate
            .as_ref()
            .context("登记记录缺少本机身份")?;
        anyhow::ensure!(peer != local, "登记记录不能信任本机身份");
    }
    Ok(())
}
fn journal_bytes(proposal: Proposal) -> Result<Vec<u8>> {
    validate(&proposal)?;
    let checksum = blake3::hash(&serde_json::to_vec(&proposal)?)
        .to_hex()
        .to_string();
    let bytes = serde_json::to_vec(&Journal {
        journal_version: 1,
        proposal,
        checksum,
    })?;
    anyhow::ensure!(bytes.len() <= MAX_JOURNAL, "登记收尾记录过大");
    Ok(bytes)
}
fn parse(bytes: &[u8]) -> Result<Journal> {
    let journal: Journal = serde_json::from_slice(bytes).context("登记收尾记录无效，原件已保留")?;
    anyhow::ensure!(
        journal.journal_version == 1,
        "未来版本登记记录已保留，不会写入"
    );
    anyhow::ensure!(
        journal.checksum
            == blake3::hash(&serde_json::to_vec(&journal.proposal)?)
                .to_hex()
                .as_str(),
        "登记收尾记录校验失败，原件已保留"
    );
    validate(&journal.proposal)?;
    Ok(journal)
}
fn apply(
    settings: &Path,
    bytes: &[u8],
    mut checkpoint: impl FnMut(Step) -> Result<()>,
) -> Result<()> {
    let journal = parse(bytes)?;
    let proposal = &journal.proposal;
    let dir = settings.parent().context("资料目录不可用")?;
    let peer_path = dir.join("trusted-peer.crt.der");
    let current_settings = config_store::read(settings)?;
    let current_peer = public_certificate(&peer_path)?;
    anyhow::ensure!(
        current_settings == proposal.previous_settings
            || current_settings.as_deref() == Some(&proposal.next_settings),
        "登记期间设置已变化，停止恢复并保留原件"
    );
    anyhow::ensure!(
        current_peer == proposal.previous_peer || current_peer == proposal.next_peer,
        "登记期间证书已变化，停止恢复并保留原件"
    );
    anyhow::ensure!(
        public_certificate(&dir.join("identity/node.crt.der"))? == proposal.local_certificate,
        "登记期间本机身份已变化，停止恢复并保留原件"
    );
    if current_peer != proposal.next_peer {
        if let Some(next) = &proposal.next_peer {
            config_store::atomic_write(&peer_path, next)?;
        } else {
            // Only the exact prepared old public certificate can reach removal.
            fs::remove_file(&peer_path)?;
            config_store::sync_parent(&peer_path)?;
        }
    }
    checkpoint(Step::PeerReplaced)?;
    if current_settings.as_deref() != Some(&proposal.next_settings) {
        config_store::atomic_write(settings, &proposal.next_settings)?;
    }
    checkpoint(Step::SettingsReplaced)?;
    anyhow::ensure!(
        config_store::read(settings)?.as_deref() == Some(&proposal.next_settings)
            && public_certificate(&peer_path)? == proposal.next_peer
            && public_certificate(&dir.join("identity/node.crt.der"))?
                == proposal.local_certificate,
        "登记提交后文件再次变化，已保留收尾记录"
    );
    anyhow::ensure!(
        read_bounded(&path(settings)?, MAX_JOURNAL)?.as_deref() == Some(bytes),
        "登记收尾记录已变化，原件已保留"
    );
    fs::remove_file(path(settings)?)?;
    config_store::sync_parent(settings)
}
pub(super) fn commit(
    store: &mut config_store::Store,
    next: &Config,
    peer: Option<&[u8]>,
) -> Result<()> {
    commit_with(store, next, peer, |_| Ok(()))
}
fn commit_with(
    store: &mut config_store::Store,
    next: &Config,
    peer: Option<&[u8]>,
    mut checkpoint: impl FnMut(Step) -> Result<()>,
) -> Result<()> {
    store.commit_enrollment(next, |settings, previous_settings, next_settings| {
        let dir = settings.parent().context("资料目录不可用")?;
        let peer_path = dir.join("trusted-peer.crt.der");
        let previous_peer = public_certificate(&peer_path)?;
        let previous: Config = previous_settings
            .as_deref()
            .map(config_store::decode)
            .transpose()?
            .unwrap_or_default();
        if let Some(previous_peer) = &previous_peer {
            anyhow::ensure!(
                previous.fingerprint.as_deref()
                    == Some(transport::fingerprint(previous_peer).as_str()),
                "已保存的设备证书与设置不一致，请先检查原文件"
            );
        } else if peer.is_some() {
            anyhow::ensure!(
                previous.fingerprint.is_none(),
                "已登记证书缺失，请先撤销旧登记"
            );
        }
        let proposal = Proposal {
            previous_settings,
            next_settings: next_settings.to_vec(),
            previous_peer,
            next_peer: peer.map(<[u8]>::to_vec),
            local_certificate: public_certificate(&dir.join("identity/node.crt.der"))?,
        };
        let bytes = journal_bytes(proposal)?;
        let proposal = parse(&bytes)?.proposal;
        // Prepare backups before the durable intent; collisions never overwrite.
        config_store::backup(settings, proposal.previous_settings.as_deref())?;
        config_store::backup(&peer_path, proposal.previous_peer.as_deref())?;
        let journal_path = path(settings)?;
        let mut file = config_store::options()
            .create_new(true)
            .open(&journal_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        config_store::sync_parent(&journal_path)?;
        checkpoint(Step::JournalDurable)?;
        apply(settings, &bytes, checkpoint)
    })
}
/// Call only while owning this profile's GUI instance lock, before loading its
/// settings. No network, private key creation, input or OS permission request.
pub(super) fn recover(dir: &Path) -> Result<bool> {
    let settings = dir.join("settings.json");
    let journal_path = path(&settings)?;
    let Some(bytes) = read_bounded(&journal_path, MAX_JOURNAL)? else {
        return Ok(false);
    };
    let _lock = config_store::lock(&settings.with_extension("lock"))?;
    apply(&settings, &bytes, |_| Ok(()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{tests::Fixture, App, Identity};
    use std::process::Command;

    fn paired(dir: &Path) -> Result<()> {
        Identity::create(&dir.join("identity"))?;
        let peer = Identity::generate()?;
        fs::write(dir.join("trusted-peer.crt.der"), &peer.certificate)?;
        let config = Config {
            fingerprint: Some(peer.fingerprint()),
            allow: true,
            native: true,
            ..Config::default()
        };
        let (mut store, _): (_, Config) = config_store::Store::open(dir.join("settings.json"))?;
        store.save(&config)
    }
    fn next(dir: &Path, revoke: bool) -> Result<(config_store::Store, Config, Option<Vec<u8>>)> {
        let (store, mut config): (_, Config) =
            config_store::Store::open(dir.join("settings.json"))?;
        let peer = if revoke {
            None
        } else {
            Some(Identity::generate()?.certificate)
        };
        config.fingerprint = peer.as_deref().map(transport::fingerprint);
        config.allow = false;
        config.native = false;
        Ok((store, config, peer))
    }
    #[test]
    fn crash_helper() -> Result<()> {
        let Some(dir) = std::env::var_os("PEERGLIDE_ENROLLMENT_CRASH_FIXTURE") else {
            return Ok(());
        };
        let dir = PathBuf::from(dir);
        // Test-only helper, never a runtime command/authentication shortcut.
        anyhow::ensure!(dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("peerglide-ui-"));
        let _owner = config_store::lock(&dir.join("gui-instance.lock"))?;
        let revoke = std::env::var("PEERGLIDE_ENROLLMENT_REVOKE")? == "1";
        let wanted = std::env::var("PEERGLIDE_ENROLLMENT_CRASH_STEP")?;
        let (mut store, config, peer) = next(&dir, revoke)?;
        commit_with(&mut store, &config, peer.as_deref(), |step| {
            if format!("{step:?}") == wanted {
                std::process::exit(86);
            }
            Ok(())
        })?;
        anyhow::bail!("crash checkpoint was not reached")
    }
    #[test]
    fn process_death_at_each_commit_boundary_recovers_enrollment_and_revocation() -> Result<()> {
        for fresh in [false, true] {
            for revoke in [false, true] {
                if fresh && revoke {
                    continue;
                }
                for step in [
                    Step::JournalDurable,
                    Step::PeerReplaced,
                    Step::SettingsReplaced,
                ] {
                    let f = Fixture::new()?;
                    if fresh {
                        Identity::create(&f.path.join("identity"))?;
                    } else {
                        paired(&f.path)?;
                    }
                    let identity = fs::read(f.path.join("identity/node.crt.der"))?;
                    let key = fs::read(f.path.join("identity/node.key.der"))?;
                    let exit = Command::new(std::env::current_exe()?)
                        .args([
                            "--exact",
                            "enrollment_store::tests::crash_helper",
                            "--nocapture",
                        ])
                        .env("PEERGLIDE_ENROLLMENT_CRASH_FIXTURE", &f.path)
                        .env(
                            "PEERGLIDE_ENROLLMENT_REVOKE",
                            if revoke { "1" } else { "0" },
                        )
                        .env("PEERGLIDE_ENROLLMENT_CRASH_STEP", format!("{step:?}"))
                        .output()?;
                    assert_eq!(
                        exit.status.code(),
                        Some(86),
                        "child did not reach {step:?}: {}",
                        String::from_utf8_lossy(&exit.stderr)
                    );
                    let journal = parse(&fs::read(f.path.join(NAME))?)?;
                    let expected = journal.proposal.next_peer;
                    let app = App::load(f.path.clone());
                    assert!(app.config_store.is_some(), "{}", app.notice);
                    assert_eq!(
                        app.config.fingerprint,
                        expected.as_deref().map(transport::fingerprint)
                    );
                    assert!(!app.config.allow && !app.config.native && app.child.is_none());
                    assert_eq!(
                        public_certificate(&f.path.join("trusted-peer.crt.der"))?,
                        expected
                    );
                    assert!(!f.path.join(NAME).exists());
                    assert_eq!(fs::read(f.path.join("identity/node.crt.der"))?, identity);
                    assert!(
                        fs::read(f.path.join("identity/node.key.der"))? == key,
                        "fixture identity key changed; its contents must never be printed"
                    );
                    drop(app);
                    assert!(!recover(&f.path)?, "recovery is idempotent");
                }
            }
        }
        Ok(())
    }
    #[test]
    fn changed_config_peer_or_identity_stops_recovery_without_overwriting() -> Result<()> {
        for changed in [
            "settings.json",
            "trusted-peer.crt.der",
            "identity/node.crt.der",
        ] {
            let f = Fixture::new()?;
            paired(&f.path)?;
            let (mut store, config, peer) = next(&f.path, false)?;
            assert!(commit_with(&mut store, &config, peer.as_deref(), |step| {
                anyhow::ensure!(step != Step::JournalDurable, "injected interruption");
                Ok(())
            })
            .is_err());
            assert!(
                store.save(&config).is_err(),
                "pending enrollment cannot be overwritten by a later save"
            );
            let replacement = if changed == "settings.json" {
                br#"{"store_version":999,"data":{}}"#.to_vec()
            } else {
                Identity::generate()?.certificate
            };
            fs::write(f.path.join(changed), &replacement)?;
            let settings = fs::read(f.path.join("settings.json"))?;
            let peer = fs::read(f.path.join("trusted-peer.crt.der"))?;
            let journal = fs::read(f.path.join(NAME))?;
            let app = App::load(f.path.clone());
            assert!(app.config_store.is_none() && app.enrollment_blocked);
            assert!(app.config.fingerprint.is_none() && !app.config.allow && !app.config.native);
            assert!(config_store::Store::reset::<Config>(f.path.join("settings.json")).is_err());
            assert_eq!(fs::read(f.path.join(changed))?, replacement);
            assert_eq!(fs::read(f.path.join("settings.json"))?, settings);
            assert_eq!(fs::read(f.path.join("trusted-peer.crt.der"))?, peer);
            assert_eq!(fs::read(f.path.join(NAME))?, journal);
        }
        Ok(())
    }
    #[test]
    fn pending_record_blocks_other_gui_writes_reset_and_future_or_bad_records_stay_intact(
    ) -> Result<()> {
        for bytes in [
            b"broken".to_vec(),
            br#"{"journal_version":999}"#.to_vec(),
            vec![0; MAX_JOURNAL + 1],
        ] {
            let f = Fixture::new()?;
            paired(&f.path)?;
            let owner = config_store::lock(&f.path.join("gui-instance.lock"))?;
            let settings = fs::read(f.path.join("settings.json"))?;
            fs::write(f.path.join(NAME), &bytes)?;
            let second = App::load(f.path.clone());
            assert!(
                second.instance_lock.is_none()
                    && second.config_store.is_none()
                    && second.enrollment_blocked
            );
            assert_eq!(fs::read(f.path.join(NAME))?, bytes);
            drop(second);
            drop(owner);
            let app = App::load(f.path.clone());
            assert!(app.config_store.is_none());
            assert_eq!(fs::read(f.path.join(NAME))?, bytes);
            assert_eq!(fs::read(f.path.join("settings.json"))?, settings);
        }
        Ok(())
    }
    #[test]
    fn preparation_backup_failure_leaves_primary_pair_and_settings_untouched() -> Result<()> {
        let f = Fixture::new()?;
        paired(&f.path)?;
        let settings = f.path.join("settings.json");
        let original = fs::read(&settings)?;
        let peer = fs::read(f.path.join("trusted-peer.crt.der"))?;
        let backup = settings.with_extension(format!("bak-{}", blake3::hash(&original).to_hex()));
        fs::write(&backup, b"unrelated existing file")?;
        let (mut store, config, next_peer) = next(&f.path, false)?;
        assert!(commit(&mut store, &config, next_peer.as_deref()).is_err());
        assert_eq!(fs::read(settings)?, original);
        assert_eq!(fs::read(f.path.join("trusted-peer.crt.der"))?, peer);
        assert_eq!(fs::read(backup)?, b"unrelated existing file");
        assert!(!f.path.join(NAME).exists());
        Ok(())
    }
    #[test]
    fn changed_journal_checksum_and_grants_are_rejected_before_file_writes() -> Result<()> {
        for invalid in [0, 1, 2] {
            let f = Fixture::new()?;
            paired(&f.path)?;
            let (mut store, config, peer) = next(&f.path, false)?;
            assert!(
                commit_with(&mut store, &config, peer.as_deref(), |_| anyhow::bail!(
                    "stop after prepare"
                ))
                .is_err()
            );
            let mut journal = parse(&fs::read(f.path.join(NAME))?)?;
            if invalid == 1 {
                let mut settings: Config = config_store::decode(&journal.proposal.next_settings)?;
                settings.allow = true;
                journal.proposal.next_settings = config_store::encode(&settings)?;
                journal.checksum = blake3::hash(&serde_json::to_vec(&journal.proposal)?)
                    .to_hex()
                    .to_string();
            } else if invalid == 2 {
                journal.journal_version = 999;
            } else {
                journal.checksum = "0".repeat(64);
            }
            let bytes = serde_json::to_vec(&journal)?;
            fs::write(f.path.join(NAME), &bytes)?;
            let original_settings = fs::read(f.path.join("settings.json"))?;
            let original_peer = fs::read(f.path.join("trusted-peer.crt.der"))?;
            assert!(recover(&f.path).is_err());
            assert_eq!(fs::read(f.path.join(NAME))?, bytes);
            assert_eq!(fs::read(f.path.join("settings.json"))?, original_settings);
            assert_eq!(
                fs::read(f.path.join("trusted-peer.crt.der"))?,
                original_peer
            );
        }
        Ok(())
    }
    #[test]
    fn settings_lock_contention_preserves_pending_commit_until_owner_releases() -> Result<()> {
        let f = Fixture::new()?;
        paired(&f.path)?;
        let (mut store, config, peer) = next(&f.path, false)?;
        assert!(commit_with(&mut store, &config, peer.as_deref(), |step| {
            anyhow::ensure!(
                step != Step::PeerReplaced,
                "write failed after peer replacement"
            );
            Ok(())
        })
        .is_err());
        let settings = fs::read(f.path.join("settings.json"))?;
        let certificate = fs::read(f.path.join("trusted-peer.crt.der"))?;
        let journal = fs::read(f.path.join(NAME))?;
        assert!(store.save(&config).is_err());
        let writer = config_store::lock(&f.path.join("settings.lock"))?;
        assert!(recover(&f.path).is_err());
        assert_eq!(fs::read(f.path.join(NAME))?, journal);
        assert_eq!(fs::read(f.path.join("settings.json"))?, settings);
        assert_eq!(fs::read(f.path.join("trusted-peer.crt.der"))?, certificate);
        drop(writer);
        assert!(recover(&f.path)?);
        let app = App::load(f.path.clone());
        assert_eq!(app.config.fingerprint, config.fingerprint);
        assert!(!app.config.allow && !app.config.native && app.config_store.is_some());
        Ok(())
    }
    #[test]
    fn late_file_change_cannot_report_commit_complete_or_remove_the_journal() -> Result<()> {
        let f = Fixture::new()?;
        paired(&f.path)?;
        let (mut store, config, peer) = next(&f.path, false)?;
        let replacement = Identity::generate()?.certificate;
        assert!(commit_with(&mut store, &config, peer.as_deref(), |step| {
            if step == Step::SettingsReplaced {
                fs::write(f.path.join("trusted-peer.crt.der"), &replacement)?;
            }
            Ok(())
        })
        .is_err());
        assert!(f.path.join(NAME).is_file());
        assert_eq!(fs::read(f.path.join("trusted-peer.crt.der"))?, replacement);
        let app = App::load(f.path.clone());
        assert!(app.config_store.is_none());
        assert_eq!(fs::read(f.path.join("trusted-peer.crt.der"))?, replacement);
        Ok(())
    }
}
