//! Terminal styling and the live progress line.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Return to column 0 and clear the line.
pub const CLEAR_LINE: &str = "\r\x1b[2K";

static COLOR: AtomicBool = AtomicBool::new(true);

pub fn init_color() {
    let on = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    COLOR.store(on, Ordering::Relaxed);
}

pub fn color() -> bool {
    COLOR.load(Ordering::Relaxed)
}

fn sgr(code: &str, s: &str) -> String {
    if color() { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
}

pub fn bold(s: &str) -> String { sgr("1", s) }
pub fn dim(s: &str) -> String { sgr("2", s) }
pub fn red(s: &str) -> String { sgr("31", s) }
pub fn green(s: &str) -> String { sgr("32", s) }
pub fn yellow(s: &str) -> String { sgr("33", s) }
pub fn cyan(s: &str) -> String { sgr("36", s) }

/// Clickable OSC 8 hyperlink (Ghostty, iTerm2, ...); plain URL without color.
pub fn link(text: &str, url: &str) -> String {
    if color() { format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\") } else { url.to_string() }
}

/// "  ⌂ Address    " with the icon in the accent color.
pub fn row_label(icon: &str, label: &str) -> String {
    format!("  {} {}", cyan(icon), bold(&format!("{label:<11}")))
}

/// Indent for continuation lines under a row label.
pub const ROW_INDENT: &str = "               ";

/// "35 m", "1.2 km"
pub fn distance(m: f64) -> String {
    if m >= 1000.0 { format!("{:.1} km", m / 1000.0) } else { format!("{m:.0} m") }
}

/// "just now", "12 s ago", "4 min ago"
pub fn ago(secs: f64) -> String {
    match secs {
        s if s < 2.0 => "just now".to_string(),
        s if s < 90.0 => format!("{s:.0} s ago"),
        s if s < 5400.0 => format!("{:.0} min ago", s / 60.0),
        s => format!("{:.1} h ago", s / 3600.0),
    }
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Single self-overwriting status line ("⠹ Locating…  1.2s"); a no-op when not live.
pub struct Progress {
    live: bool,
    label: &'static str,
    frame: usize,
    started: Instant,
}

impl Progress {
    pub fn new(live: bool) -> Self {
        Self { live, label: "", frame: 0, started: Instant::now() }
    }

    pub fn set(&mut self, label: &'static str) {
        self.label = label;
        self.started = Instant::now();
        self.draw();
    }

    pub fn tick(&mut self) {
        self.frame += 1;
        self.draw();
    }

    fn draw(&self) {
        if !self.live || self.label.is_empty() {
            return;
        }
        let spin = SPINNER[self.frame % SPINNER.len()];
        let secs = self.started.elapsed().as_secs_f64();
        print!("{CLEAR_LINE}  {} {}  {}", cyan(spin), self.label, dim(&format!("{secs:.1}s")));
        let _ = std::io::stdout().flush();
    }

    pub fn clear(&mut self) {
        if self.live && !self.label.is_empty() {
            print!("{CLEAR_LINE}");
            let _ = std::io::stdout().flush();
        }
        self.label = "";
    }
}
