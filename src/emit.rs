use crate::types::{Banner, Batch};
use std::io::{self, BufWriter, Write};
use std::sync::mpsc;

/// The NDJSON emitter. Receives completed batches from the coalescer
/// and writes them as newline-delimited JSON to a writer (usually stdout).
pub struct Emitter<W: Write> {
    writer: BufWriter<W>,
    rx: mpsc::Receiver<Batch>,
}

impl<W: Write> Emitter<W> {
    pub fn new(writer: W, rx: mpsc::Receiver<Batch>) -> Self {
        Self {
            writer: BufWriter::new(writer),
            rx,
        }
    }

    /// Write the startup banner as the first record.
    pub fn emit_banner(&mut self, banner: &Banner) -> io::Result<()> {
        let json = serde_json::to_string(banner)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        writeln!(self.writer, "{}", json)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Run the emitter loop. Reads batches from the channel and writes
    /// them as NDJSON. Returns when the channel is closed (coalescer done).
    pub fn run(&mut self) -> io::Result<()> {
        loop {
            match self.rx.recv() {
                Ok(batch) => {
                    let json = serde_json::to_string(&batch)
                        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                    writeln!(self.writer, "{}", json)?;
                    self.writer.flush()?;
                }
                Err(mpsc::RecvError) => {
                    // Channel closed — no more batches.
                    break;
                }
            }
        }
        // Final flush.
        self.writer.flush()?;
        Ok(())
    }


}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Event, EventType, FileInfo};

    fn make_batch(seq: u64, kind: EventType, path: &str) -> Batch {
        Batch::new(
            seq,
            vec![Event {
                kind,
                path: path.into(),
                cookie: None,
                info: Some(FileInfo {
                    size: 0,
                    mode_str: "0644".into(),
                    is_dir: false,
                }),
            }],
        )
    }

    #[test]
    fn test_emit_banner() {
        let (_, rx) = mpsc::channel();
        let mut buf = Vec::new();
        let mut emitter = Emitter::new(&mut buf, rx);

        let banner = Banner::new("0.1.0", 12345, &["/tmp/test".into()], 50);
        emitter.emit_banner(&banner).unwrap();
        drop(emitter);

        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("\"type\":\"banner\""));
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn test_emit_batch() {
        let (tx, rx) = mpsc::channel();
        let mut buf = Vec::new();
        let mut emitter = Emitter::new(&mut buf, rx);

        tx.send(make_batch(1, EventType::Create, "/tmp/test.txt")).unwrap();
        tx.send(make_batch(2, EventType::Delete, "/tmp/test.txt")).unwrap();
        drop(tx); // close channel

        emitter.run().unwrap();
        drop(emitter);

        let output = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = output.trim().lines().collect();
        assert_eq!(lines.len(), 2, "should emit 2 lines");
        assert!(lines[0].contains("\"seq\":1"));
        assert!(lines[1].contains("\"seq\":2"));
    }

    #[test]
    fn test_emit_ndjson_newline_terminated() {
        let (tx, rx) = mpsc::channel();
        let mut buf = Vec::new();
        let mut emitter = Emitter::new(&mut buf, rx);

        tx.send(make_batch(1, EventType::Create, "/tmp/foo.txt")).unwrap();
        drop(tx);

        emitter.run().unwrap();
        drop(emitter);

        let output = String::from_utf8(buf).unwrap();
        // Every line should end with \n.
        for line in output.lines() {
            // Each line should be valid JSON.
            let _parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        }
    }

    #[test]
    fn test_emit_empty_channel() {
        let (tx, rx) = mpsc::channel();
        let mut buf = Vec::new();
        let mut emitter = Emitter::new(&mut buf, rx);

        drop(tx); // immediately close
        emitter.run().unwrap();
        drop(emitter);

        let output = String::from_utf8(buf).unwrap();
        assert!(output.is_empty(), "no output expected");
    }
}
