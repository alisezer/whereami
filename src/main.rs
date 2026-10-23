//! whereami: print the machine's current location, address and time zone using CoreLocation.
//!
//! Must run from inside a signed .app bundle so macOS attributes the location
//! permission to the bundle rather than the terminal. See the README.

mod geocode;
mod location;
mod nearby;
mod ui;

use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::time::Duration;

use objc2::MainThreadMarker;
use objc2_foundation::NSISO8601DateFormatter;
use serde::Serialize;
use serde_json::json;

use geocode::Place;
use nearby::Poi;
use ui::Progress;

const USAGE: &str = "\
Usage: whereami [--json] [--offline] [--no-nearby] [--radius METRES] [--timeout SECONDS]
       whereami --status [--json]

Print where this Mac is: street address, what's around, coordinates and time zone.

Options:
  -j, --json             Print JSON (the default when stdout isn't a terminal)
  -n, --offline          Coordinates only; skip the address and nearby lookups
      --no-nearby        Skip the nearby places lookup
  -r, --radius METRES    How far to look for nearby places (default 150)
  -t, --timeout SECONDS  Seconds to wait for the fix and for the lookup (default 15)
      --status           Show the permission status without requesting anything
  -h, --help             Show this help
  -V, --version          Show version

Exit codes: 0 ok, 1 error, 2 bad arguments, 3 permission denied or restricted,
4 Location Services off, 5 timed out, 6 location found but the address or nearby lookup failed.";

const DEFAULT_TIMEOUT: f64 = 15.0;
const DEFAULT_RADIUS: f64 = 150.0;
/// Nearby places shown in the terminal view; JSON lists all of them.
const NEARBY_SHOWN: usize = 8;
/// At most this many of one category in the terminal view, so the list stays varied.
const NEARBY_PER_CATEGORY: usize = 2;
/// Useful to know about, but only shown when there's room left.
const UTILITY_CATEGORIES: [&str; 5] = ["EV charger", "Restroom", "Parking", "Car rental", "Gas station"];
/// Radius for counting what's registered at this spot (the building check).
const BUILDING_RADIUS: f64 = 25.0;
/// This many listings at one spot reads as a shared office building.
const SHARED_OFFICE_MIN: usize = 10;
/// Venues this close count as inside your building when it's a shared office.
const IN_BUILDING_RADIUS: f64 = 20.0;

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
    nearby: bool,
    radius: f64,
    status: bool,
    timeout: Duration,
}

fn positive(flag: &str, value: Option<String>) -> Result<f64, Failure> {
    let value = value.ok_or_else(|| Failure::new("usage", 2, format!("{flag} needs a value")))?;
    value
        .parse()
        .ok()
        .filter(|v: &f64| v.is_finite() && *v > 0.0)
        .ok_or_else(|| Failure::new("usage", 2, format!("invalid {flag} value: {value}")))
}

fn parse_args() -> Result<Option<Options>, Failure> {
    let mut opts = Options {
        json: !std::io::stdout().is_terminal(),
        geocode: true,
        nearby: true,
        radius: DEFAULT_RADIUS,
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
            "-n" | "--offline" => (opts.geocode, opts.nearby) = (false, false),
            "--no-nearby" => opts.nearby = false,
            "-r" | "--radius" => opts.radius = positive(&flag, inline.or_else(|| args.next()))?,
            "--status" => opts.status = true,
            "-t" | "--timeout" => {
                opts.timeout = Duration::from_secs_f64(positive(&flag, inline.or_else(|| args.next()))?);
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
    #[serde(skip)]
    radius_m: f64,
    place: Option<Place>,
    nearby: Option<Vec<Poi>>,
    building: Option<Building>,
    #[serde(skip_serializing_if = "Option::is_none")]
    geocode_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nearby_error: Option<String>,
}

#[derive(Serialize)]
struct Building {
    /// Businesses listed within BUILDING_RADIUS, categorized or not.
    listed: usize,
    /// MapKit caps results, so the real number may be higher.
    at_least: bool,
    likely_shared_office: bool,
    sample: Vec<String>,
}

fn building_from(listed: &[Poi]) -> Building {
    Building {
        listed: listed.len(),
        at_least: listed.len() >= nearby::RESULT_CAP,
        likely_shared_office: listed.iter().filter(|p| p.category.is_none()).count() >= SHARED_OFFICE_MIN,
        sample: listed.iter().take(5).map(|p| p.name.clone()).collect(),
    }
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
    let (mut nearby, mut building, mut nearby_error) = (None, None, None);
    if opts.nearby {
        match nearby::venues(&location, opts.radius, opts.timeout, progress) {
            Ok(p) => nearby = Some(p),
            Err(e) => nearby_error = Some(e),
        }
        match nearby::listed_here(&location, BUILDING_RADIUS, opts.timeout, progress) {
            Ok(p) => building = Some(building_from(&p)),
            Err(e) => nearby_error = nearby_error.or(Some(e)),
        }
        if let Some(venues) = &mut nearby {
            let shared = building.as_ref().is_some_and(|b| b.likely_shared_office);
            nearby::mark_in_building(venues, shared, IN_BUILDING_RADIUS);
            // Recognizable tenants (they have a category) make the best examples
            if let Some(b) = &mut building {
                let named: Vec<String> = venues.iter().filter(|v| v.in_building && v.distance_m <= IN_BUILDING_RADIUS).map(|v| v.name.clone()).collect();
                if !named.is_empty() {
                    b.sample = named.into_iter().take(5).collect();
                }
            }
        }
    }
    progress.clear();
    Ok(Report {
        latitude: coord.latitude,
        longitude: coord.longitude,
        horizontal_accuracy_m: accuracy,
        timestamp: NSISO8601DateFormatter::new().stringFromDate(&timestamp).to_string(),
        age_seconds: -timestamp.timeIntervalSinceNow(),
        radius_m: opts.radius,
        place,
        nearby,
        building,
        geocode_error,
        nearby_error,
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
                s += &format!("{}{}\n", ui::row_label("★", "Landmark"), p.points_of_interest.join(" · "));
            }
        }
        (None, Some(e)) => {
            s += &format!("{}{}\n", ui::row_label("⌂", "Address"), ui::red(&format!("lookup failed · {e}")));
        }
        (None, None) => {
            s += &format!("{}{}\n", ui::row_label("⌂", "Address"), ui::dim("skipped (--offline)"));
        }
    }
    if let Some(b) = r.building.as_ref().filter(|b| b.listed >= 2) {
        let count = format!("{}{}", b.listed, if b.at_least { "+" } else { "" });
        let mut text = format!("{count} businesses listed here");
        if b.likely_shared_office {
            text += &ui::dim(" · likely a shared office");
        }
        s += &format!("{}{text}\n", ui::row_label("▦", "Building"));
        if !b.sample.is_empty() {
            s += &format!("{}{}\n", ui::ROW_INDENT, ui::dim(&format!("incl. {}", b.sample.iter().take(3).cloned().collect::<Vec<_>>().join(" · "))));
        }
    }
    s += &render_nearby(r);

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

fn render_nearby(r: &Report) -> String {
    let label = ui::row_label("⌖", "Around");
    match (&r.nearby, &r.nearby_error) {
        (Some(pois), _) if pois.iter().all(|p| p.in_building) => format!("{label}{}\n", ui::dim(&format!("no venues within {}", ui::distance(r.radius_m)))),
        (Some(pois), _) => {
            // Nearest first, one entry per name, each category capped; utilities fill leftover slots
            let mut seen: Vec<&str> = Vec::new();
            let candidates: Vec<&Poi> = pois
                .iter()
                .filter(|p| !p.in_building)
                .filter(|p| {
                    let new = !seen.contains(&p.name.as_str());
                    seen.push(&p.name);
                    new
                })
                .collect();
            let is_utility = |p: &Poi| p.category.as_deref().is_some_and(|c| UTILITY_CATEGORIES.contains(&c));
            let mut per_category: Vec<(&str, usize)> = Vec::new();
            let mut shown: Vec<&Poi> = Vec::new();
            for utility_pass in [false, true] {
                for &p in candidates.iter().filter(|p| is_utility(p) == utility_pass) {
                    let cat = p.category.as_deref().unwrap_or("");
                    let n = match per_category.iter_mut().find(|(c, _)| *c == cat) {
                        Some((_, n)) => n,
                        None => {
                            per_category.push((cat, 0));
                            &mut per_category.last_mut().expect("pushed").1
                        }
                    };
                    if *n < NEARBY_PER_CATEGORY && shown.len() < NEARBY_SHOWN {
                        *n += 1;
                        shown.push(p);
                    }
                }
            }
            shown.sort_by(|a, b| a.distance_m.total_cmp(&b.distance_m));
            let width = shown.iter().map(|p| p.name.chars().count()).max().unwrap_or(0).min(28);
            let mut s = String::new();
            for (i, p) in shown.iter().enumerate() {
                let name: String = if p.name.chars().count() > width {
                    p.name.chars().take(width - 1).chain(['…']).collect()
                } else {
                    p.name.clone()
                };
                let pad = " ".repeat(width - name.chars().count());
                let details = format!("{:>6}  {}", ui::distance(p.distance_m), p.category.as_deref().unwrap_or(""));
                s += &format!(
                    "{}{}{pad}  {}\n",
                    if i == 0 { label.clone() } else { ui::ROW_INDENT.to_string() },
                    ui::bold(&name),
                    ui::dim(details.trim_end())
                );
            }
            let around = candidates.len();
            if around > shown.len() {
                s += &format!("{}{}\n", ui::ROW_INDENT, ui::dim(&format!("+{} more (--json lists all)", around - shown.len())));
            }
            s
        }
        (None, Some(e)) => format!("{label}{}\n", ui::red(&format!("lookup failed · {e}"))),
        (None, None) => String::new(),
    }
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
            let lookup_errors = [("geocode_error", &report.geocode_error), ("nearby_error", &report.nearby_error)];
            let mut failed = false;
            for (code, e) in lookup_errors {
                if let Some(e) = e {
                    failed = true;
                    if opts.json {
                        eprintln!("{}", json!({ "error": code, "message": e }));
                    }
                }
            }
            if failed { ExitCode::from(6) } else { ExitCode::SUCCESS }
        }
        Err(f) => {
            progress.clear();
            report_failure(&f, opts.json);
            ExitCode::from(f.exit)
        }
    }
}
