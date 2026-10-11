//! The lab: a real bash shell in a pseudo-terminal, instrumented with Bramble's markers.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::Context;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc::UnboundedSender;

const LAB_RC: &str = r#"
[ -f /etc/bash.bashrc ] && . /etc/bash.bashrc
HISTCONTROL=
HISTFILE="$HOME/.bramble_history"
PS1='\[\e[1;32m\]learner\[\e[0m\]@\[\e[1;35m\]${BRAMBLE_PET_NAME:-pip}\[\e[0m\]:\[\e[1;34m\]\w\[\e[0m\]\$ '
PS0=$'\e]7771\a'
PS2='\[\e[33m\]… unfinished quote or line: finish it, or Ctrl-C to cancel > \[\e[0m\]'
command_not_found_handle() {
    if (( $# >= 3 )); then
        printf '%s: that looks like a sentence. This pane is a bash shell, so it only runs commands.\n' "${BRAMBLE_PET_NAME:-pip}" >&2
        printf 'Chatting with me comes later; for now, type the command shown at the bottom of my lesson panel.\n' >&2
    else
        printf 'bash: %s: command not found\n' "$1" >&2
    fi
    return 127
}
__bramble_hook() {
    local ec=$? num cmd
    read -r num cmd <<< "$(HISTTIMEFORMAT= builtin history 1)"
    if [[ -n "$num" && "$num" != "$__bramble_last" ]]; then
        __bramble_last=$num
        printf '\e]7770;%s\a' "$(printf '%s\t%s\t%s' "$ec" "$PWD" "$cmd" | base64 -w0)"
    fi
}
read -r __bramble_last _ <<< "$(HISTTIMEFORMAT= builtin history 1)"
PROMPT_COMMAND=__bramble_hook
alias ls='ls --color=auto'
"#;

pub enum PtyEvent {
    Output(Vec<u8>),
    Exited,
}

pub struct Lab {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    // Held so the rc file outlives the shell's startup.
    _rcfile: tempfile::NamedTempFile,
}

impl Lab {
    pub fn spawn(
        lab_dir: &Path,
        rows: u16,
        cols: u16,
        events: UnboundedSender<PtyEvent>,
    ) -> anyhow::Result<Self> {
        let mut rcfile = tempfile::Builder::new()
            .prefix("bramble-labrc")
            .tempfile()?;
        rcfile.write_all(LAB_RC.as_bytes())?;

        std::fs::create_dir_all(lab_dir)
            .with_context(|| format!("creating lab dir {}", lab_dir.display()))?;
        let pair = native_pty_system()
            .openpty(size(rows, cols))
            .context("opening a pseudo-terminal")?;
        let mut cmd = CommandBuilder::new("bash");
        cmd.args([
            "--rcfile".as_ref(),
            rcfile.path().as_os_str(),
            "-i".as_ref(),
        ]);
        cmd.cwd(lab_dir);
        cmd.env("TERM", "xterm-256color");
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .context("starting the lab shell")?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if events.send(PtyEvent::Output(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
            // Reap the shell; its exit status doesn't matter to the TUI.
            let _ = child.wait();
            let _ = events.send(PtyEvent::Exited);
        });

        Ok(Self {
            master: pair.master,
            writer,
            _rcfile: rcfile,
        })
    }

    pub fn write(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> anyhow::Result<()> {
        self.master.resize(size(rows, cols))
    }
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}
