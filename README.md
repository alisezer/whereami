# whereami

Shows where this Mac is: street address, neighbourhood, coordinates and time
zone. It calls CoreLocation directly through the `objc2` crates, with no
third-party location tools.

```console
$ whereami

  whereami  1 Apple Park Way, Cupertino

  ⌂ Address    1 Apple Park Way
               Cupertino CA 95014
               United States
  ◍ Area       Santa Clara · California
  ★ Nearby     Apple Park
  ◎ Position   37.334900, -122.009020  ±35 m
               house number is approximate
  ◷ Time zone  America/Los_Angeles  UTC−7
  ↗ Maps       Open in Apple Maps

  fix just now · 2026-10-23T08:38:11Z
```

The example output above is illustrative. whereami keeps no history, and its
only network call is the reverse-geocoding request to Apple (skipped with
`--no-geocode`).

In a terminal it shows a live spinner while it locates you and looks up the
address, then the styled view above. "Open in Apple Maps" is a clickable link
in Ghostty. Colour is off with `NO_COLOR` or when output isn't a terminal.

## Usage

```
whereami [--json] [--no-geocode] [--timeout SECONDS]
whereami --status [--json]
```

- `-j, --json` prints JSON. This is the default when stdout isn't a terminal,
  as with `speed`.
- `-n, --no-geocode` prints coordinates only and skips the address lookup,
  which needs the network.
- `-t, --timeout` (default 15 s) applies to the location fix and, separately,
  to the address lookup. The permission prompt has its own 120 s allowance, so
  a first run doesn't time out while you read the dialog.
- `--status` shows whether Location Services is on and whether whereami is
  authorized, without requesting anything. After a rebuild it reports
  `not_determined` until the next real run (see *Rebuilds* below).

### JSON

```json
{
  "latitude": 37.3349, "longitude": -122.00902, "horizontal_accuracy_m": 35.0,
  "timestamp": "2026-10-23T08:38:11Z",
  "place": {
    "address": "1 Apple Park Way\nCupertino CA 95014\nUnited States",
    "name": "1 Apple Park Way",
    "street": "Apple Park Way", "house_number": "1",
    "neighbourhood": null, "postal_code": "95014",
    "city": "Cupertino", "district": "Santa Clara", "region": "California",
    "country": "United States", "country_code": "US",
    "points_of_interest": ["Apple Park"],
    "timezone": { "id": "America/Los_Angeles", "abbreviation": "PDT", "utc_offset_seconds": -25200 }
  }
}
```

- `place.address` is formatted the way addresses are written in that country,
  using `CNPostalAddressFormatter`. Formatting an address doesn't need Contacts
  permission.
- Missing fields are `null`. `place` is `null` with `--no-geocode`.
- If the lookup fails, `place` is `null`, `geocode_error` explains why, and
  the exit code is 6.

### Accuracy

Macs locate themselves from nearby Wi-Fi networks, typically within ±35 m. That
is enough to get the street right, but the house number is only the nearest
guess and can change between runs. The view adds a note when the house number
is approximate, and another when accuracy is worse than 100 m.

### Errors

Errors go to stderr: a one-line message (with a link to Location Services
settings where relevant), or `{"error": "<code>", "message": "..."}` in JSON
mode.

| exit | error codes |
|---|---|
| 1 | `location_error` |
| 2 | `usage` |
| 3 | `denied`, `restricted` |
| 4 | `location_services_disabled` |
| 5 | `timeout`, `authorization_timeout` |
| 6 | `geocode_error`: the location is still printed, but the address lookup failed |

## Build and install

Requires the Rust toolchain and Xcode command-line tools (`codesign`).

```sh
make install     # cargo build --release, assemble + ad-hoc sign the bundle, install
make lint        # cargo clippy -D warnings
```

This installs:

- `~/Applications/whereami.app`: a minimal bundle (`io.sezer.whereami`,
  `LSUIElement`, `NSLocationUsageDescription`), ad-hoc signed and registered
  with Launch Services.
- `~/.local/bin/whereami`: a two-line `sh` wrapper that `exec`s
  `whereami.app/Contents/MacOS/whereami`.

Override locations with `make install APP_DIR=... BIN_DIR=...`.

## Why direct exec is enough (permission attribution)

locationd identifies location clients by the calling executable, not by the
TCC "responsible process" (the terminal). When the executable lives inside a
bundle, locationd resolves it to the bundle identifier. Verified with
`/usr/bin/log stream --predicate 'process == "locationd"'`:

- bundled binary → client `io.sezer.whereami`, prompt posted for
  `bundlepath = ~/Applications/whereami.app`
- bare `target/release/whereami` → client keyed by its executable path

Neither is attributed to Ghostty, so the wrapper uses plain `exec` rather than
`open -W --stdout/--stderr`. That keeps stdin/stdout, exit codes and Ctrl-C
working normally.

## Rebuilds

Ad-hoc signatures have no stable signing identity, so every `make install`
changes the code hash. The grant stays keyed to the bundle ID, but locationd
checks it again on the next run, which may show the prompt again.

## Testing the denied path

Denial can't be simulated without changing settings. To test it yourself:

1. System Settings > Privacy & Security > Location Services, switch **whereami** off.
2. `whereami; echo $?`. Expect
   `{"error":"denied","message":"Location access was denied. Enable whereami in System Settings > Privacy & Security > Location Services, then run it again."}`
   and exit 3.
3. Switch it back on.

To test the services-off path, turn off the main Location Services switch in
the same pane. Expect `location_services_disabled` and exit 4.

## Uninstall and revoke

```sh
make uninstall   # unregister from Launch Services, remove the bundle and wrapper
```

The permission entry is kept by locationd. To revoke it, switch **whereami**
off in System Settings > Privacy & Security > Location Services (or remove it
there if your macOS version offers that). `tccutil` doesn't manage Location
Services.

## License

MIT
