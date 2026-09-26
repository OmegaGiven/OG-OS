use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

pub struct PtySession {
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub rows: u16,
    pub cols: u16,
    child: Box<dyn portable_pty::Child + Send>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

/// Write bytes to the pty (input for the child process). Errors ignored —
/// the child may have exited.
pub fn send_input(writer: &Arc<Mutex<Box<dyn Write + Send>>>, bytes: &[u8]) {
    if let Ok(mut w) = writer.lock() {
        let _ = w.write_all(bytes);
        let _ = w.flush();
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

pub fn spawn_btop(rows: u16, cols: u16) -> Result<PtySession, Box<dyn std::error::Error>> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new("btop");
    // The app is launched from sway/waybar where TERM is unset; without it
    // btop can't do cursor addressing and every refresh scrolls the screen.
    cmd.env("TERM", "xterm-256color");
    let child = pair.slave.spawn_command(cmd)?;

    let mut reader = pair.master.try_clone_reader()?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
    let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
    let parser_clone = Arc::clone(&parser);

    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut rewriter = HvpRewriter::default();
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    rewriter.rewrite(&mut buf[..n]);
                    if let Ok(mut p) = parser_clone.lock() {
                        p.process(&buf[..n]);
                    }
                }
            }
        }
    });

    Ok(PtySession {
        parser,
        writer,
        rows,
        cols,
        child,
        _master: pair.master,
    })
}

/// The vt100 crate ignores HVP (`CSI row;col f`), which btop uses exclusively
/// for absolute cursor positioning. Rewrite the final byte to the equivalent
/// CUP (`H`) as bytes stream through. Tracks CSI state across read chunks so
/// sequences split over chunk boundaries are still rewritten.
#[derive(Default)]
struct HvpRewriter {
    state: RewriteState,
}

#[derive(Default, PartialEq)]
enum RewriteState {
    #[default]
    Ground,
    Esc,
    Csi,
}

impl HvpRewriter {
    fn rewrite(&mut self, buf: &mut [u8]) {
        for b in buf.iter_mut() {
            match self.state {
                RewriteState::Ground => {
                    if *b == 0x1b {
                        self.state = RewriteState::Esc;
                    }
                }
                RewriteState::Esc => {
                    self.state = if *b == b'[' { RewriteState::Csi } else { RewriteState::Ground };
                }
                RewriteState::Csi => match *b {
                    b'0'..=b'9' | b';' | b'?' => {}
                    b'f' => {
                        *b = b'H';
                        self.state = RewriteState::Ground;
                    }
                    _ => self.state = RewriteState::Ground,
                },
            }
        }
    }
}

/// Convert a vt100 color to an iced Color.
pub fn vt_color(c: vt100::Color, default: iced::Color) -> iced::Color {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => ansi256(i),
        vt100::Color::Rgb(r, g, b) => iced::Color::from_rgb8(r, g, b),
    }
}

fn ansi256(i: u8) -> iced::Color {
    let table: [(u8, u8, u8); 16] = [
        (0,0,0),(170,0,0),(0,170,0),(170,85,0),
        (0,0,170),(170,0,170),(0,170,170),(170,170,170),
        (85,85,85),(255,85,85),(85,255,85),(255,255,85),
        (85,85,255),(255,85,255),(85,255,255),(255,255,255),
    ];
    if (i as usize) < table.len() {
        let (r, g, b) = table[i as usize];
        return iced::Color::from_rgb8(r, g, b);
    }
    if i >= 232 {
        let v = (8 + (i - 232) as u32 * 10).min(255) as u8;
        return iced::Color::from_rgb8(v, v, v);
    }
    let n = i - 16;
    let b = n % 6;
    let g = (n / 6) % 6;
    let r = n / 36;
    let to_byte = |x: u8| if x == 0 { 0 } else { 55 + x * 40 };
    iced::Color::from_rgb8(to_byte(r), to_byte(g), to_byte(b))
}
