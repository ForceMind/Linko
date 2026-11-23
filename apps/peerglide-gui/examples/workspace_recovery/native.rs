//! Bridges restart workers to a separate owner of a self-created native window.
use std::fmt;
use std::path::Path;
use std::process::Command;
use std::str::FromStr;

use anyhow::{ensure, Context, Result};
use linko_workspace_lab::geometry::Rect;

use super::{check_clear, Fixture, Journal, WindowInstance, Worker, FILENAME};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Observation {
    pub(super) instance: WindowInstance,
    pub(super) bounds: Rect,
}

impl fmt::Display for Observation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.instance.backend {
            write!(formatter, "{byte:02x}")?;
        }
        let Rect {
            x,
            y,
            width,
            height,
        } = self.bounds;
        write!(
            formatter,
            " {} {x} {y} {width} {height}",
            self.instance.object
        )
    }
}

impl FromStr for Observation {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let mut fields = text.split_ascii_whitespace();
        let hex = fields.next().context("missing native owner incarnation")?;
        ensure!(
            hex.len() == 32 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid native owner incarnation"
        );
        let mut backend = [0; 16];
        for (index, byte) in backend.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)?;
        }
        let object = fields
            .next()
            .context("missing native window incarnation")?
            .parse()?;
        ensure!(
            object != 0 && backend != [0; 16],
            "empty native window incarnation"
        );
        let bounds = Rect {
            x: fields.next().context("missing native x")?.parse()?,
            y: fields.next().context("missing native y")?.parse()?,
            width: fields.next().context("missing native width")?.parse()?,
            height: fields.next().context("missing native height")?.parse()?,
        };
        bounds.validate()?;
        ensure!(fields.next().is_none(), "trailing native window fields");
        Ok(Self {
            instance: WindowInstance { backend, object },
            bounds,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Snapshot {
    visible: bool,
    restores: u64,
    window: Observation,
}

fn snapshot(owner: &mut Worker) -> Result<Snapshot> {
    owner.write("inspect")?;
    let text = owner.message()?;
    let mut fields = text.splitn(4, ' ');
    ensure!(fields.next() == Some("snapshot"), "invalid native snapshot");
    let visible = match fields.next() {
        Some("1") => true,
        Some("0") => false,
        _ => anyhow::bail!("invalid native visibility"),
    };
    let restores = fields.next().context("missing restore count")?.parse()?;
    let window = fields.next().context("missing native window")?.parse()?;
    Ok(Snapshot {
        visible,
        restores,
        window,
    })
}

fn begin_hide(
    directory: &Path,
    owner: &mut Worker,
    before: Snapshot,
    transaction: u64,
) -> Result<(Worker, String)> {
    let mut writer = Worker::start_native(directory, "hide", before.window)?;
    writer.expect("prepared")?;
    writer.write("arm")?;
    writer.expect(&format!("armed {transaction}"))?;
    ensure!(snapshot(owner)? == before);
    writer.write("hide")?;
    let request = writer.message()?;
    ensure!(request == format!("hide {transaction} {}", before.window));
    Ok((writer, request))
}

fn park(
    directory: &Path,
    owner: &mut Worker,
    before: Snapshot,
    transaction: u64,
) -> Result<Snapshot> {
    let (mut writer, request) = begin_hide(directory, owner, before, transaction)?;
    owner.write(&request)?;
    let receipt = owner.message()?;
    ensure!(receipt == format!("hidden {transaction}"));
    writer.write(&receipt)?;
    writer.expect("parked")?;
    let hidden = snapshot(owner)?;
    ensure!(!hidden.visible && hidden.window.instance == before.window.instance);
    ensure!(hidden.window.bounds != before.window.bounds);
    writer.crash()?;
    ensure!(snapshot(owner)? == hidden);
    Ok(hidden)
}

fn restore(
    directory: &Path,
    owner: &mut Worker,
    original: Observation,
    observed: Observation,
    transaction: u64,
    completion: &str,
) -> Result<Worker> {
    let mut worker = Worker::start_native(directory, "recover", observed)?;
    worker.expect(&format!("pending {transaction}"))?;
    let request = worker.message()?;
    ensure!(request == format!("restore {transaction} {original}"));
    owner.write(&request)?;
    let receipt = owner.message()?;
    ensure!(receipt == format!("{completion} {transaction}"));
    worker.write(&receipt)?;
    Ok(worker)
}

fn open_owner(directory: &Path, binary: &Path) -> Result<(Worker, Snapshot)> {
    let mut owner = Worker::start_command(Command::new(binary))?;
    owner.expect("ready")?;
    let original = snapshot(&mut owner)?;
    ensure!(original.visible && original.restores == 0);
    let journal = Journal::open(directory)?;
    let base = journal.clean_sequence()?;
    owner.write(&format!("bind {base} {}", original.window))?;
    owner.expect("bound")?;
    Ok((owner, original))
}

fn fence_unapplied(directory: &Path, owner: &mut Worker, original: Snapshot) -> Result<()> {
    let (mut writer, delayed_hide) = begin_hide(directory, owner, original, 1)?;
    writer.crash()?;
    let saved = std::fs::read(directory.join(FILENAME))?;
    owner.write(&format!("restore 2 {}", original.window))?;
    owner.expect("rejected")?;
    let mut worker = restore(
        directory,
        owner,
        original.window,
        original.window,
        1,
        "untouched",
    )?;
    worker.expect("before-clear")?;
    owner.write(&delayed_hide)?;
    owner.expect("rejected")?;
    ensure!(snapshot(owner)? == original);
    worker.crash()?;
    ensure!(std::fs::read(directory.join(FILENAME))? == saved);
    let mut worker = restore(
        directory,
        owner,
        original.window,
        original.window,
        1,
        "untouched",
    )?;
    check_clear(&mut worker)?;
    ensure!(snapshot(owner)? == original);
    ensure!(Journal::open(directory)?.pending()?.is_none());
    println!(
        "Native recovery: unexecuted hide fenced, late command rejected and window unchanged."
    );
    Ok(())
}

pub(super) fn smoke(binary: &Path) -> Result<()> {
    let fixture = Fixture::new()?;
    let directory = &fixture.path;
    let (mut owner, original) = open_owner(directory, binary)?;
    fence_unapplied(directory, &mut owner, original)?;
    let hidden = park(directory, &mut owner, original, 2)?;
    println!("Native recovery: the same owner retained a hidden moved window after worker death.");

    let saved = std::fs::read(directory.join(FILENAME))?;
    let mut wrong = original.window;
    wrong.instance.object += 1;
    owner.write(&format!("restore 2 {wrong}"))?;
    owner.expect("rejected")?;
    owner.write(&format!("restore 1 {}", original.window))?;
    owner.expect("rejected")?;
    ensure!(snapshot(&mut owner)? == hidden);
    ensure!(std::fs::read(directory.join(FILENAME))? == saved);

    let mut worker = restore(
        directory,
        &mut owner,
        original.window,
        hidden.window,
        2,
        "restored",
    )?;
    worker.expect("before-clear")?;
    let recovered = snapshot(&mut owner)?;
    ensure!(recovered.visible && recovered.window == original.window && recovered.restores == 1);
    worker.crash()?;
    ensure!(std::fs::read(directory.join(FILENAME))? == saved);
    let mut worker = restore(
        directory,
        &mut owner,
        original.window,
        recovered.window,
        2,
        "restored",
    )?;
    check_clear(&mut worker)?;
    ensure!(snapshot(&mut owner)? == recovered);
    ensure!(Journal::open(directory)?.pending()?.is_none());
    println!(
        "Native recovery: original visibility and bounds restored once across two worker crashes."
    );

    owner.write("exit")?;
    owner.wait(true)?;
    let (mut owner, next) = open_owner(directory, binary)?;
    ensure!(next.window.instance != original.window.instance);
    let _ = park(directory, &mut owner, next, 3)?;
    let lost = std::fs::read(directory.join(FILENAME))?;
    owner.crash()?;
    let mut replacement = Worker::start_command(Command::new(binary))?;
    replacement.expect("ready")?;
    let new_window = snapshot(&mut replacement)?;
    ensure!(new_window.visible && new_window.window.instance != next.window.instance);
    ensure!(Journal::open(directory)?.clean_sequence().is_err());
    let mut worker = Worker::start_native(directory, "recover", new_window.window)?;
    worker.expect("pending 3")?;
    worker.wait(false)?;
    ensure!(snapshot(&mut replacement)? == new_window);
    ensure!(std::fs::read(directory.join(FILENAME))? == lost);
    replacement.write("exit")?;
    replacement.wait(true)?;
    println!(
        "Native recovery: a new owner was left untouched and the lost window record retained."
    );
    fixture.close()
}
