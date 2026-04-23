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
--record <FILE>           Write JSONL session events
--replay <FILE>           Replay JSONL session events
--log <FILE>              Write human-readable text log
```

ICMP backend defaults:

- macOS: `exec`
- Windows: `api`
- Linux: `exec`

The `exec` backend runs `ping` with `LC_ALL=C` and `LANG=C` to reduce locale-dependent output differences.

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
cargo run -- --record session.jsonl
```

Replay a recorded session:

```sh
cargo run -- --replay session.jsonl
```

Replay and also write a text log:

```sh
cargo run -- --replay session.jsonl --log replay.log
```

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
