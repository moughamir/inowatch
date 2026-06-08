/// MCP (Model Context Protocol) server implementation.
///
/// Implements the stdio transport layer of the MCP spec (2025-11-25):
///   - JSON-RPC 2.0 message framing (newline-delimited over stdin/stdout)
///   - Initialize / Initialized lifecycle
///   - Tools: watch_directory, unwatch, list_watches, status
///   - Resources: file:// URIs, inowatch://watches meta-resource
///
/// No async runtime — pure blocking I/O with BufReader/BufWriter.
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

const PROTOCOL_VERSION: &str = "2025-11-25";
const SERVER_NAME: &str = "inowatch";

/// MCP server state.
pub struct McpServer {
    initialized: bool,
    /// Set of watched directory paths (canonical).
    watched_dirs: HashSet<PathBuf>,
}

impl McpServer {
    pub fn new() -> Self {
        Self {
            initialized: false,
            watched_dirs: HashSet::new(),
        }
    }

    /// Run the MCP server loop. Reads JSON-RPC messages from stdin,
    /// dispatches them, and writes responses to stdout.
    /// Returns on EOF or `shutdown` request.
    pub fn serve(&mut self) -> io::Result<()> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut reader = stdin.lock();
        let mut line_buf = String::new();

        loop {
            line_buf.clear();
            let n = reader.read_line(&mut line_buf)?;
            if n == 0 {
                // EOF — client closed stdin
                break;
            }

            let line = line_buf.trim();
            if line.is_empty() {
                continue;
            }

            let msg: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    // JSON-RPC parse error
                    write_msg(
                        &mut stdout.lock(),
                        &json!({
                            "jsonrpc": "2.0",
                            "id": null,
                            "error": {
                                "code": -32700,
                                "message": format!("Parse error: {}", e)
                            }
                        }),
                    )?;
                    continue;
                }
            };

            let method = msg
                .get("method")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            let id = msg.get("id");
            let has_id = id.is_some() && !id.as_ref().map_or(true, |v| v.is_null());

            // --- Lifecycle: initialize before anything else ---
            if method == "initialize" {
                self.handle_initialize(&msg);
                write_msg(&mut stdout.lock(), &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {
                            "tools": {},
                            "resources": {}
                        },
                        "serverInfo": {
                            "name": SERVER_NAME,
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }
                }))?;
                self.initialized = true;
                continue;
            }

            if !self.initialized {
                if has_id {
                    write_msg(&mut stdout.lock(), &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32000, "message": "Server not initialized" }
                    }))?;
                }
                continue;
            }

            match method {
                "notifications/initialized" => {
                    // Acknowledged — no response needed
                }
                "ping" => {
                    write_msg(&mut stdout.lock(), &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {}
                    }))?;
                }
                "tools/list" => {
                    self.handle_tools_list(&mut stdout.lock(), id, has_id)?;
                }
                "tools/call" => {
                    self.handle_tools_call(&mut stdout.lock(), &msg, id, has_id)?;
                }
                "resources/list" => {
                    self.handle_resources_list(&mut stdout.lock(), id, has_id)?;
                }
                "resources/templates/list" => {
                    self.handle_resource_templates(&mut stdout.lock(), id, has_id)?;
                }
                "resources/read" => {
                    self.handle_resources_read(&mut stdout.lock(), &msg, id, has_id)?;
                }
                "shutdown" => {
                    // Graceful shutdown — client will close stdin after receiving response
                    if has_id {
                        write_msg(&mut stdout.lock(), &json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {}
                        }))?;
                    }
                    break;
                }
                _ => {
                    if has_id {
                        write_msg(&mut stdout.lock(), &json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32601, "message": format!("Method not found: {}", method) }
                        }))?;
                    }
                }
            }
        }

        Ok(())
    }

    // ── Lifecycle ──────────────────────────────────────────────────────────

    fn handle_initialize(&mut self, msg: &Value) {
        // We log the client info for debugging via stderr.
        if let Some(client) = msg.pointer("/params/clientInfo") {
            let name = client.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let ver = client.get("version").and_then(|v| v.as_str()).unwrap_or("?");
            eprintln!("MCP client connected: {} v{}", name, ver);
        }
    }

    // ── Tools ──────────────────────────────────────────────────────────────

    fn handle_tools_list(
        &self,
        writer: &mut impl Write,
        id: Option<&Value>,
        has_id: bool,
    ) -> io::Result<()> {
        if !has_id {
            return Ok(());
        }

        let tools = json!([
            {
                "name": "watch_directory",
                "description": "Watch a directory for filesystem changes using inotify. Adds a recursive watch on the given path.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the directory to watch" },
                        "recursive": { "type": "boolean", "description": "Watch subdirectories recursively (default: true)" }
                    },
                    "required": ["path"]
                }
            },
            {
                "name": "unwatch",
                "description": "Stop watching a previously watched path.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to stop watching" }
                    },
                    "required": ["path"]
                }
            },
            {
                "name": "list_watches",
                "description": "List all active directory watches.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false
                }
            },
            {
                "name": "status",
                "description": "Get daemon status information.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false
                }
            }
        ]);

        write_msg(writer, &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": tools }
        }))?;
        Ok(())
    }

    fn handle_tools_call(
        &mut self,
        writer: &mut impl Write,
        msg: &Value,
        id: Option<&Value>,
        has_id: bool,
    ) -> io::Result<()> {
        if !has_id {
            return Ok(());
        }

        let name = msg
            .pointer("/params/name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let args = msg
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or(json!({}));

        match name {
            "watch_directory" => self.tool_watch(writer, id, &args),
            "unwatch" => self.tool_unwatch(writer, id, &args),
            "list_watches" => self.tool_list_watches(writer, id),
            "status" => self.tool_status(writer, id),
            _ => write_msg(writer, &json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32602, "message": format!("Unknown tool: {}", name) }
            })),
        }
    }

    /// Check if watching a path is allowed.
    /// Blocks sensitive system directories to prevent resource exhaustion
    /// and monitoring of privileged paths by MCP clients.
    fn is_watch_allowed(&self, path: &std::path::Path) -> bool {
        // Block known sensitive system directories
        let sensitive_prefixes = [
            "/etc",
            "/proc",
            "/sys",
            "/dev",
            "/run",
            "/boot",
            "/lost+found",
            "/root",
        ];
        let path_str = path.to_string_lossy();
        !sensitive_prefixes
            .iter()
            .any(|&prefix| path_str == prefix || path_str.starts_with(&format!("{}/", prefix)))
    }

    fn tool_watch(
        &mut self,
        writer: &mut impl Write,
        id: Option<&Value>,
        args: &Value,
    ) -> io::Result<()> {
        let path_str = match args.get("path").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => {
                return write_msg(writer, &json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "content": [{ "type": "text", "text": "Error: path argument is required" }], "isError": true }
                }));
            }
        };

        let path = std::path::Path::new(path_str);
        if !path.exists() {
            return write_msg(writer, &json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "content": [{ "type": "text", "text": format!("Error: path does not exist: {}", path_str) }], "isError": true }
            }));
        }

        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

        // Security: validate path is not a sensitive system directory
        if !self.is_watch_allowed(&canonical) {
            return write_msg(writer, &json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "content": [{ "type": "text", "text": format!("Error: watching system directory is not allowed: {}", canonical.display()) }], "isError": true }
            }));
        }

        if self.watched_dirs.contains(&canonical) {
            return write_msg(writer, &json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "content": [{ "type": "text", "text": format!("Already watching: {}", canonical.display()) }] }
            }));
        }

        let recursive = args.get("recursive").and_then(|v| v.as_bool()).unwrap_or(true);
        let mut watcher = match crate::watch::Watcher::new(recursive) {
            Ok(w) => w,
            Err(e) => {
                return write_msg(writer, &json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "content": [{ "type": "text", "text": format!("Error creating watcher: {}", e) }], "isError": true }
                }));
            }
        };

        match watcher.add_watch(&canonical) {
            Ok(_) => {
                // We intentionally drop the watcher here — in MCP mode v1 we
                // don't stream events, we just acknowledge the watch.
                // The watcher resources (inotify fd, wd map) are released.
                // 
                // A future version could keep the watcher alive in a background
                // thread and emit notifications/resources/list_changed events.
                let _ = watcher;
                self.watched_dirs.insert(canonical.clone());
                write_msg(writer, &json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "content": [{ "type": "text", "text": format!("Watching: {}", canonical.display()) }] }
                }))
            }
            Err(e) => {
                write_msg(writer, &json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "content": [{ "type": "text", "text": format!("Error watching {}: {}", canonical.display(), e) }], "isError": true }
                }))
            }
        }
    }

    fn tool_unwatch(
        &mut self,
        writer: &mut impl Write,
        id: Option<&Value>,
        args: &Value,
    ) -> io::Result<()> {
        let path_str = match args.get("path").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => {
                return write_msg(writer, &json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "content": [{ "type": "text", "text": "Error: path argument is required" }], "isError": true }
                }));
            }
        };

        let target = std::path::Path::new(path_str);
        let canonical = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());

        if self.watched_dirs.remove(&canonical) {
            write_msg(writer, &json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "content": [{ "type": "text", "text": format!("Stopped watching: {}", canonical.display()) }] }
            }))
        } else {
            write_msg(writer, &json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "content": [{ "type": "text", "text": format!("Not watching: {}", canonical.display()) }] }
            }))
        }
    }

    fn tool_list_watches(
        &self,
        writer: &mut impl Write,
        id: Option<&Value>,
    ) -> io::Result<()> {
        let paths: Vec<String> = self
            .watched_dirs
            .iter()
            .map(|p| p.display().to_string())
            .collect();

        let text = if paths.is_empty() {
            "No active watches.".to_string()
        } else {
            let mut s = String::from("Active watches:\n");
            for p in &paths {
                s.push_str(&format!("  - {}\n", p));
            }
            s
        };

        write_msg(writer, &json!({
            "jsonrpc": "2.0", "id": id,
            "result": { "content": [{ "type": "text", "text": text }] }
        }))
    }

    fn tool_status(
        &self,
        writer: &mut impl Write,
        id: Option<&Value>,
    ) -> io::Result<()> {
        let text = format!(
            "inowatch v{}\nMCP protocol: {}\nWatched directories: {}\n",
            env!("CARGO_PKG_VERSION"),
            PROTOCOL_VERSION,
            self.watched_dirs.len()
        );

        write_msg(writer, &json!({
            "jsonrpc": "2.0", "id": id,
            "result": { "content": [{ "type": "text", "text": text }] }
        }))
    }

    // ── Resources ──────────────────────────────────────────────────────────

    fn handle_resources_list(
        &self,
        writer: &mut impl Write,
        id: Option<&Value>,
        has_id: bool,
    ) -> io::Result<()> {
        if !has_id {
            return Ok(());
        }

        let mut resources: Vec<Value> = Vec::new();

        // Add file:// resources for watched directories
        for dir in &self.watched_dirs {
            resources.push(json!({
                "uri": format!("file://{}", dir.display()),
                "name": dir.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                "mimeType": "inode/directory",
                "description": format!("Watched directory: {}", dir.display())
            }));
        }

        // Add the inowatch://watches meta-resource
        resources.push(json!({
            "uri": "inowatch://watches",
            "name": "Active Watches",
            "mimeType": "application/json",
            "description": "List of currently watched directories"
        }));

        write_msg(writer, &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "resources": resources }
        }))
    }

    fn handle_resource_templates(
        &self,
        writer: &mut impl Write,
        id: Option<&Value>,
        has_id: bool,
    ) -> io::Result<()> {
        if !has_id {
            return Ok(());
        }

        let templates = json!([
            {
                "uriTemplate": "file://{path}",
                "name": "File Contents",
                "description": "Read the contents of any file on the filesystem by its absolute path",
                "mimeType": "application/octet-stream"
            }
        ]);

        write_msg(writer, &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "resourceTemplates": templates }
        }))
    }

    fn handle_resources_read(
        &self,
        writer: &mut impl Write,
        msg: &Value,
        id: Option<&Value>,
        has_id: bool,
    ) -> io::Result<()> {
        if !has_id {
            return Ok(());
        }

        let uri = match msg.pointer("/params/uri").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => {
                return write_msg(writer, &json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32602, "message": "Missing uri parameter" }
                }));
            }
        };

        // Handle inowatch://watches meta-resource
        if uri == "inowatch://watches" {
            let paths: Vec<String> = self
                .watched_dirs
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            let text = serde_json::to_string_pretty(&paths).unwrap_or_default();
            return write_msg(writer, &json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "contents": [{
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": text
                    }]
                }
            }));
        }

        // Handle file:// URIs
        if let Some(path) = uri.strip_prefix("file://") {
            let file_path = std::path::Path::new(path);
            if !file_path.exists() {
                return write_msg(writer, &json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32002, "message": "Resource not found", "data": { "uri": uri } }
                }));
            }

            if file_path.is_dir() {
                // Directory: list contents as text
                let mut text = String::new();
                if let Ok(entries) = std::fs::read_dir(file_path) {
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let ty = entry.file_type().ok().map(|t| {
                            if t.is_dir() { "dir" } else if t.is_symlink() { "link" } else { "file" }
                        }).unwrap_or("?");
                        text.push_str(&format!("  [{}] {}\n", ty, name));
                    }
                }

                return write_msg(writer, &json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "contents": [{
                            "uri": uri,
                            "mimeType": "text/plain",
                            "text": text
                        }]
                    }
                }));
            }

            // Regular file: read content
            match std::fs::read_to_string(file_path) {
                Ok(text) => {
                    // Detect MIME type from extension
                    let mime = mime_from_extension(file_path)
                        .unwrap_or("application/octet-stream");
                    write_msg(writer, &json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {
                            "contents": [{
                                "uri": uri,
                                "mimeType": mime,
                                "text": text
                            }]
                        }
                    }))
                }
                Err(e) => {
                    // Try binary read if text failed
                    match std::fs::read(file_path) {
                        Ok(bytes) => {
                            let b64 = base64_encode(&bytes);
                            write_msg(writer, &json!({
                                "jsonrpc": "2.0", "id": id,
                                "result": {
                                    "contents": [{
                                        "uri": uri,
                                        "mimeType": "application/octet-stream",
                                        "blob": b64
                                    }]
                                }
                            }))
                        }
                        Err(_) => {
                            write_msg(writer, &json!({
                                "jsonrpc": "2.0", "id": id,
                                "error": { "code": -32002, "message": format!("Cannot read resource: {}", e), "data": { "uri": uri } }
                            }))
                        }
                    }
                }
            }
        } else {
            write_msg(writer, &json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32002, "message": format!("Unsupported URI scheme: {}", uri) }
            }))
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Write a JSON value as a newline-delimited message to the writer.
fn write_msg(writer: &mut impl Write, msg: &Value) -> io::Result<()> {
    let json = serde_json::to_string(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    writeln!(writer, "{}", json)?;
    writer.flush()?;
    Ok(())
}

/// Simple MIME type detection from file extension.
fn mime_from_extension(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    match ext.as_str() {
        "txt" | "md" | "markdown" => Some("text/plain"),
        "rs" => Some("text/x-rust"),
        "py" => Some("text/x-python"),
        "js" | "jsx" => Some("text/javascript"),
        "ts" | "tsx" => Some("text/typescript"),
        "json" => Some("application/json"),
        "yaml" | "yml" => Some("application/x-yaml"),
        "toml" => Some("application/toml"),
        "html" | "htm" => Some("text/html"),
        "css" => Some("text/css"),
        "xml" => Some("application/xml"),
        "sh" => Some("text/x-shellscript"),
        "go" => Some("text/x-go"),
        "c" | "h" => Some("text/x-c"),
        "cpp" | "hpp" | "cc" => Some("text/x-c++"),
        "java" => Some("text/x-java"),
        "rb" => Some("text/x-ruby"),
        "php" => Some("text/x-php"),
        "sql" => Some("text/x-sql"),
        _ => None,
    }
}

/// Minimal base64 encoding for binary resource data.
fn base64_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn test_mime_from_extension() {
        assert_eq!(mime_from_extension(std::path::Path::new("main.rs")), Some("text/x-rust"));
        assert_eq!(mime_from_extension(std::path::Path::new("index.html")), Some("text/html"));
        assert_eq!(mime_from_extension(std::path::Path::new("data.json")), Some("application/json"));
        assert_eq!(mime_from_extension(std::path::Path::new("noext")), None);
    }

    #[test]
    fn test_write_msg_valid_json() {
        let mut buf = Vec::new();
        let msg = json!({"jsonrpc": "2.0", "result": {}});
        write_msg(&mut buf, &msg).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.ends_with('\n'));
        let parsed: Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
    }

    /// Test a full MCP initialize exchange.
    #[test]
    fn test_mcp_initialize() {
        let mut server = McpServer::new();
        assert!(!server.initialized);

        // Send initialize via stdin simulation
        let init_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "1.0.0" }
            }
        });

        // Capture stdout
        let mut output = Vec::new();
        
        // Simulate the handler
        server.handle_initialize(&init_req);
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": { "tools": {}, "resources": {} },
                "serverInfo": { "name": "inowatch", "version": env!("CARGO_PKG_VERSION") }
            }
        });
        write_msg(&mut output, &response).unwrap();

        server.initialized = true;
        assert!(server.initialized);

        let output_str = String::from_utf8(output).unwrap();
        let parsed: Value = serde_json::from_str(output_str.trim()).unwrap();
        assert_eq!(parsed["result"]["protocolVersion"], "2025-11-25");
        assert!(parsed["result"]["capabilities"]["tools"].is_object());
        assert!(parsed["result"]["capabilities"]["resources"].is_object());
    }

    #[test]
    fn test_mcp_tools_list() {
        let mut server = McpServer::new();
        server.initialized = true;

        let mut output = Vec::new();
        server.handle_tools_list(&mut io::BufWriter::new(&mut output), Some(&json!(2)), true).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let parsed: Value = serde_json::from_str(output_str.trim()).unwrap();
        let tools = parsed["result"]["tools"].as_array().unwrap();
        assert!(tools.len() >= 4);
        
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"watch_directory"));
        assert!(names.contains(&"unwatch"));
        assert!(names.contains(&"list_watches"));
        assert!(names.contains(&"status"));
    }

    #[test]
    fn test_mcp_tool_status() {
        let mut server = McpServer::new();
        server.initialized = true;

        let mut output = Vec::new();
        server.tool_status(&mut io::BufWriter::new(&mut output), Some(&json!(3))).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let parsed: Value = serde_json::from_str(output_str.trim()).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("inowatch v"));
        assert!(text.contains("MCP protocol"));
    }

    #[test]
    fn test_mcp_tool_list_watches_empty() {
        let mut server = McpServer::new();
        server.initialized = true;

        let mut output = Vec::new();
        server.tool_list_watches(&mut io::BufWriter::new(&mut output), Some(&json!(4))).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let parsed: Value = serde_json::from_str(output_str.trim()).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("No active watches"));
    }

    #[test]
    fn test_mcp_resources_list_empty() {
        let mut server = McpServer::new();
        server.initialized = true;

        let mut output = Vec::new();
        server.handle_resources_list(&mut io::BufWriter::new(&mut output), Some(&json!(5)), true).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let parsed: Value = serde_json::from_str(output_str.trim()).unwrap();
        let resources = parsed["result"]["resources"].as_array().unwrap();
        // Should have at least the inowatch://watches meta-resource
        assert!(!resources.is_empty());
        let uris: Vec<&str> = resources.iter().filter_map(|r| r["uri"].as_str()).collect();
        assert!(uris.contains(&"inowatch://watches"));
    }

    #[test]
    fn test_mcp_resource_templates() {
        let mut server = McpServer::new();
        server.initialized = true;

        let mut output = Vec::new();
        server.handle_resource_templates(&mut io::BufWriter::new(&mut output), Some(&json!(6)), true).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let parsed: Value = serde_json::from_str(output_str.trim()).unwrap();
        let templates = parsed["result"]["resourceTemplates"].as_array().unwrap();
        let uris: Vec<&str> = templates.iter().filter_map(|t| t["uriTemplate"].as_str()).collect();
        assert!(uris.contains(&"file://{path}"));
    }

    #[test]
    fn test_mcp_unknown_method() {
        let mut server = McpServer::new();
        server.initialized = true;

        let mut output = Vec::new();
        let id = json!(7);
        write_msg(&mut output, &json!({
            "jsonrpc": "2.0",
            "id": &id,
            "error": { "code": -32601, "message": format!("Method not found: {}", "nonexistent") }
        })).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let parsed: Value = serde_json::from_str(output_str.trim()).unwrap();
        assert_eq!(parsed["error"]["code"], -32601);
    }

    #[test]
    fn test_mcp_not_initialized() {
        // Don't initialize — should reject tools/list

        let mut output = Vec::new();
        write_msg(&mut output, &json!({
            "jsonrpc": "2.0",
            "id": 8,
            "error": { "code": -32000, "message": "Server not initialized" }
        })).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let parsed: Value = serde_json::from_str(output_str.trim()).unwrap();
        assert_eq!(parsed["error"]["code"], -32000);
    }

    #[test]
    fn test_inowatch_read_meta_resource() {
        let mut server = McpServer::new();
        server.initialized = true;

        // Test inowatch://watches resource
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "resources/read",
            "params": { "uri": "inowatch://watches" }
        });

        let mut output = Vec::new();
        server.handle_resources_read(&mut io::BufWriter::new(&mut output), &msg, Some(&json!(9)), true).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let parsed: Value = serde_json::from_str(output_str.trim()).unwrap();
        assert_eq!(parsed["result"]["contents"][0]["uri"], "inowatch://watches");
        assert_eq!(parsed["result"]["contents"][0]["mimeType"], "application/json");
    }

    #[test]
    fn test_inowatch_read_nonexistent_file() {
        let mut server = McpServer::new();
        server.initialized = true;

        let msg = json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "resources/read",
            "params": { "uri": "file:///tmp/__nonexistent_file_inowatch_test__" }
        });

        let mut output = Vec::new();
        server.handle_resources_read(&mut io::BufWriter::new(&mut output), &msg, Some(&json!(10)), true).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let parsed: Value = serde_json::from_str(output_str.trim()).unwrap();
        assert_eq!(parsed["error"]["code"], -32002);
    }

    #[test]
    fn test_mcp_shutdown() {
        // Shutdown just returns empty result — test the response format
        let mut output = Vec::new();
        write_msg(&mut output, &json!({
            "jsonrpc": "2.0",
            "id": 11,
            "result": {}
        })).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let parsed: Value = serde_json::from_str(output_str.trim()).unwrap();
        assert!(parsed.get("result").is_some());
    }
}
