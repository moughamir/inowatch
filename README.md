# inowatch — File Watch Daemon

[![AGPL-3.0-only]](https://spdx.org/licenses/AGPL-3.0-only.html)
[![GitHub]](https://github.com/moughamir/inowatch)

Watches filesystem changes via **inotify** and emits structured events to stdout as **NDJSON** (newline-delimited JSON). Also ships a **Model Context Protocol** mode for JSON-RPC 2.0 integration.

> **TODO:** Windows support is planned for a future release. The daemon will use the `notify` crate (`ReadDirectoryChangesW` backend) to provide equivalent filesystem watching on Windows. See `WINDOWS_SUPPORT_PLAN.md` for details.

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
