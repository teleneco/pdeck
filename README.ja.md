# pdeck

[English](./README.md) | [日本語](./README.ja.md)

`pdeck` は、複数ターゲットのネットワーク到達性をひとつの画面で監視するためのターミナル probe deck です。

1 つの target file に ICMP、TCP connect、HTTP/HTTPS を混在させて、直近の状態、packet loss、dead host、RTT history を TUI で確認できます。

## 特徴

- ICMP、TCP、HTTP、HTTPS の target を 1 つのファイルに混在できる
- 複数 host の状態をターミナル UI で継続監視できる
- dead host をすばやくたどれる
- probe session を JSONL で記録できる
- 記録した session をあとで replay できる

## クイックスタート

カレントディレクトリの `targets.txt` をそのまま使って起動:

```sh
cargo run
```

別の target file を使って起動:

```sh
cargo run -- -f targets-mixed.txt
```

一時ファイルをエディタで開いて target を入力して起動:

```sh
cargo run -- -V
```

`EDITOR` が未設定の場合は `vi` を使います。

## Target File Format

空行を除く各行は次の形式です。

```text
target description
```

例:

```text
8.8.8.8 google-dns
1.1.1.1 cloudflare
tcp://1.1.1.1:53 cloudflare-dns-tcp
https://example.com example-web
http://example.com:8080 local-http
```

判定ルール:

- `host`: ICMP
- `tcp://host:port`: TCP connect
- `host:port`: TCP connect
- `http://host[:port]`: HTTP
- `https://host[:port]`: HTTPS

混在例は `targets-mixed.txt` を参照してください。

macOS の `arp -a` 出力をそのまま読みたい場合は `-A` を使います。

## 主なオプション

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

ICMP backend のデフォルト:

- macOS: `exec`
- Windows: `api`
- Linux: `exec`

`exec` backend は `ping` 実行時に `LC_ALL=C` と `LANG=C` を指定して、ロケール依存の出力差分を減らしています。

## 操作

```text
Up/Down      Select host
d / D        Jump to next/previous dead host
Ctrl+S       Pause/resume
q / Esc      Quit
Ctrl+C       Quit
```

## Record / Replay

live session を記録:

```sh
cargo run -- --record session.jsonl
```

記録した session を replay:

```sh
cargo run -- --replay session.jsonl
```

replay しながら text log も出力:

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

## 補足

- macOS、Windows、Linux をサポート
- source build には Rust stable と Cargo が必要
- private host list、internal endpoint、credential は repository に含めないでください
