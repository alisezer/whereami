//! whereami: print the machine's current location, address and time zone using CoreLocation.
//!
//! Must run from inside a signed .app bundle so macOS attributes the location
//! permission to the bundle rather than the terminal. See the README.

mod geocode;
mod location;
mod ui;

use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::time::Duration;

use objc2::MainThreadMarker;
use objc2_foundation::NSISO8601DateFormatter;
use serde::Serialize;
use serde_json::json;

use geocode::Place;
use ui::Progress;

const USAGE: &str = "\
Usage: whereami [--json] [--no-geocode] [--timeout SECONDS]
       whereami --status [--json]

Print where this Mac is: street address, area, coordinates and time zone.

Options:
  -j, --json             Print JSON (the default when stdout isn't a terminal)
  -n, --no-geocode       Coordinates only; skip the address lookup (works offline)
  -t, --timeout SECONDS  Seconds to wait for the fix and for the lookup (default 15)
      --status           Show the permission status without requesting anything
  -h, --help             Show this help
  -V, --version          Show version

Exit codes: 0 ok, 1 error, 2 bad arguments, 3 permission denied or restricted,
4 Location Services off, 5 timed out, 6 location found but the address lookup failed.";

const DEFAULT_TIMEOUT: f64 = 15.0;

pub struct Failure {
    code: &'static str,
    message: String,
    exit: u8,
    settings_link: bool,
}

impl Failure {
    pub fn new(code: &'static str, exit: u8, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), exit, settings_link: false }
    }

    /// Offer a link to the Location Services pane in human output.
    pub fn with_settings_link(mut self) -> Self {
        self.settings_link = true;
        self
    }
}

struct Options {
    json: bool,
    geocode: bool,
    status: bool,
    timeout: Duration,
}

fn parse_args() -> Result<Option<Options>, Failure> {
    let mut opts = Options {
        json: !std::io::stdout().is_terminal(),
        geocode: true,
        status: false,
        timeout: Duration::from_secs_f64(DEFAULT_TIMEOUT),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        match flag.as_str() {
            "-j" | "--json" => opts.json = true,
            "-n" | "--no-geocode" => opts.geocode = false,
            "--status" => opts.status = true,
            "-t" | "--timeout" => {
                let value = inline
                    .or_else(|| args.next())
                    .ok_or_else(|| Failure::new("usage", 2, "--timeout needs a value"))?;
                let secs: f64 = value
                    .parse()
                    .ok()
                    .filter(|s: &f64| s.is_finite() && *s > 0.0)
                    .ok_or_else(|| {
                        Failure::new("usage", 2, format!("invalid --timeout value: {value}"))
                    })?;
                opts.timeout = Duration::from_secs_f64(secs);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("whereami {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            _ => return Err(Failure::new("usage", 2, format!("unknown argument: {arg}"))),
        }
    }
    Ok(Some(opts))
}

#[derive(Serialize)]
struct Report {
    latitude: f64,
    longitude: f64,
    horizontal_accuracy_m: f64,
    timestamp: String,
    #[serde(skip)]
    age_seconds: f64,
    place: Option<Place>,
    #[serde(skip_serializing_if = "Option::is_none")]
    geocode_error: Option<String>,
}

fn locate(mtm: MainThreadMarker, opts: &Options, progress: &mut Progress) -> Result<Report, Failure> {
    let location = location::get_location(mtm, opts.timeout, progress)?;
    let (coord, accuracy, timestamp) =
        unsafe { (location.coordinate(), location.horizontalAccuracy(), location.timestamp()) };
    let (place, geocode_error) = if opts.geocode {
        match geocode::reverse_geocode(&location, opts.timeout, progress) {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(e)),
        }
    } else {
        (None, None)
    };
    progress.clear();
    Ok(Report {
        latitude: coord.latitude,
        longitude: coord.longitude,
        horizontal_accuracy_m: accuracy,
        timestamp: NSISO8601DateFormatter::new().stringFromDate(&timestamp).to_string(),
        age_seconds: -timestamp.timeIntervalSinceNow(),
        place,
        geocode_error,
    })
}

/// "UTC+2", "UTC+5:30", "UTC−3"
fn utc_offset(secs: isize) -> String {
    let sign = if secs < 0 { "−" } else { "+" };
    let (h, m) = (secs.abs() / 3600, secs.abs() % 3600 / 60);
    if m == 0 { format!("UTC{sign}{h}") } else { format!("UTC{sign}{h}:{m:02}") }
}

/// Keep the first occurrence of each value, dropping ones equal to anything in `exclude`.
fn unique<'a>(values: impl IntoIterator<Item = &'a Option<String>>, exclude: &[&Option<String>]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values.into_iter().flatten() {
        if !out.contains(v) && !exclude.iter().any(|e| e.as_deref() == Some(v.as_str())) {
            out.push(v.clone());
        }
    }
    out
}

fn render(r: &Report) -> String {
    let mut s = String::new();
    let place = r.place.as_ref();

    let summary = place
        .map(|p| unique([&p.street_line(), &p.city], &[]).join(", "))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{:.5}, {:.5}", r.latitude, r.longitude));
    s += &format!("\n  {}  {}\n\n", ui::bold("whereami"), ui::dim(&summary));

    match (place, &r.geocode_error) {
        (Some(p), _) => {
            let lines: Vec<String> = match &p.address {
                Some(a) => a.lines().map(str::to_string).collect(),
                None => unique([&p.street_line(), &p.city, &p.country], &[]),
            };
            for (i, line) in lines.iter().enumerate() {
                let label = if i == 0 { ui::row_label("⌂", "Address") } else { ui::ROW_INDENT.to_string() };
                let text = if i == 0 { ui::bold(line) } else { line.clone() };
                s += &format!("{label}{text}\n");
            }
            let area = unique([&p.neighbourhood, &p.district, &p.region], &[&p.city]);
            if !area.is_empty() {
                s += &format!("{}{}\n", ui::row_label("◍", "Area"), area.join(" · "));
            }
            if !p.points_of_interest.is_empty() {
                s += &format!("{}{}\n", ui::row_label("★", "Nearby"), p.points_of_interest.join(" · "));
            }
        }
        (None, Some(e)) => {
            s += &format!("{}{}\n", ui::row_label("⌂", "Address"), ui::red(&format!("lookup failed · {e}")));
        }
        (None, None) => {
            s += &format!("{}{}\n", ui::row_label("⌂", "Address"), ui::dim("skipped (--no-geocode)"));
        }
    }

    let acc = r.horizontal_accuracy_m;
    let acc_text = format!("±{}", ui::distance(acc));
    let acc_text = match acc {
        a if a <= 50.0 => ui::green(&acc_text),
        a if a <= 500.0 => ui::yellow(&acc_text),
        _ => ui::red(&acc_text),
    };
    s += &format!(
        "{}{:.6}, {:.6}  {acc_text}\n",
        ui::row_label("◎", "Position"),
        r.latitude,
        r.longitude
    );
    // Wi-Fi positioning is typically ±35 m, which spans several buildings
    match acc {
        a if a > 100.0 && place.is_some() => {
            s += &format!("{}{}\n", ui::ROW_INDENT, ui::dim("low accuracy: the street may be off"));
        }
        a if a > 15.0 && place.is_some_and(|p| p.house_number.is_some()) => {
            s += &format!("{}{}\n", ui::ROW_INDENT, ui::dim("house number is approximate"));
        }
        _ => {}
    }

    if let Some(tz) = place.and_then(|p| p.timezone.as_ref()) {
        let mut details = vec![utc_offset(tz.utc_offset_seconds)];
        details.extend(tz.abbreviation.clone().filter(|a| !a.starts_with("GMT")));
        s += &format!("{}{}  {}\n", ui::row_label("◷", "Time zone"), tz.id, ui::dim(&details.join(" · ")));
    }

    let url = format!("https://maps.apple.com/?ll={:.6},{:.6}&q=Here", r.latitude, r.longitude);
    s += &format!("{}{}\n", ui::row_label("↗", "Maps"), ui::cyan(&ui::link("Open in Apple Maps", &url)));

    s += &format!("\n  {}\n\n", ui::dim(&format!("fix {} · {}", ui::ago(r.age_seconds), r.timestamp)));
    s
}

fn report_failure(f: &Failure, json: bool) {
    if json {
        eprintln!("{}", json!({ "error": f.code, "message": f.message }));
        return;
    }
    let mut msg = format!("{} {}", ui::red("whereami:"), f.message);
    if f.settings_link {
        msg += &format!("\n  {}", ui::cyan(&ui::link("Open Location Services settings", location::SETTINGS_URL)));
    }
    eprintln!("{msg}");
}

fn print(text: &str) {
    // Ignore write errors (e.g. piped into `head`) instead of panicking
    let _ = std::io::stdout().write_all(text.as_bytes());
}

fn main() -> ExitCode {
    ui::init_color();
    let opts = match parse_args() {
        Ok(Some(o)) => o,
        Ok(None) => return ExitCode::SUCCESS,
        Err(f) => {
            report_failure(&f, !std::io::stdout().is_terminal());
            return ExitCode::from(f.exit);
        }
    };
    let mtm = MainThreadMarker::new().expect("whereami must run on the main thread");
    let mut progress = Progress::new(!opts.json);

    if opts.status {
        let (enabled, auth) = location::status(mtm, &mut progress);
        progress.clear();
        if opts.json {
            print(&format!(
                "{:#}\n",
                json!({ "location_services_enabled": enabled, "authorization": auth })
            ));
        } else {
            let on = if enabled { ui::green("on") } else { ui::red("off") };
            let auth_text = match auth {
                "authorized" | "authorized_when_in_use" => ui::green(auth),
                "not_determined" => ui::yellow(auth),
                _ => ui::red(auth),
            };
            print(&format!(
                "\n{}{on}\n{}{auth_text}\n\n",
                ui::row_label("◉", "Services"),
                ui::row_label("⚿", "whereami")
            ));
        }
        return ExitCode::SUCCESS;
    }

    match locate(mtm, &opts, &mut progress) {
        Ok(report) => {
            if opts.json {
                let text = serde_json::to_string_pretty(&report).expect("report serializes");
                print(&format!("{text}\n"));
            } else {
                print(&render(&report));
            }
            if let Some(e) = &report.geocode_error {
                if opts.json {
                    eprintln!("{}", json!({ "error": "geocode_error", "message": e }));
                }
                return ExitCode::from(6);
            }
            ExitCode::SUCCESS
        }
        Err(f) => {
            progress.clear();
            report_failure(&f, opts.json);
            ExitCode::from(f.exit)
        }
    }
}
