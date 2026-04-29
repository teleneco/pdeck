# Changelog
## v0.3.0

- Add optional live-mode recording defaults from `~/.config/pdeck/config.toml`.
- Add `--no-record` to disable config-enabled recording for one run.
- Add `pdeck config set`, `show`, and `verify` for config management.
- Remove hidden legacy replay, stats, and log options in favor of subcommands.

## v0.2.6

- Default probe interval increased to 1s for improved stability.
- Improved ICMP exec scheduling to reduce timeouts under load.

## v0.2.5

- Add JSONL format v2 recordings with per-file metadata, `session_id`, and
  rotated parts.
- Rotate record output with `--record-size-limit SIZE` instead of stopping
  event writes at the limit, with byte values and suffixes such as `100mb` and
  `100mib`.
- Read v2 rotated sessions as one logical session for replay, stats, and log
  conversion by discovering matching `.jsonl` files in the same directory.
- Add `--only <FILE>` to replay, stats, and log for inspecting a single v2 part.
- Reject existing record base files or matching rotated parts instead of
  overwriting output.

## v0.2.4

- Remove the unused `-V`/`EDITOR` target-entry flow.
- Harden ICMP exec target validation against whitespace and shell
  metacharacters.
- Create generated record files with atomic `create_new` collision retries and
  refuse symlink targets when opening record files.
- Add stricter replay header errors and additional Windows ICMP reply checks.
- Make replay, stats, and log conversion skip blank or malformed JSONL event
  lines after a valid session header.
- Prevent record files from replacing existing paths by default, add
  `--record-overwrite` for explicit replacement, and avoid collisions for
  generated record names.
- Add `--record-size-limit BYTES` to stop writing record events after the
  configured file size is reached while live probing continues.

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
