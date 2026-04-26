# pdeck

[English](./README.md) | [日本語](./README.ja.md)

`pdeck` は、複数ターゲットのネットワーク到達性をひとつの画面で監視するためのターミナル probe deck です。

1 つの target file に ICMP、TCP connect、HTTP/HTTPS を混在させて、直近の状態、packet loss、dead host、RTT history を TUI で確認できます。

リリース履歴は [CHANGELOG.md](./CHANGELOG.md) を参照してください。

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
-c, --concurrency <N>     Maximum concurrent TCP/HTTP probes
--icmp-backend <BACKEND>  auto, exec, or api
--record [FILE]           Write JSONL session events
--record-size-limit SIZE  この size を超える前に record file を rotate する。default 0
--no-tui                  TUI を開かず live probe 結果を出力する
```

記録済み session は subcommand で扱います:

```sh
pdeck replay <FILE>
pdeck replay --only <FILE>
pdeck stats <FILE> [-o FILE]
pdeck stats --only <FILE> [-o FILE]
pdeck log <FILE> [-o FILE]
pdeck log --only <FILE> [-o FILE]
```

ICMP backend のデフォルト:

- macOS: `exec`
- Windows: `api`
- Linux: `exec`

`exec` backend は `ping` 実行時に `LC_ALL=C` と `LANG=C` を指定して、ロケール依存の出力差分を減らしています。
現在の Windows `api` backend は内部的に IPv4 ICMP API path を使っています。
IPv6 ICMP を運用で使う場合は、Windows 上で対象 host の挙動を確認してから使ってください。

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
cargo run -- -f targets.txt --record
cargo run -- --record session.jsonl
```

record path を省略した場合は、`-f` のファイル名を含めた
`targets_20260425_120000.jsonl` のような名前で生成します。
生成名が既存ファイルと衝突する場合は numeric suffix を付けて回避します。
明示した record path が既に存在する場合はエラーにします。親 directory は事前に
存在している必要があります。pdeck は record output 用の directory を自動作成しません。

record file size limit はデフォルト 0、つまり無制限です。
`--record-size-limit SIZE` を指定すると、次の event で上限を超える前に
次の JSONL file へ rotate します。数字だけなら byte として扱い、`1kb`、
`100mb`、`1gb`、`100mib`、`1gib` のような suffix も使えます。
`--record session.jsonl` の場合、rotation 後の file は
`session_part0002.jsonl`、`session_part0003.jsonl` のように作成します。
既存の base file や matching rotated part は上書きせずエラーにします。

TUI を開かずに live probe:

```sh
cargo run -- -f targets.txt --no-tui
cargo run -- -f targets.txt --no-tui --record
```

記録した session を replay:

```sh
cargo run -- replay session.jsonl
cargo run -- replay --only session_part0002.jsonl
```

新規 record は JSONL format v2 を使います。各 file の先頭には
`session_id`、`part`、`file_started_at`、targets を含む metadata が入ります。
replay、stats、log に v2 file を渡すと、pdeck は同じ directory の `.jsonl`
から同じ `session_id` の file を探し、`part` 順で 1 session として読み込みます。
単一 part だけを確認したい場合は `--only <FILE>` を使います。従来の v1
single-file record も引き続き読み込めます。

replay 中の操作:

- `Ctrl+S`: 一時停止/再開
- `1`/`2`/`5`/`0`: 再生速度 x1/x2/x5/x10
- `Left`/`Right`: 10 秒戻す/進める
- `Shift+Left`/`Shift+Right`: 60 秒戻す/進める

記録した session を text log に変換:

```sh
cargo run -- log session.jsonl
cargo run -- log session.jsonl -o replay.log
cargo run -- log --only session_part0002.jsonl
```

記録した JSONL session をホスト単位の CSV 統計へ変換:

```sh
cargo run -- stats session.jsonl
cargo run -- stats session.jsonl -o session-stats.csv
cargo run -- stats --only session_part0002.jsonl
```

この変換は記録ファイル全体を読み込み、TUI を開かずに終了します。
stats path を省略した場合、`session.jsonl` から `session_stats.csv` を出力します。
replay、stats、log conversion は session metadata/header 後の空行や壊れた
JSONL event 行を skip するため、部分的に壊れた record でも残りの有効な event を
利用できます。
stats CSV には host、最後に解決した IP、description、パケット数、
応答数/ロス数、ロス率、RTT min/avg/max/stddev、最初/最後の probe 時刻、
duration、ダウンタイム合計、ダウン率、ダウン区間、最後の status/response を出力します。

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
