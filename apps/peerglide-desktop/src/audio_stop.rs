//! Cooperative audio shutdown on the local supervisor pipe, its EOF, or Ctrl+C.
use std::io::{self, BufRead};

#[cfg(any(windows, target_os = "macos"))]
pub(super) async fn requested(managed: bool) -> io::Result<()> {
    if !managed {
        return tokio::signal::ctrl_c().await;
    }
    let (send, closed) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("linko-audio-stop".into())
        .spawn(move || {
            let result = read_stop(&mut io::stdin().lock());
            let _ = send.send(result);
        })?;
    tokio::select! {
        biased;
        result = closed => result.map_err(|_| io::Error::other("audio stop listener ended"))?,
        result = tokio::signal::ctrl_c() => result,
    }
}

fn read_stop(reader: &mut impl BufRead) -> io::Result<()> {
    while let Some(command) = super::layout_control::read_command(reader)? {
        if command.trim() == "stop" {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn stop_consumes_only_the_complete_local_command() -> io::Result<()> {
        let oversized = format!("stop{}\n", " ".repeat(256));
        let input = format!("{oversized}start\nstop\r\nremaining\n");
        let mut reader = BufReader::new(Cursor::new(input));
        read_stop(&mut reader)?;
        assert_eq!(reader.fill_buf()?, b"remaining\n");
        Ok(())
    }

    #[test]
    fn loss_of_the_supervisor_pipe_requests_stop() -> io::Result<()> {
        let mut reader = Cursor::new(b"unknown\nunterminated");
        read_stop(&mut reader)?;
        assert!(reader.fill_buf()?.is_empty());
        Ok(())
    }
}
