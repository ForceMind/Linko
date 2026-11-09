use peerglide_platform::now_ms;
use peerglide_protocol::{layout_ipc::LayoutUpdate, Edge};
use std::sync::Mutex;
use tokio::sync::mpsc;

pub struct Request {
    pub update: LayoutUpdate,
    pub deadline: u64,
}

#[derive(Default)]
struct State {
    sender: Option<mpsc::Sender<Request>>,
    edge: Option<Edge>,
}

#[derive(Default)]
pub struct Controller(Mutex<State>);

pub struct Session<'a>(&'a Controller);
impl Drop for Session<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.0.lock() {
            state.sender = None;
        }
    }
}

impl Controller {
    pub fn open(&self, fallback: Edge) -> anyhow::Result<(Session<'_>, mpsc::Receiver<Request>, Edge)> {
        let mut state = self.0.lock().map_err(|_| anyhow::anyhow!("layout controller unavailable"))?;
        anyhow::ensure!(state.sender.is_none(), "layout session already active");
        let (sender, receiver) = mpsc::channel(1);
        state.sender = Some(sender);
        Ok((Session(self), receiver, state.edge.unwrap_or(fallback)))
    }

    pub fn submit(&self, update: LayoutUpdate) {
        if update.applied.is_some() {
            return;
        }
        if let Ok(state) = self.0.lock() {
            if let Some(sender) = &state.sender {
                let _ = sender.try_send(Request {
                    update,
                    deadline: now_ms().saturating_add(2000),
                });
            }
        }
    }

    pub fn applied(&self, edge: Edge) {
        if let Ok(mut state) = self.0.lock() {
            state.edge = Some(edge);
        }
    }
}

/// Drain oversized commands without retaining their prefix as a valid command.
pub fn read_command(reader: &mut impl std::io::BufRead) -> std::io::Result<Option<String>> {
    let mut line = Vec::with_capacity(256);
    let mut oversized = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(None);
        }
        let end = buffer.iter().position(|b| *b == b'\n');
        let consumed = end.map_or(buffer.len(), |index| index + 1);
        if !oversized && line.len() + consumed <= 256 {
            line.extend_from_slice(&buffer[..consumed]);
        } else {
            oversized = true;
            line.clear();
        }
        reader.consume(consumed);
        if end.is_some() {
            return Ok(Some(if oversized {
                String::new()
            } else {
                String::from_utf8(line).unwrap_or_default()
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_bounded_and_cannot_leak_into_a_successor_session() {
        let controller = Controller::default();
        controller.submit(LayoutUpdate::request(1, Edge::Left));
        let (scope, mut old, edge) = controller.open(Edge::Right).unwrap();
        assert_eq!(edge, Edge::Right);
        controller.submit(LayoutUpdate::request(2, Edge::Left));
        controller.submit(LayoutUpdate::request(3, Edge::Bottom));
        assert_eq!(old.try_recv().unwrap().update.request, 2);
        assert!(old.try_recv().is_err());
        controller.applied(Edge::Left);
        drop(scope);
        controller.submit(LayoutUpdate::request(4, Edge::Bottom));
        let (_scope, mut next, edge) = controller.open(Edge::Right).unwrap();
        assert_eq!(edge, Edge::Left);
        assert!(next.try_recv().is_err());
        controller.submit(LayoutUpdate::request(5, Edge::Top).reply(true));
        assert!(next.try_recv().is_err());
    }

    #[test]
    fn oversized_or_invalid_commands_are_drained_without_hiding_stop() {
        let mut bytes = vec![b'x'; 4096];
        bytes.extend_from_slice(b"\n\xff\nstop\n");
        let mut reader = std::io::Cursor::new(bytes);
        assert_eq!(read_command(&mut reader).unwrap(), Some(String::new()));
        assert_eq!(read_command(&mut reader).unwrap(), Some(String::new()));
        assert_eq!(read_command(&mut reader).unwrap().as_deref(), Some("stop\n"));
        assert_eq!(read_command(&mut reader).unwrap(), None);
    }
}
