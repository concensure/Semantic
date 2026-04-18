use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustAnalyzerSymbol {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(default)]
    pub container_name: Option<String>,
}

pub fn is_available() -> bool {
    let Some(binary) = resolve_rust_analyzer_binary() else {
        return false;
    };
    Command::new(binary)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn workspace_symbol_search(
    repo_root: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<RustAnalyzerSymbol>> {
    if !is_available() {
        return Err(anyhow!("rust-analyzer is not available on PATH"));
    }
    let mut client = RustAnalyzerClient::start(repo_root)?;
    client.initialize()?;
    let response = client.request("workspace/symbol", json!({ "query": query }))?;
    let mut symbols = parse_workspace_symbols(repo_root, response);
    symbols.truncate(limit.max(1));
    client.shutdown()?;
    Ok(symbols)
}

pub fn document_symbol_search(
    repo_root: &Path,
    relative_file: &str,
) -> Result<Vec<RustAnalyzerSymbol>> {
    if !is_available() {
        return Err(anyhow!("rust-analyzer is not available on PATH"));
    }
    let absolute = repo_root.join(relative_file);
    let text = std::fs::read_to_string(&absolute)?;
    let uri = path_to_file_uri(&absolute);
    let mut client = RustAnalyzerClient::start(repo_root)?;
    client.initialize()?;
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "rust",
                "version": 1,
                "text": text,
            }
        }),
    )?;
    let response = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    )?;
    let symbols = parse_workspace_symbols(repo_root, response);
    client.shutdown()?;
    Ok(symbols)
}

struct RustAnalyzerClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    repo_root: PathBuf,
}

impl RustAnalyzerClient {
    fn start(repo_root: &Path) -> Result<Self> {
        let binary = resolve_rust_analyzer_binary()
            .ok_or_else(|| anyhow!("failed to locate rust-analyzer binary"))?;
        let mut child = Command::new(binary)
            .current_dir(repo_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to open rust-analyzer stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("failed to open rust-analyzer stdout"))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            repo_root: repo_root.to_path_buf(),
        })
    }

    fn initialize(&mut self) -> Result<()> {
        let root_uri = path_to_file_uri(&self.repo_root);
        let workspace_name = self
            .repo_root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workspace")
            .to_string();
        let _ = self.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {},
                "clientInfo": {"name": "semantic", "version": env!("CARGO_PKG_VERSION")},
                "workspaceFolders": [{"uri": root_uri, "name": workspace_name}],
                "initializationOptions": {
                    "cargo": {"buildScripts": {"enable": true}},
                    "procMacro": {"enable": true}
                }
            }),
        )?;
        self.notify("initialized", json!({}))?;
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        let _ = self.request("shutdown", Value::Null);
        let _ = self.notify("exit", Value::Null);
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        loop {
            let message = self.read_message()?;
            if let Some(response_id) = message.get("id").and_then(|value| value.as_u64()) {
                if response_id != id {
                    continue;
                }
                if let Some(error) = message.get("error") {
                    return Err(anyhow!("rust-analyzer {method} failed: {error}"));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn write_message(&mut self, payload: &Value) -> Result<()> {
        let body = payload.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read_message(&mut self) -> Result<Value> {
        let mut content_length = None::<usize>;
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line)?;
            if read == 0 {
                return Err(anyhow!("rust-analyzer closed stdout unexpectedly"));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                content_length = value.trim().parse::<usize>().ok();
            }
        }
        let length = content_length.ok_or_else(|| anyhow!("missing Content-Length header"))?;
        let mut body = vec![0u8; length];
        self.stdout.read_exact(&mut body)?;
        Ok(serde_json::from_slice(&body)?)
    }
}

fn resolve_rust_analyzer_binary() -> Option<PathBuf> {
    let output = Command::new("rustup")
        .args(["which", "rust-analyzer"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    Some(PathBuf::from("rust-analyzer"))
}

fn parse_workspace_symbols(repo_root: &Path, response: Value) -> Vec<RustAnalyzerSymbol> {
    response
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| parse_workspace_symbol_item(repo_root, item))
        .collect()
}

fn parse_workspace_symbol_item(repo_root: &Path, item: &Value) -> Option<RustAnalyzerSymbol> {
    let location = item.get("location")?;
    let uri = location.get("uri")?.as_str()?;
    let file = uri_to_repo_relative_path(repo_root, uri)?;
    let range = location.get("range")?;
    let start_line = range.get("start")?.get("line")?.as_u64()? as u32 + 1;
    let end_line = range.get("end")?.get("line")?.as_u64()? as u32 + 1;
    Some(RustAnalyzerSymbol {
        name: item.get("name")?.as_str()?.to_string(),
        kind: workspace_symbol_kind_name(item.get("kind").and_then(|value| value.as_u64())),
        file,
        start_line,
        end_line: end_line.max(start_line),
        container_name: item
            .get("containerName")
            .and_then(|value| value.as_str())
            .map(ToString::to_string),
    })
}

fn workspace_symbol_kind_name(kind: Option<u64>) -> String {
    match kind.unwrap_or_default() {
        2 => "module",
        5 => "class",
        6 => "method",
        10 => "enum",
        11 => "interface",
        12 => "function",
        23 => "struct",
        _ => "symbol",
    }
    .to_string()
}

fn path_to_file_uri(path: &Path) -> String {
    let mut normalized = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
        .replace(' ', "%20");
    if let Some(stripped) = normalized.strip_prefix("//?/") {
        normalized = stripped.to_string();
    }
    if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    }
}

fn uri_to_repo_relative_path(repo_root: &Path, uri: &str) -> Option<String> {
    let path = uri.strip_prefix("file://")?;
    let decoded = urlencoding::decode(path).ok()?.into_owned();
    #[cfg(windows)]
    let decoded = if decoded.starts_with('/') && decoded.chars().nth(2) == Some(':') {
        decoded[1..].to_string()
    } else {
        decoded
    };
    #[cfg(windows)]
    let absolute = PathBuf::from(decoded.replace('/', "\\"));
    #[cfg(not(windows))]
    let absolute = PathBuf::from(decoded);

    let repo_root = repo_root.canonicalize().unwrap_or_else(|_| repo_root.to_path_buf());
    let absolute = absolute.canonicalize().unwrap_or(absolute);
    let repo_norm = repo_root.to_string_lossy().replace('\\', "/");
    let absolute_norm = absolute.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let (repo_norm, absolute_norm) = (repo_norm.to_ascii_lowercase(), absolute_norm.to_ascii_lowercase());
    let prefix = format!("{}/", repo_norm.trim_end_matches('/'));
    if absolute_norm == repo_norm {
        return Some(String::new());
    }
    absolute_norm
        .strip_prefix(&prefix)
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::{path_to_file_uri, uri_to_repo_relative_path};
    use std::path::Path;

    #[test]
    fn converts_file_uri_back_to_relative_path() {
        let repo = Path::new("C:/repo");
        let uri = "file:///C:/repo/src/lib.rs";
        assert_eq!(
            uri_to_repo_relative_path(repo, uri).as_deref(),
            Some("src/lib.rs")
        );
    }

    #[test]
    fn file_uri_uses_file_scheme() {
        let uri = path_to_file_uri(Path::new("C:/repo"));
        assert!(uri.starts_with("file://"));
    }
}
