# inowatch — File Watch Daemon

[![AGPL-3.0-only]](https://spdx.org/licenses/AGPL-3.0-only.html)
[![GitHub]](https://github.com/moughamir/inowatch)
[![zread](https://img.shields.io/badge/Ask_Zread-_.svg?style=for-the-badge&color=00b0aa&labelColor=000000&logo=data%3Aimage%2Fsvg%2Bxml%3Bbase64%2CPHN2ZyB3aWR0aD0iMTYiIGhlaWdodD0iMTYiIHZpZXdCb3g9IjAgMCAxNiAxNiIgZmlsbD0ibm9uZSIgeG1sbnM9Imh0dHA6Ly93d3cudzMub3JnLzIwMDAvc3ZnIj4KPHBhdGggZD0iTTQuOTYxNTYgMS42MDAxSDIuMjQxNTZDMS44ODgxIDEuNjAwMSAxLjYwMTU2IDEuODg2NjQgMS42MDE1NiAyLjI0MDFWNC45NjAxQzEuNjAxNTYgNS4zMTM1NiAxLjg4ODEgNS42MDAxIDIuMjQxNTYgNS42MDAxSDQuOTYxNTZDNS4zMTUwMiA1LjYwMDEgNS42MDE1NiA1LjMxMzU2IDUuNjAxNTYgNC45NjAxVjIuMjQwMUM1LjYwMTU2IDEuODg2NjQgNS4zMTUwMiAxLjYwMDEgNC45NjE1NiAxLjYwMDFaIiBmaWxsPSIjZmZmIi8%2BCjxwYXRoIGQ9Ik00Ljk2MTU2IDEwLjM5OTlIMi4yNDE1NkMxLjg4ODEgMTAuMzk5OSAxLjYwMTU2IDEwLjY4NjQgMS42MDE1NiAxMS4wMzk5VjEzLjc1OTlDMS42MDE1NiAxNC4xMTM0IDEuODg4MSAxNC4zOTk5IDIuMjQxNTYgMTQuMzk5OUg0Ljk2MTU2QzUuMzE1MDIgMTQuMzk5OSA1LjYwMTU2IDE0LjExMzQgNS42MDE1NiAxMy43NTk5VjExLjAzOTlDNS42MDE1NiAxMC42ODY0IDUuMzE1MDIgMTAuMzk5OSA0Ljk2MTU2IDEwLjM5OTlaIiBmaWxsPSIjZmZmIi8%2BCjxwYXRoIGQ9Ik0xMy43NTg0IDEuNjAwMUgxMS4wMzg0QzEwLjY4NSAxLjYwMDEgMTAuMzk4NCAxLjg4NjY0IDEwLjM5ODQgMi4yNDAxVjQuOTYwMUMxMC4zOTg0IDUuMzEzNTYgMTAuNjg1IDUuNjAwMSAxMS4wMzg0IDUuNjAwMUgxMy43NTg0QzE0LjExMTkgNS42MDAxIDE0LjM5ODQgNS4zMTM1NiAxNC4zOTg0IDQuOTYwMVYyLjI0MDFDMTQuMzk4NCAxLjg4NjY0IDE0LjExMTkgMS42MDAxIDEzLjc1ODQgMS42MDAxWiIgZmlsbD0iI2ZmZiIvPgo8cGF0aCBkPSJNNCAxMkwxMiA0TDQgMTJaIiBmaWxsPSIjZmZmIi8%2BCjxwYXRoIGQ9Ik00IDEyTDEyIDQiIHN0cm9rZT0iI2ZmZiIgc3Ryb2tlLXdpZHRoPSIxLjUiIHN0cm9rZS1saW5lY2FwPSJyb3VuZCIvPgo8L3N2Zz4K&logoColor=ffffff)](https://zread.ai/moughamir/inowatch)

Watches filesystem changes via **inotify** and emits structured events to stdout as **NDJSON** (newline-delimited JSON). Also ships a **Model Context Protocol** mode for JSON-RPC 2.0 integration.

---

## Installation

```console
$ cargo install --git https://github.com/moughamir/inowatch
```

Or build from source:

```console
$ git clone https://github.com/moughamir/inowatch
$ cd inowatch
$ cargo build --release
```

---

## Usage

```console
$ inowatch [OPTIONS] <PATH>...
```

Watch one or more paths and print coalesced events to stdout:

```console
$ inowatch /var/log
{"timestamp":"2026-06-08T12:00:00.000000000Z","seq":1,"events":[{"type":"create","path":"/var/log/nginx/access.log","info":{"size":0,"mode":"0644","is_dir":false}}]}
```

### Options

| Flag | Description |
|------|-------------|
| `-d` | Fork into background (daemon mode). |
| `-p <FILE>` | Write PID to file (implies `-d`). |
| `--debounce <MS>` | Coalescing window in milliseconds (default: `500`). |
| `--no-recursive` | Do not watch subdirectories recursively. |
| `-q` | Suppress the startup banner record. |
| `--mcp` | Run in MCP mode (see below). |

All paths are watched recursively by default.

### MCP Mode

`--mcp` puts fwd in **Model Context Protocol** mode — it speaks JSON-RPC 2.0 over stdin/stdout, exposing filesystem events as MCP resources. This mode is incompatible with `-d`, `-p`, and `-q`.

```console
$ inowatch --mcp
```

### Output Format

Standard mode emits one NDJSON line per batch of coalesced events:

```json
{
  "timestamp": "2026-06-08T12:00:00.000000000Z",
  "seq": 1,
  "events": [
    {
      "type": "create|modify|delete|rename",
      "path": "/some/file",
      "cookie": null,
      "info": { "size": 1024, "mode": "0644", "is_dir": false }
    }
  ]
}
```

The first line may be a `"type":"banner"` record with the daemon version, PID, watched paths, and debounce setting.

---

## License

AGPL-3.0-only. See [LICENSE](LICENSE).

<!-- Badge URLs -->
[AGPL-3.0-only]: https://img.shields.io/badge/license-AGPL--3.0--only-blue.svg
[GitHub]: https://img.shields.io/badge/repo-github-181717?logo=github
