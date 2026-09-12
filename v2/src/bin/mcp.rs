use serde_json::{json, Value};
use skb::state::UsageState;
use skb::{FileIndex, SkbPaths};
use skb_v2::{
    AutoSearchRequest, LiveWatcher, MetadataFilter, SharedGenerationEngine, WatcherConfig,
};
use std::io::{self, BufRead, Write};
use std::time::Instant;

const MAX_SEARCH_RESULTS: usize = 100;
const MAX_EXTENSIONS: usize = 32;

fn main() {
    if let Err(error) = run() {
        eprintln!("SKB v2 MCP failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let paths = SkbPaths::discover()?;
    let index = FileIndex::load(&paths.index).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot load SKB index; run `skb scan <root>` first: {error}"),
        )
    })?;
    let state = UsageState::load(&paths.state)?;
    let engine = SharedGenerationEngine::from_parts(index, state, paths.state.clone());
    let watcher = LiveWatcher::start(engine.clone(), WatcherConfig::default())
        .map_err(|error| io::Error::other(format!("cannot start SKB v2 live watcher: {error}")))?;

    let root = engine
        .active_root()
        .map_err(|error| io::Error::other(error.to_string()))?;
    eprintln!("SKB v2 MCP ready: root={root:?}");

    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("invalid JSON-RPC input: {error}");
                continue;
            }
        };

        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            continue;
        };
        if id.is_none() {
            continue;
        }

        let result = match method {
            "initialize" => initialize_result(&message),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(tool_list()),
            "tools/call" => tool_call(&engine, &watcher, &message),
            _ => Err((-32601, format!("method not found: {method}"))),
        };

        let response = match result {
            Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
            Err((code, message)) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
            }
        };
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

fn initialize_result(message: &Value) -> Result<Value, (i64, String)> {
    let requested = message
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("2025-11-25");
    let supported = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
    let protocol = if supported.contains(&requested) {
        requested
    } else {
        "2025-11-25"
    };
    Ok(json!({
        "protocolVersion": protocol,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name":"smart-kernel-brain-v2", "version":env!("CARGO_PKG_VERSION")}
    }))
}

fn tool_list() -> Value {
    json!({"tools":[
        {
            "name":"skb_search",
            "description":"AI-oriented local file retrieval. Automatically chooses exact, prefix, or fuzzy filename search, supports project/path/extension/metadata filters, and returns generation-safe ranked results. Does not read file contents.",
            "inputSchema":{
                "type":"object",
                "properties":{
                    "query":{"type":"string","minLength":1,"description":"Filename, partial filename, path-like hint, or short concept such as 'restor conf'."},
                    "scope":{"type":"string","description":"Optional relative project/workspace subtree, e.g. projects/bio."},
                    "path_contains":{"type":"string","description":"Optional case-insensitive path substring filter."},
                    "extensions":{
                        "type":"array",
                        "items":{"type":"string","minLength":1},
                        "maxItems":32,
                        "description":"Optional extensions such as ['rs','toml']; leading dots are accepted."
                    },
                    "min_size_bytes":{"type":"integer","minimum":0},
                    "max_size_bytes":{"type":"integer","minimum":0},
                    "modified_after_unix_secs":{"type":"integer","minimum":0},
                    "modified_before_unix_secs":{"type":"integer","minimum":0},
                    "include_metadata":{"type":"boolean","default":false,"description":"Include size, modified Unix time, and readonly flag in each hit."},
                    "limit":{"type":"integer","minimum":1,"maximum":100,"default":20}
                },
                "required":["query"],
                "additionalProperties":false
            }
        },
        {
            "name":"skb_status",
            "description":"Return SKB v2 live-index generation, active root, and filesystem watcher health.",
            "inputSchema":{"type":"object","properties":{},"additionalProperties":false}
        }
    ]})
}

fn tool_call(
    engine: &SharedGenerationEngine,
    watcher: &LiveWatcher,
    message: &Value,
) -> Result<Value, (i64, String)> {
    let name = message
        .pointer("/params/name")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32602, "missing tool name".to_string()))?;
    let args = message
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name {
        "skb_search" => {
            let request = parse_search_request(&args)?;
            let started = Instant::now();
            let response = engine
                .search_auto(&request)
                .map_err(|error| (-32603, error.to_string()))?;
            let latency_us = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
            tool_json(json!({
                "query": request.query,
                "latency_us": latency_us,
                "generation": response.generation,
                "plan": response.plan,
                "count": response.hits.len(),
                "truncated": response.truncated,
                "content_read": false,
                "results": response.hits
            }))
        }
        "skb_status" => {
            let generation = engine
                .generation()
                .map_err(|error| (-32603, error.to_string()))?;
            let root = engine
                .active_root()
                .map_err(|error| (-32603, error.to_string()))?;
            let status = watcher
                .status()
                .map_err(|error| (-32603, error.to_string()))?;
            tool_json(json!({
                "generation": generation,
                "root": root,
                "watcher": {
                    "running": status.running,
                    "events_seen": status.events_seen,
                    "batches_seen": status.batches_seen,
                    "rebuilds_succeeded": status.rebuilds_succeeded,
                    "rebuilds_failed": status.rebuilds_failed,
                    "last_generation": status.last_generation,
                    "last_error": status.last_error
                },
                "content_indexing": false,
                "content_reading": false
            }))
        }
        other => Err((-32602, format!("unknown tool: {other}"))),
    }
}

fn parse_search_request(args: &Value) -> Result<AutoSearchRequest, (i64, String)> {
    let query = args.get("query").and_then(Value::as_str).ok_or_else(|| {
        (
            -32602,
            "skb_search requires string argument `query`".to_string(),
        )
    })?;
    if query.trim().is_empty() {
        return Err((-32602, "query must not be empty".to_string()));
    }

    let mut request = AutoSearchRequest::new(query);
    request.scope = optional_string(args, "scope")?;
    request.path_contains = optional_string(args, "path_contains")?;
    request.extensions = parse_extensions(args)?;
    request.include_metadata = args
        .get("include_metadata")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| (-32602, "include_metadata must be a boolean".to_string()))
        })
        .transpose()?
        .unwrap_or(false);
    request.limit = args
        .get("limit")
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| (-32602, "limit must be a positive integer".to_string()))
        })
        .transpose()?
        .unwrap_or(20)
        .clamp(1, MAX_SEARCH_RESULTS as u64) as usize;

    request.metadata = MetadataFilter {
        min_size_bytes: optional_u64(args, "min_size_bytes")?,
        max_size_bytes: optional_u64(args, "max_size_bytes")?,
        modified_after_unix_secs: optional_u64(args, "modified_after_unix_secs")?,
        modified_before_unix_secs: optional_u64(args, "modified_before_unix_secs")?,
    };
    validate_metadata_filter(&request.metadata)?;
    Ok(request)
}

fn optional_string(args: &Value, key: &str) -> Result<Option<String>, (i64, String)> {
    args.get(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| (-32602, format!("{key} must be a string")))
        })
        .transpose()
}

fn optional_u64(args: &Value, key: &str) -> Result<Option<u64>, (i64, String)> {
    args.get(key)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| (-32602, format!("{key} must be a non-negative integer")))
        })
        .transpose()
}

fn parse_extensions(args: &Value) -> Result<Vec<String>, (i64, String)> {
    let Some(raw) = args.get("extensions") else {
        return Ok(Vec::new());
    };
    let values = raw
        .as_array()
        .ok_or_else(|| (-32602, "extensions must be an array of strings".to_string()))?;
    if values.len() > MAX_EXTENSIONS {
        return Err((
            -32602,
            format!("extensions must contain at most {MAX_EXTENSIONS} values"),
        ));
    }
    values
        .iter()
        .map(|value| {
            let extension = value
                .as_str()
                .ok_or_else(|| (-32602, "every extension must be a string".to_string()))?;
            if extension.trim().is_empty() {
                return Err((
                    -32602,
                    "extensions must not contain empty values".to_string(),
                ));
            }
            Ok(extension.to_owned())
        })
        .collect()
}

fn validate_metadata_filter(filter: &MetadataFilter) -> Result<(), (i64, String)> {
    if let (Some(minimum), Some(maximum)) = (filter.min_size_bytes, filter.max_size_bytes) {
        if minimum > maximum {
            return Err((
                -32602,
                "min_size_bytes must be <= max_size_bytes".to_string(),
            ));
        }
    }
    if let (Some(after), Some(before)) = (
        filter.modified_after_unix_secs,
        filter.modified_before_unix_secs,
    ) {
        if after >= before {
            return Err((
                -32602,
                "modified_after_unix_secs must be < modified_before_unix_secs".to_string(),
            ));
        }
    }
    Ok(())
}

fn tool_json(data: Value) -> Result<Value, (i64, String)> {
    let text = serde_json::to_string_pretty(&data).map_err(|error| (-32603, error.to_string()))?;
    Ok(json!({
        "content":[{"type":"text","text":text}],
        "structuredContent": data,
        "isError": false
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_exposes_high_level_search_and_status() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        assert!(tools
            .iter()
            .any(|tool| tool["name"].as_str() == Some("skb_search")));
        assert!(tools
            .iter()
            .any(|tool| tool["name"].as_str() == Some("skb_status")));
    }

    #[test]
    fn search_arguments_map_to_auto_request() {
        let request = parse_search_request(&json!({
            "query":"restor conf",
            "scope":"projects/bio",
            "extensions":[".rs"],
            "min_size_bytes":10,
            "max_size_bytes":1000,
            "include_metadata":true,
            "limit":7
        }))
        .unwrap();

        assert_eq!(request.query, "restor conf");
        assert_eq!(request.scope.as_deref(), Some("projects/bio"));
        assert_eq!(request.extensions, vec![".rs"]);
        assert_eq!(request.metadata.min_size_bytes, Some(10));
        assert_eq!(request.metadata.max_size_bytes, Some(1000));
        assert!(request.include_metadata);
        assert_eq!(request.limit, 7);
    }

    #[test]
    fn invalid_metadata_range_is_rejected() {
        let error = parse_search_request(&json!({
            "query":"config",
            "min_size_bytes":200,
            "max_size_bytes":100
        }))
        .unwrap_err();
        assert_eq!(error.0, -32602);
    }

    #[test]
    fn empty_query_is_rejected() {
        assert!(parse_search_request(&json!({"query":"   "})).is_err());
    }
}
