//! PTY-backed terminal session (M6-04).

use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

const MAX_OUTPUT_BYTES: usize = 256 * 1024;

pub struct TerminalSession {
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output: Arc<Mutex<Vec<u8>>>,
    grid: Arc<Mutex<Term<VoidListener>>>,
}

#[derive(Clone, Copy, Default)]
struct VoidListener;
impl EventListener for VoidListener {
    fn send_event(&self, _event: Event) {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalLine {
    pub text: String,
    pub foreground: u8,
    pub background: u8,
    pub bold: bool,
    pub underline: bool,
}

struct TerminalSize {
    cols: usize,
    rows: usize,
}
impl Dimensions for TerminalSize {
    fn columns(&self) -> usize {
        self.cols
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn total_lines(&self) -> usize {
        self.rows
    }
}

impl TerminalSession {
    pub fn spawn(cwd: &Path, cols: u16, rows: u16) -> io::Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut command = CommandBuilder::new(shell);
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        pair.slave
            .spawn_command(command)
            .map_err(io::Error::other)?;
        let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let writer = pair.master.take_writer().map_err(io::Error::other)?;
        let output = Arc::new(Mutex::new(Vec::new()));
        let grid = Arc::new(Mutex::new(Term::new(
            Config::default(),
            &TerminalSize {
                cols: cols as usize,
                rows: rows as usize,
            },
            VoidListener,
        )));
        let parser = Arc::new(Mutex::new(Processor::<
            alacritty_terminal::vte::ansi::StdSyncHandler,
        >::new()));
        let output_for_reader = Arc::clone(&output);
        let grid_for_reader = Arc::clone(&grid);
        let parser_for_reader = Arc::clone(&parser);
        thread::Builder::new()
            .name("galaxy-terminal-reader".into())
            .spawn(move || {
                let mut reader = reader;
                let mut chunk = [0u8; 8192];
                while let Ok(count) = reader.read(&mut chunk) {
                    if count == 0 {
                        break;
                    }
                    let mut output = output_for_reader.lock().unwrap();
                    output.extend_from_slice(&chunk[..count]);
                    if output.len() > MAX_OUTPUT_BYTES {
                        let drain = output.len() - MAX_OUTPUT_BYTES;
                        output.drain(..drain);
                    }
                    let mut parser = parser_for_reader.lock().unwrap();
                    let mut grid = grid_for_reader.lock().unwrap();
                    parser.advance(&mut *grid, &chunk[..count]);
                }
            })
            .map_err(io::Error::other)?;
        Ok(Self {
            master: Arc::new(Mutex::new(pair.master)),
            writer: Arc::new(Mutex::new(writer)),
            output,
            grid,
        })
    }

    pub fn take_output(&self) -> String {
        let mut output = self.output.lock().unwrap();
        String::from_utf8_lossy(&std::mem::take(&mut *output)).into_owned()
    }

    pub fn write(&self, input: &[u8]) -> io::Result<()> {
        self.writer.lock().unwrap().write_all(input)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        self.master
            .lock()
            .unwrap()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)
    }

    pub fn snapshot(&self) -> Vec<TerminalLine> {
        let grid = self.grid.lock().unwrap();
        (0..grid.screen_lines())
            .map(|line| {
                let row = &grid.grid()[Line(line as i32)];
                let mut text = String::new();
                let mut foreground = 0;
                let mut background = 0;
                let mut bold = false;
                let mut underline = false;
                for column in 0..grid.columns() {
                    let cell = &row[Column(column)];
                    text.push(cell.c);
                    foreground = 0;
                    background = 0;
                    bold |= cell
                        .flags
                        .contains(alacritty_terminal::term::cell::Flags::BOLD);
                    underline |= cell
                        .flags
                        .contains(alacritty_terminal::term::cell::Flags::UNDERLINE);
                }
                TerminalLine {
                    text,
                    foreground,
                    background,
                    bold,
                    underline,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn shell_session_reads_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let session = TerminalSession::spawn(dir.path(), 80, 24).unwrap();
        session.write(b"printf 'galaxy-pty\\n'\n").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut output = String::new();
        while std::time::Instant::now() < deadline {
            output.push_str(&session.take_output());
            if output.contains("galaxy-pty") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(output.contains("galaxy-pty"), "terminal output: {output:?}");
    }

    #[test]
    fn grid_keeps_viewport_after_scrollback() {
        let dir = tempfile::tempdir().unwrap();
        let session = TerminalSession::spawn(dir.path(), 80, 24).unwrap();
        session
            .write(b"i=0; while [ $i -lt 10050 ]; do echo line-$i; i=$((i+1)); done\n")
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let _ = session.take_output();
            if session
                .snapshot()
                .iter()
                .any(|line| line.text.contains("line-10049"))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(session.snapshot().len(), 24);
    }
}
