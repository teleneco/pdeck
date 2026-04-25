# Changelog

## v0.2.0

- Add replay controls for speed changes and 10s/60s seeking.
- Add `stats` and `log` subcommands for converting recorded JSONL sessions.
- Add per-host CSV stats with packet counts, loss rate, RTT min/avg/max/stddev,
  duration, downtime totals, and downtime periods.
- Add `--no-tui` for live line-oriented output, with optional `--record`.
- Allow `--record` without a path, deriving the file name from `-f`.
- Split live, replay, record, stats, log, and config code out of `main.rs`.
- Add cross-platform replay fixture coverage.
- Harden CLI behavior with clippy cleanup, secure temporary files, terminal
  restore guard, and a concurrency upper bound.
- Update Cargo dependencies.

## v0.1.0

- Initial release.
