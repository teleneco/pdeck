# pdeck

[English](./README.md) | [日本語](./README.ja.md)

`pdeck` is a terminal probe deck for watching network reachability across multiple targets from one screen.

It can mix ICMP, TCP connect, and HTTP/HTTPS checks in a single target file, then show recent status, packet loss, dead hosts, and RTT history in a TUI.

See [CHANGELOG.md](./CHANGELOG.md) for release history.

## Features

- Mix ICMP, TCP, HTTP, and HTTPS targets in one file
- Watch multiple hosts in a live terminal UI
- Jump between dead hosts quickly
- Record probe sessions as JSONL
- Replay recorded sessions later

## Quick Start

Run with the default `targets.txt` in the current directory:

```sh
cargo run
```

Run with a different target file:

```sh
cargo run -- -f targets-mixed.txt
```

## Target File Format

Each non-empty line is:

```text
target description
```

Examples:

```text
8.8.8.8 google-dns
1.1.1.1 cloudflare
tcp://1.1.1.1:53 cloudflare-dns-tcp
https://example.com example-web
http://example.com:8080 local-http
```

Parsing rules:

- `host`: ICMP
- `tcp://host:port`: TCP connect
- `host:port`: TCP connect
- `http://host[:port]`: HTTP
- `https://host[:port]`: HTTPS

See `targets-mixed.txt` for a mixed example.

If you want to read macOS `arp -a` style output directly, use `-A`.

## Common Options

```text
-f <FILE>                 Target file, default targets.txt
-i <DURATION>             Probe interval, default 500ms
-t <DURATION>             Per-probe timeout, default 3s
-A                        Parse macOS arp -a style entries
-c, --concurrency <N>     Maximum concurrent TCP/HTTP probes
--icmp-backend <BACKEND>  auto, exec, or api
--record [FILE]           Write JSONL session events
--record-size-limit SIZE  Rotate record files after this size, default 0
--no-tui                  Print live probe results without opening the TUI
```

Recorded sessions are handled with subcommands:

```sh
pdeck replay <FILE>
pdeck replay --only <FILE>
pdeck stats <FILE> [-o FILE]
pdeck stats --only <FILE> [-o FILE]
pdeck log <FILE> [-o FILE]
pdeck log --only <FILE> [-o FILE]
```

ICMP backend defaults:

- macOS: `exec`
- Windows: `api`
- Linux: `exec`

The `exec` backend runs `ping` with `LC_ALL=C` and `LANG=C` to reduce locale-dependent output differences.
The current Windows `api` backend uses the IPv4 ICMP API path internally; if
IPv6 ICMP coverage is needed, verify the target behavior on Windows before
depending on it operationally.

## Controls

```text
Up/Down      Select host
d / D        Jump to next/previous dead host
Ctrl+S       Pause/resume
q / Esc      Quit
Ctrl+C       Quit
```

## Recording And Replay

Record a live session:

```sh
cargo run -- -f targets.txt --record
cargo run -- --record session.jsonl
```

When no record path is provided, the target file stem is included in the
generated file name, such as `targets_20260425_120000.jsonl`.
Generated record names avoid existing files by adding a numeric suffix when
needed. Explicit record paths fail if the file already exists. Parent
directories must already exist; pdeck does not create directories for record
output.

By default, record files have no size limit. Set `--record-size-limit SIZE` to
rotate to the next JSONL file once the next event would exceed that size. Plain
numbers are bytes; suffixes such as `1kb`, `100mb`, `1gb`, `100mib`, and
`1gib` are also supported. For `--record session.jsonl`, rotated parts are
named `session_part0002.jsonl`, `session_part0003.jsonl`, and so on. Existing
base files or matching rotated parts are rejected instead of overwritten.

Run live probes without the TUI:

```sh
cargo run -- -f targets.txt --no-tui
cargo run -- -f targets.txt --no-tui --record
```

Replay a recorded session:

```sh
cargo run -- replay session.jsonl
cargo run -- replay --only session_part0002.jsonl
```

New recordings use JSONL format v2. Each file starts with metadata containing a
`session_id`, `part`, `file_started_at`, and targets. When replay, stats, or log
is given a v2 file, pdeck scans the same directory for `.jsonl` files with the
same `session_id`, sorts them by `part`, and reads them as one session. Use
`--only <FILE>` to inspect a single part without discovery. Older v1 single-file
recordings remain readable.

Replay controls:

- `Ctrl+S`: pause/resume
- `1`/`2`/`5`/`0`: playback speed x1/x2/x5/x10
- `Left`/`Right`: skip backward/forward 10 seconds
- `Shift+Left`/`Shift+Right`: skip backward/forward 60 seconds

Convert a recorded session to a text log:

```sh
cargo run -- log session.jsonl
cargo run -- log session.jsonl -o replay.log
cargo run -- log --only session_part0002.jsonl
```

Convert a recorded JSONL session to per-host CSV statistics:

```sh
cargo run -- stats session.jsonl
cargo run -- stats session.jsonl -o session-stats.csv
cargo run -- stats --only session_part0002.jsonl
```

This conversion reads the full recorded session and exits without opening the
TUI. When no stats path is provided, `session.jsonl` writes `session_stats.csv`.
Replay, stats, and log conversion skip blank or malformed JSONL event lines
after the session metadata/header, so a partially damaged record can still be
used when the remaining event lines are valid.
The stats CSV includes host, last resolved IP, description, packet counts,
response/loss counts, loss percentage, RTT min/avg/max/stddev, first/last probe
times, duration, downtime totals, downtime percentage, downtime periods, and the
last observed status/response.

## Build

```sh
cargo check
cargo build
cargo build --release
cargo fmt
cargo clippy --all-targets --all-features
```

## Platform Notes

- Supported on macOS, Windows, and Linux
- Rust stable and Cargo are required to build from source
- Keep private host lists, internal endpoints, and credentials out of the repository
