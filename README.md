# pdeck

[English](./README.md) | [日本語](./README.ja.md)

`pdeck` is a terminal probe deck for watching network reachability across multiple targets from one screen.

It can mix ICMP, TCP connect, and HTTP/HTTPS checks in a single target file, then show recent status, packet loss, dead hosts, and RTT history in a TUI.

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

Open an editor and type targets into a temporary file:

```sh
cargo run -- -V
```

If `EDITOR` is not set, `pdeck` uses `vi`.

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
-V                        Open an editor for a temporary target file
-c, --concurrency <N>     Maximum concurrent TCP/HTTP probes
--icmp-backend <BACKEND>  auto, exec, or api
--record [FILE]           Write JSONL session events
```

Recorded sessions are handled with subcommands:

```sh
pdeck replay <FILE>
pdeck stats <FILE> [-o FILE]
pdeck log <FILE> [-o FILE]
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

Replay a recorded session:

```sh
cargo run -- replay session.jsonl
```

Replay controls:

- `Ctrl+S`: pause/resume
- `1`/`2`/`5`/`0`: playback speed x1/x2/x5/x10
- `Left`/`Right`: skip backward/forward 10 seconds
- `Shift+Left`/`Shift+Right`: skip backward/forward 60 seconds

Convert a recorded session to a text log:

```sh
cargo run -- log session.jsonl
cargo run -- log session.jsonl -o replay.log
```

Convert a recorded JSONL session to per-host CSV statistics:

```sh
cargo run -- stats session.jsonl
cargo run -- stats session.jsonl -o session-stats.csv
```

This conversion reads the full recorded session and exits without opening the
TUI. When no stats path is provided, `session.jsonl` writes `session_stats.csv`.
The stats CSV includes host, last resolved IP, description, packet counts,
response/loss counts, loss percentage, RTT min/avg/max/stddev, first/last probe
times, duration, downtime totals, downtime percentage, downtime periods, and the
last observed status/response.

## Roadmap

- Add record rotation for long-running sessions, with a default target around
  100MB per JSONL file.
- Extend stats conversion to read multiple rotated JSONL files or a record
  directory as one logical session.

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
