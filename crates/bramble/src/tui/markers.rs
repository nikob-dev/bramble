//! Finds Bramble's private OSC markers in the lab shell's output stream.
//!
//! The lab's bash prints `ESC ] 7771 BEL` (via PS0) just before a command runs and
//! `ESC ] 7770 ; <base64 exit\tcwd\tcmd> BEL` (via PROMPT_COMMAND) when it finishes.
//! Everything between the two is the command's output. Terminal emulators ignore
//! unknown OSC sequences, so the same bytes are also fed to the screen untouched.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bramble_core::session::Observation;

const PREFIX: &[u8] = b"\x1b]777";
const BEL: u8 = 0x07;
const MAX_SEQUENCE: usize = 16 * 1024;
const MAX_OUTPUT: usize = 64 * 1024;

#[derive(Default)]
pub struct MarkerScanner {
    pending: Vec<u8>,
    capturing: bool,
    output: Vec<u8>,
}

impl MarkerScanner {
    /// Feeds a chunk of PTY output; returns any commands that finished in it.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Observation> {
        self.pending.extend_from_slice(chunk);
        let mut finished = Vec::new();
        loop {
            match find(&self.pending, PREFIX) {
                Some(start) => {
                    let Some(len) = self.pending[start..].iter().position(|&b| b == BEL) else {
                        if self.pending.len() - start > MAX_SEQUENCE {
                            // Not one of ours after all; treat it as plain output.
                            let bytes = std::mem::take(&mut self.pending);
                            self.capture(&bytes);
                        } else {
                            let before: Vec<u8> = self.pending.drain(..start).collect();
                            self.capture(&before);
                        }
                        break;
                    };
                    let before: Vec<u8> = self.pending.drain(..start).collect();
                    self.capture(&before);
                    let sequence: Vec<u8> = self.pending.drain(..=len).collect();
                    // Strip "ESC ]" and the trailing BEL.
                    let body = &sequence[2..sequence.len() - 1];
                    if let Some(observation) = self.handle(body) {
                        finished.push(observation);
                    }
                }
                None => {
                    // Keep a possible partial prefix at the end for the next chunk.
                    let keep = partial_prefix_len(&self.pending);
                    let split = self.pending.len() - keep;
                    let bytes: Vec<u8> = self.pending.drain(..split).collect();
                    self.capture(&bytes);
                    break;
                }
            }
        }
        finished
    }

    fn capture(&mut self, bytes: &[u8]) {
        if self.capturing {
            let room = MAX_OUTPUT.saturating_sub(self.output.len());
            self.output
                .extend_from_slice(&bytes[..bytes.len().min(room)]);
        }
    }

    fn handle(&mut self, body: &[u8]) -> Option<Observation> {
        if body == b"7771" {
            self.capturing = true;
            self.output.clear();
            return None;
        }
        let payload = body.strip_prefix(b"7770;")?;
        let output = if self.capturing {
            String::from_utf8_lossy(&strip_ansi_escapes::strip(&self.output)).into_owned()
        } else {
            String::new()
        };
        self.capturing = false;
        self.output.clear();
        let decoded = STANDARD.decode(payload).ok()?;
        let text = String::from_utf8_lossy(&decoded);
        let mut fields = text.splitn(3, '\t');
        let exit_code = fields.next()?.trim().parse().ok()?;
        let cwd = fields.next()?.to_owned();
        let cmd = fields.next()?.trim().to_owned();
        Some(Observation {
            cmd,
            exit_code,
            cwd,
            output,
        })
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn partial_prefix_len(bytes: &[u8]) -> usize {
    (1..PREFIX.len())
        .rev()
        .find(|&n| bytes.len() >= n && bytes[bytes.len() - n..] == PREFIX[..n])
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn end_marker(exit: i32, cwd: &str, cmd: &str) -> Vec<u8> {
        let payload = STANDARD.encode(format!("{exit}\t{cwd}\t{cmd}"));
        format!("\x1b]7770;{payload}\x07").into_bytes()
    }

    fn session_bytes() -> Vec<u8> {
        let mut bytes = b"$ sqlite3 x\r\n\x1b]7771\x07".to_vec();
        bytes.extend_from_slice(b"\x1b[1m-7150\x1b[0m\r\n");
        bytes.extend(end_marker(0, "/home/learner/lab", "sqlite3 x"));
        bytes.extend_from_slice(b"$ ");
        bytes
    }

    #[test]
    fn markers_feed_keeps_multi_line_commands_whole() {
        let cmd = "for amount in 500 700; do\n  curl -d \"{\\\"amount_minor\\\": $amount}\" x/api/ach/payments\ndone";
        let obs = MarkerScanner::default().feed(&end_marker(0, "/home/learner/lab", cmd));
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].cmd, cmd, "every line survives");
    }

    #[test]
    fn markers_feed_extracts_command_and_clean_output() {
        let obs = MarkerScanner::default().feed(&session_bytes());
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].cmd, "sqlite3 x");
        assert_eq!(obs[0].exit_code, 0);
        assert_eq!(obs[0].cwd, "/home/learner/lab");
        assert_eq!(obs[0].output.trim(), "-7150");
    }

    #[test]
    fn markers_feed_handles_every_possible_chunk_split() {
        let bytes = session_bytes();
        for split in 0..bytes.len() {
            let mut scanner = MarkerScanner::default();
            let mut obs = scanner.feed(&bytes[..split]);
            obs.extend(scanner.feed(&bytes[split..]));
            assert_eq!(obs.len(), 1, "split at {split}");
            assert_eq!(obs[0].output.trim(), "-7150", "split at {split}");
        }
    }

    #[test]
    fn markers_feed_end_without_start_has_empty_output() {
        let obs = MarkerScanner::default().feed(&end_marker(1, "/", "false"));
        assert_eq!(obs[0].exit_code, 1);
        assert_eq!(obs[0].output, "");
    }

    #[test]
    fn markers_feed_ignores_garbage_payloads() {
        let obs = MarkerScanner::default().feed(b"\x1b]7770;not-base64!!\x07");
        assert!(obs.is_empty());
    }
}
