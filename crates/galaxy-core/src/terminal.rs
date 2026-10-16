//! PTY-backed terminal session (M6-04).
//!
//! The shell runs on a PTY; a reader thread feeds its output through
//! `alacritty_terminal`, and the UI takes [`TerminalScreen`] snapshots of the
//! visible grid: styled runs per line plus the cursor. `version` changes
//! whenever the screen may have changed, so the UI only copies the grid when
//! there is something new to draw.

use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, StdSyncHandler};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const MAX_OUTPUT_BYTES: usize = 256 * 1024;
/// Scrollback lines kept per terminal. alacritty_terminal defaults to 10k;
/// each cell is ~24 bytes, so a 200-column grid costs ~48 MB at 10k lines.
/// 5k keeps a useful history at half that worst case.
pub const SCROLLBACK_LINES: usize = 5_000;

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct TerminalSession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: SharedWriter,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    output: Arc<Mutex<Vec<u8>>>,
    grid: Arc<Mutex<Term<PtyListener>>>,
    version: Arc<AtomicU64>,
    exited: Arc<AtomicBool>,
    shell_name: String,
}

/// Sends the terminal's replies (cursor position reports, device
/// attributes) back to the shell. Prompts such as powerlevel10k wait for
/// them, so dropping them leaves the prompt half drawn.
#[derive(Clone)]
struct PtyListener {
    writer: SharedWriter,
}

impl EventListener for PtyListener {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event
            && let Ok(mut writer) = self.writer.lock()
        {
            let _ = writer.write_all(text.as_bytes());
            let _ = writer.flush();
        }
    }
}

/// A terminal color as the program asked for it. The UI maps it to its
/// theme's palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalColor {
    /// The theme's default foreground or background.
    #[default]
    Default,
    /// One of the 256 xterm colors (0–15 follow the theme).
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Consecutive cells on one line with the same style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRun {
    /// First grid column of the run.
    pub column: u16,
    pub text: String,
    pub foreground: TerminalColor,
    pub background: TerminalColor,
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub inverse: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalLine {
    /// The line's text without trailing blanks (for links and copying).
    pub text: String,
    pub runs: Vec<TerminalRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalScreen {
    pub version: u64,
    pub columns: u16,
    pub lines: Vec<TerminalLine>,
    /// Cursor row and column on screen, if it is shown and not scrolled away.
    pub cursor: Option<(u16, u16)>,
    /// Lines scrolled back into history (0 = following the output).
    pub display_offset: u32,
    pub history_size: u32,
    /// Arrow keys send `ESC O` sequences (vim, less).
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub alt_screen: bool,
    /// The program asked for mouse reports (`?1000`): clicks and wheel.
    pub mouse_click: bool,
    /// Also report motion while a button is held (`?1002`).
    pub mouse_drag: bool,
    /// Also report motion with no button held (`?1003`).
    pub mouse_motion: bool,
    /// Reports use the SGR encoding (`?1006`) rather than legacy X10.
    pub sgr_mouse: bool,
    /// Wheel becomes arrow keys on the alternate screen (`?1007`).
    pub alternate_scroll: bool,
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    foreground: TerminalColor,
    background: TerminalColor,
    flags: Flags,
}

const STYLE_FLAGS: Flags = Flags::BOLD
    .union(Flags::ITALIC)
    .union(Flags::DIM)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::STRIKEOUT)
    .union(Flags::INVERSE);

impl Style {
    fn of(cell: &Cell) -> Self {
        let mut flags = cell.flags & STYLE_FLAGS;
        let foreground = match cell.fg {
            // Dim variants are the base color drawn dimmed.
            Color::Named(named) if is_dim(named) => {
                flags |= Flags::DIM;
                color(Color::Named(undim(named)))
            }
            other => color(other),
        };
        Self {
            foreground,
            background: color(cell.bg),
            flags,
        }
    }

    fn run(self, column: usize) -> TerminalRun {
        TerminalRun {
            column: column as u16,
            text: String::new(),
            foreground: self.foreground,
            background: self.background,
            bold: self.flags.contains(Flags::BOLD),
            italic: self.flags.contains(Flags::ITALIC),
            dim: self.flags.contains(Flags::DIM),
            underline: self.flags.intersects(Flags::ALL_UNDERLINES),
            strikeout: self.flags.contains(Flags::STRIKEOUT),
            inverse: self.flags.contains(Flags::INVERSE),
        }
    }
}

fn is_dim(named: NamedColor) -> bool {
    let n = named as usize;
    (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&n)
}

fn undim(named: NamedColor) -> NamedColor {
    match named {
        NamedColor::DimBlack => NamedColor::Black,
        NamedColor::DimRed => NamedColor::Red,
        NamedColor::DimGreen => NamedColor::Green,
        NamedColor::DimYellow => NamedColor::Yellow,
        NamedColor::DimBlue => NamedColor::Blue,
        NamedColor::DimMagenta => NamedColor::Magenta,
        NamedColor::DimCyan => NamedColor::Cyan,
        NamedColor::DimWhite => NamedColor::White,
        other => other,
    }
}

fn color(color: Color) -> TerminalColor {
    match color {
        Color::Named(named) => {
            let n = named as usize;
            if n < 16 {
                TerminalColor::Indexed(n as u8)
            } else {
                TerminalColor::Default
            }
        }
        Color::Indexed(index) => TerminalColor::Indexed(index),
        Color::Spec(rgb) => TerminalColor::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

/// A run that draws nothing: blanks on the default background.
fn is_blank(run: &TerminalRun) -> bool {
    run.background == TerminalColor::Default
        && !run.inverse
        && !run.underline
        && !run.strikeout
        && run.text.chars().all(|c| c == ' ')
}

fn line_runs(term: &Term<PtyListener>, line: Line) -> TerminalLine {
    let row = &term.grid()[line];
    let mut runs: Vec<TerminalRun> = Vec::new();
    let mut current: Option<(Style, TerminalRun)> = None;
    let mut after_wide = false;
    for column in 0..term.columns() {
        let cell = &row[Column(column)];
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let style = Style::of(cell);
        // A wide character ends its run, so the next one starts at its own
        // column even if the font draws the wide glyph narrower.
        let continues = matches!(&current, Some((s, _)) if *s == style) && !after_wide;
        if !continues && let Some((_, run)) = current.take() {
            runs.push(run);
        }
        let (_, run) = current.get_or_insert_with(|| (style, style.run(column)));
        run.text.push(if cell.flags.contains(Flags::HIDDEN) {
            ' '
        } else {
            cell.c
        });
        if let Some(extra) = cell.zerowidth() {
            run.text.extend(extra.iter());
        }
        after_wide = cell.flags.contains(Flags::WIDE_CHAR);
    }
    if let Some((_, run)) = current {
        runs.push(run);
    }
    while runs.last().is_some_and(is_blank) {
        runs.pop();
    }
    if let Some(last) = runs.last_mut()
        && last.background == TerminalColor::Default
        && !last.inverse
    {
        let trimmed = last.text.trim_end_matches(' ').len();
        last.text.truncate(trimmed);
    }
    let text = runs
        .iter()
        .map(|run| run.text.as_str())
        .collect::<String>()
        .trim_end()
        .to_owned();
    TerminalLine { text, runs }
}

impl TerminalSession {
    pub fn spawn(cwd: &Path, cols: u16, rows: u16) -> io::Result<Self> {
        let cols = cols.max(2);
        let rows = rows.max(1);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let shell_name = Path::new(&shell)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "shell".into());
        let mut command = CommandBuilder::new(&shell);
        // A login shell, like Terminal.app and VS Code on macOS.
        command.arg("-l");
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "Galaxy");
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(io::Error::other)?;
        // The child holds its own copy of the slave side.
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let writer: SharedWriter = Arc::new(Mutex::new(
            pair.master.take_writer().map_err(io::Error::other)?,
        ));
        let output = Arc::new(Mutex::new(Vec::new()));
        let grid = Arc::new(Mutex::new(Term::new(
            Config {
                scrolling_history: SCROLLBACK_LINES,
                ..Config::default()
            },
            &TerminalSize {
                cols: cols as usize,
                rows: rows as usize,
            },
            PtyListener {
                writer: Arc::clone(&writer),
            },
        )));
        let version = Arc::new(AtomicU64::new(1));
        let exited = Arc::new(AtomicBool::new(false));

        let output_for_reader = Arc::clone(&output);
        let grid_for_reader = Arc::clone(&grid);
        let version_for_reader = Arc::clone(&version);
        let exited_for_reader = Arc::clone(&exited);
        thread::Builder::new()
            .name("galaxy-terminal-reader".into())
            .spawn(move || {
                let mut reader = reader;
                let mut parser = Processor::<StdSyncHandler>::new();
                let mut chunk = [0u8; 8192];
                while let Ok(count) = reader.read(&mut chunk) {
                    if count == 0 {
                        break;
                    }
                    {
                        let mut output = output_for_reader.lock().unwrap();
                        output.extend_from_slice(&chunk[..count]);
                        if output.len() > MAX_OUTPUT_BYTES {
                            let drain = output.len() - MAX_OUTPUT_BYTES;
                            output.drain(..drain);
                        }
                    }
                    parser.advance(&mut *grid_for_reader.lock().unwrap(), &chunk[..count]);
                    version_for_reader.fetch_add(1, Ordering::Release);
                }
                exited_for_reader.store(true, Ordering::Release);
                version_for_reader.fetch_add(1, Ordering::Release);
            })
            .map_err(io::Error::other)?;
        Ok(Self {
            master: Mutex::new(pair.master),
            writer,
            child: Mutex::new(child),
            output,
            grid,
            version,
            exited,
            shell_name,
        })
    }

    /// Raw output since the last call (capped), for tests and logging.
    pub fn take_output(&self) -> String {
        let mut output = self.output.lock().unwrap();
        String::from_utf8_lossy(&std::mem::take(&mut *output)).into_owned()
    }

    /// Typing jumps back to the live output, like other terminals.
    pub fn write(&self, input: &[u8]) -> io::Result<()> {
        {
            let mut grid = self.grid.lock().unwrap();
            if grid.grid().display_offset() != 0 {
                grid.scroll_display(Scroll::Bottom);
                self.version.fetch_add(1, Ordering::Release);
            }
        }
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(input)?;
        writer.flush()
    }

    /// Resizes the PTY and the grid together, so the shell and the screen
    /// agree on the line width.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let cols = cols.max(2);
        let rows = rows.max(1);
        self.master
            .lock()
            .unwrap()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)?;
        self.grid.lock().unwrap().resize(TerminalSize {
            cols: cols as usize,
            rows: rows as usize,
        });
        self.version.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Scrolls the view through history; positive `lines` goes back.
    pub fn scroll(&self, lines: i32) {
        if lines == 0 {
            return;
        }
        self.grid
            .lock()
            .unwrap()
            .scroll_display(Scroll::Delta(lines));
        self.version.fetch_add(1, Ordering::Release);
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    pub fn shell_name(&self) -> &str {
        &self.shell_name
    }

    pub fn is_running(&self) -> bool {
        if self.exited.load(Ordering::Acquire) {
            return false;
        }
        matches!(self.child.lock().unwrap().try_wait(), Ok(None))
    }

    pub fn kill(&self) {
        let _ = self.child.lock().unwrap().kill();
    }

    pub fn screen(&self) -> TerminalScreen {
        // Read the version first: a change after this is picked up next time.
        let version = self.version();
        let term = self.grid.lock().unwrap();
        let grid = term.grid();
        let offset = grid.display_offset();
        let rows = term.screen_lines();
        let lines = (0..rows)
            .map(|row| line_runs(&term, Line(row as i32 - offset as i32)))
            .collect();
        let mode = *term.mode();
        let point = grid.cursor.point;
        let cursor_row = point.line.0 + offset as i32;
        let cursor = (mode.contains(TermMode::SHOW_CURSOR)
            && (0..rows as i32).contains(&cursor_row))
        .then_some((cursor_row as u16, point.column.0 as u16));
        TerminalScreen {
            version,
            columns: term.columns() as u16,
            lines,
            cursor,
            display_offset: offset as u32,
            history_size: grid.history_size() as u32,
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            mouse_click: mode.contains(TermMode::MOUSE_REPORT_CLICK),
            mouse_drag: mode.contains(TermMode::MOUSE_DRAG),
            mouse_motion: mode.contains(TermMode::MOUSE_MOTION),
            sgr_mouse: mode.contains(TermMode::SGR_MOUSE),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for(
        session: &TerminalSession,
        what: impl Fn(&TerminalScreen) -> bool,
    ) -> TerminalScreen {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let _ = session.take_output();
            let screen = session.screen();
            if what(&screen) || Instant::now() > deadline {
                return screen;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn shell_session_reads_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let session = TerminalSession::spawn(dir.path(), 80, 24).unwrap();
        session.write(b"printf 'galaxy-pty\\n'\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = String::new();
        while Instant::now() < deadline {
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
        let screen = wait_for(&session, |s| {
            s.lines.iter().any(|line| line.text.contains("line-10049"))
        });
        assert_eq!(screen.lines.len(), 24);
        assert!(screen.history_size > 0);

        session.scroll(10);
        let back = session.screen();
        assert_eq!(back.display_offset, 10);
        assert_eq!(back.cursor, None, "cursor scrolled out of view");
        session.write(b"").unwrap();
        assert_eq!(session.screen().display_offset, 0, "typing follows output");
    }

    #[test]
    fn colors_become_runs_and_resize_reflows_the_grid() {
        let dir = tempfile::tempdir().unwrap();
        let session = TerminalSession::spawn(dir.path(), 80, 24).unwrap();
        session
            .write(b"printf 'plain \\033[1;31mred\\033[0m \\033[38;2;1;2;3mrgb\\033[0m\\n'\n")
            .unwrap();
        let screen = wait_for(&session, |s| {
            s.lines
                .iter()
                .any(|line| line.text.starts_with("plain red rgb"))
        });
        let line = screen
            .lines
            .iter()
            .find(|line| line.text.starts_with("plain red rgb"))
            .expect("colored line");
        let red = line
            .runs
            .iter()
            .find(|run| run.text == "red")
            .expect("red run");
        assert_eq!(red.column, 6);
        assert!(red.bold);
        assert_eq!(red.foreground, TerminalColor::Indexed(1));
        let rgb = line
            .runs
            .iter()
            .find(|run| run.text == "rgb")
            .expect("rgb run");
        assert_eq!(rgb.foreground, TerminalColor::Rgb(1, 2, 3));
        assert!(screen.cursor.is_some());

        session.resize(40, 10).unwrap();
        let resized = session.screen();
        assert_eq!(resized.columns, 40);
        assert_eq!(resized.lines.len(), 10);
    }

    #[test]
    fn mouse_modes_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let session = TerminalSession::spawn(dir.path(), 80, 24).unwrap();
        let idle = session.screen();
        assert!(!idle.mouse_click && !idle.mouse_drag && !idle.sgr_mouse);

        session
            .write(b"printf '\\033[?1049h\\033[?1002h\\033[?1006h'\n")
            .unwrap();
        let on = wait_for(&session, |s| s.mouse_drag);
        assert!(on.alt_screen);
        assert!(on.mouse_drag);
        assert!(on.sgr_mouse);
        assert!(!on.mouse_motion);

        session
            .write(b"printf '\\033[?1002l\\033[?1006l\\033[?1049l'\n")
            .unwrap();
        let off = wait_for(&session, |s| !s.mouse_drag);
        assert!(!off.mouse_drag && !off.sgr_mouse && !off.alt_screen);
    }

    #[test]
    fn exit_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let session = TerminalSession::spawn(dir.path(), 80, 24).unwrap();
        assert!(session.is_running());
        assert!(!session.shell_name().is_empty());
        session.write(b"exit\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!session.is_running());
    }
}
