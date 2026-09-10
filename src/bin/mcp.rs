use serde_json::{json, Value};
use skb::state::UsageState;
use skb::{FileIndex, SearchEngine, SkbPaths};
use std::io::{self, BufRead, Write};

const MAX_MCP_BATCH: usize = 4096;

pub fn run() -> io::Result<()> {
    let paths = SkbPaths::discover()?;
    let index = FileIndex::load(&paths.index).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("cannot load SKB index; run `skb scan <root>` first: {e}"),
        )
    })?;
    let state = UsageState::load(&paths.state)?;
    let mut engine = SearchEngine::new(index, state, paths.state.clone());

    eprintln!("SKB MCP ready: {} files", engine.index.entry_count());
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("invalid JSON-RPC input: {e}");
                continue;
            }
        };

        // Notifications have no id and must not receive responses.
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
            "tools/call" => tool_call(&mut engine, &message),
            _ => Err((-32601, format!("method not found: {method}"))),
        };

        let response = match result {
            Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
            Err((code, msg)) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":msg}})
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
        "serverInfo": {"name":"smart-kernel-brain", "version":env!("CARGO_PKG_VERSION")}
    }))
}

fn tool_list() -> Value {
    json!({"tools":[
        {
            "name":"skb_find_id",
            "description":"Lowest-overhead exact-filename locator. Returns only the first compact file_id and hot-cache flag; no path or adaptive score metadata is materialized.",
            "inputSchema":{
                "type":"object",
                "properties":{
                    "name":{"type":"string","description":"Exact filename, e.g. PlayerController.cs"}
                },
                "required":["name"],
                "additionalProperties":false
            }
        },
        {
            "name":"skb_find_ids",
            "description":"Batch exact-filename locator. Looks up 1..4096 filenames in one MCP tool call. Set compact=true to return only file_id-or-null values in input order.",
            "inputSchema":{
                "type":"object",
                "properties":{
                    "names":{
                        "type":"array",
                        "items":{"type":"string"},
                        "minItems":1,
                        "maxItems":4096
                    },
                    "compact":{"type":"boolean","default":false,"description":"Return only file_id-or-null values and use compact JSON text output."}
                },
                "required":["names"],
                "additionalProperties":false
            }
        },
        {
            "name":"skb_find_paths",
            "description":"One-call batch filename-to-path resolver. Looks up the first exact hit for 1..4096 filenames and materializes paths only because this tool explicitly requests them. Set compact=true for path-or-null values only.",
            "inputSchema":{
                "type":"object",
                "properties":{
                    "names":{
                        "type":"array",
                        "items":{"type":"string"},
                        "minItems":1,
                        "maxItems":4096
                    },
                    "compact":{"type":"boolean","default":false,"description":"Return only path-or-null values and use compact JSON text output."}
                },
                "required":["names"],
                "additionalProperties":false
            }
        },
        {
            "name":"skb_find_refs",
            "description":"Lazy exact-filename lookup. Returns compact file_id references and adaptive metadata without materializing full path strings.",
            "inputSchema":{
                "type":"object",
                "properties":{
                    "name":{"type":"string","description":"Exact filename, e.g. PlayerController.cs"},
                    "limit":{"type":"integer","minimum":1,"maximum":100,"default":20}
                },
                "required":["name"],
                "additionalProperties":false
            }
        },
        {
            "name":"skb_resolve_paths",
            "description":"Resolve 1..4096 SKB file_id references to filename + full path only when the path is actually needed.",
            "inputSchema":{
                "type":"object",
                "properties":{
                    "file_ids":{
                        "type":"array",
                        "items":{"type":"integer","minimum":0,"maximum":4294967295u64},
                        "minItems":1,
                        "maxItems":4096
                    }
                },
                "required":["file_ids"],
                "additionalProperties":false
            }
        },
        {
            "name":"skb_find",
            "description":"Compatibility exact-filename lookup that materializes full paths. Prefer skb_find_refs when paths are not immediately needed.",
            "inputSchema":{
                "type":"object",
                "properties":{
                    "name":{"type":"string","description":"Exact filename, e.g. PlayerController.cs"},
                    "limit":{"type":"integer","minimum":1,"maximum":100,"default":20}
                },
                "required":["name"],
                "additionalProperties":false
            }
        },
        {
            "name":"skb_hot_files",
            "description":"Return the hottest filenames according to SKB's frequency/recency weight.",
            "inputSchema":{
                "type":"object",
                "properties":{"limit":{"type":"integer","minimum":1,"maximum":100,"default":20}},
                "additionalProperties":false
            }
        },
        {
            "name":"skb_stats",
            "description":"Return SKB filename index and hot-cache statistics. Does not inspect file contents.",
            "inputSchema":{"type":"object","properties":{},"additionalProperties":false}
        }
    ]})
}

fn tool_call(engine: &mut SearchEngine, message: &Value) -> Result<Value, (i64, String)> {
    let name = message
        .pointer("/params/name")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32602, "missing tool name".to_string()))?;
    let args = message
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name {
        "skb_find_id" => {
            let filename = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    (
                        -32602,
                        "skb_find_id requires string argument `name`".to_string(),
                    )
                })?;
            let start = std::time::Instant::now();
            let result = engine.find_first_ref(filename);
            let latency_ns = start.elapsed().as_nanos();
            tool_json(json!({
                "query": filename,
                "latency_ns": latency_ns,
                "path_materialized": false,
                "adaptive_metadata_materialized": false,
                "result": result
            }))
        }
        "skb_find_ids" => {
            let names = parse_names(&args, "skb_find_ids")?;
            let compact = compact_requested(&args);
            let start = std::time::Instant::now();

            if compact {
                let results: Vec<Value> = names
                    .iter()
                    .map(|filename| match engine.find_first_ref(filename) {
                        Some(hit) => json!(hit.file_id),
                        None => Value::Null,
                    })
                    .collect();
                let latency_ns = start.elapsed().as_nanos();
                return tool_json_compact(json!({
                    "requested": names.len(),
                    "latency_ns": latency_ns,
                    "path_materialized": false,
                    "compact": true,
                    "results": results
                }));
            }

            let results: Vec<Value> = names
                .iter()
                .map(|filename| match engine.find_first_ref(filename) {
                    Some(hit) => json!({
                        "name": filename,
                        "found": true,
                        "file_id": hit.file_id,
                        "hot_cache_hit": hit.hot_cache_hit
                    }),
                    None => json!({"name": filename, "found": false}),
                })
                .collect();
            let latency_ns = start.elapsed().as_nanos();
            tool_json(json!({
                "requested": names.len(),
                "latency_ns": latency_ns,
                "path_materialized": false,
                "compact": false,
                "results": results
            }))
        }
        "skb_find_paths" => {
            let names = parse_names(&args, "skb_find_paths")?;
            let compact = compact_requested(&args);
            let start = std::time::Instant::now();

            if compact {
                let results: Vec<Value> = names
                    .iter()
                    .map(|filename| {
                        engine
                            .find_first_ref(filename)
                            .and_then(|hit| engine.resolve_file(hit.file_id))
                            .map(|file| json!(file.path))
                            .unwrap_or(Value::Null)
                    })
                    .collect();
                let latency_ns = start.elapsed().as_nanos();
                return tool_json_compact(json!({
                    "requested": names.len(),
                    "latency_ns": latency_ns,
                    "path_materialized": true,
                    "compact": true,
                    "results": results
                }));
            }

            let results: Vec<Value> = names
                .iter()
                .map(|filename| match engine.find_first_ref(filename) {
                    Some(hit) => match engine.resolve_file(hit.file_id) {
                        Some(file) => json!({
                            "name": filename,
                            "found": true,
                            "file_id": hit.file_id,
                            "hot_cache_hit": hit.hot_cache_hit,
                            "path": file.path
                        }),
                        None => json!({
                            "name": filename,
                            "found": false,
                            "error": "matched file_id could not be resolved"
                        }),
                    },
                    None => json!({"name": filename, "found": false}),
                })
                .collect();
            let latency_ns = start.elapsed().as_nanos();
            tool_json(json!({
                "requested": names.len(),
                "latency_ns": latency_ns,
                "path_materialized": true,
                "compact": false,
                "results": results
            }))
        }
        "skb_find_refs" => {
            let filename = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    (
                        -32602,
                        "skb_find_refs requires string argument `name`".to_string(),
                    )
                })?;
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 100) as usize;
            let (refs, latency_ns) = engine
                .find_lazy(filename, limit, true)
                .map_err(|e| (-32603, e.to_string()))?;
            tool_json(json!({
                "query": filename,
                "latency_ns": latency_ns,
                "count": refs.len(),
                "path_materialized": false,
                "results": refs
            }))
        }
        "skb_resolve_paths" => {
            let raw_ids = args
                .get("file_ids")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    (
                        -32602,
                        "skb_resolve_paths requires array argument `file_ids`".to_string(),
                    )
                })?;
            if raw_ids.is_empty() || raw_ids.len() > MAX_MCP_BATCH {
                return Err((
                    -32602,
                    format!("file_ids must contain 1..{MAX_MCP_BATCH} IDs"),
                ));
            }
            let mut file_ids = Vec::with_capacity(raw_ids.len());
            for value in raw_ids {
                let raw = value.as_u64().ok_or_else(|| {
                    (
                        -32602,
                        "every file_id must be a non-negative integer".to_string(),
                    )
                })?;
                let file_id = u32::try_from(raw)
                    .map_err(|_| (-32602, format!("file_id out of u32 range: {raw}")))?;
                file_ids.push(file_id);
            }
            let results = engine.resolve_files(&file_ids);
            tool_json(json!({
                "requested": file_ids.len(),
                "resolved": results.len(),
                "results": results
            }))
        }
        "skb_find" => {
            let filename = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    (
                        -32602,
                        "skb_find requires string argument `name`".to_string(),
                    )
                })?;
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 100) as usize;
            let (hits, latency_us) = engine
                .find(filename, limit, true)
                .map_err(|e| (-32603, e.to_string()))?;
            tool_json(json!({
                "query": filename,
                "latency_us": latency_us,
                "count": hits.len(),
                "results": hits
            }))
        }
        "skb_hot_files" => {
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 100) as usize;
            tool_json(json!({"results": engine.hot_names(limit)}))
        }
        "skb_stats" => tool_json(json!({
            "root": engine.index.root.clone(),
            "files": engine.index.entry_count(),
            "directories": engine.index.directory_count(),
            "compact_payload_bytes": engine.index.payload_bytes(),
            "fixed_metadata_bytes_per_file": FileIndex::fixed_metadata_bytes_per_file(),
            "hot_names": engine.hot_name_count(),
            "hot_entries": engine.hot_entry_count(),
            "hot_slots": engine.hot_slot_count(),
            "hot_ways": 4,
            "usage_records": engine.state.records.len(),
            "lazy_path": true,
            "content_indexing": false,
            "max_batch": MAX_MCP_BATCH
        })),
        other => Err((-32602, format!("unknown tool: {other}"))),
    }
}

fn parse_names<'a>(args: &'a Value, tool: &str) -> Result<Vec<&'a str>, (i64, String)> {
    let raw_names = args
        .get("names")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            (
                -32602,
                format!("{tool} requires array argument `names`"),
            )
        })?;
    if raw_names.is_empty() || raw_names.len() > MAX_MCP_BATCH {
        return Err((
            -32602,
            format!("names must contain 1..{MAX_MCP_BATCH} filenames"),
        ));
    }

    let mut names = Vec::with_capacity(raw_names.len());
    for value in raw_names {
        let name = value
            .as_str()
            .ok_or_else(|| (-32602, "every name must be a string".to_string()))?;
        if name.is_empty() {
            return Err((-32602, "filenames must not be empty".to_string()));
        }
        names.push(name);
    }
    Ok(names)
}

fn compact_requested(args: &Value) -> bool {
    args.get("compact")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn tool_json(data: Value) -> Result<Value, (i64, String)> {
    let text = serde_json::to_string_pretty(&data).map_err(|e| (-32603, e.to_string()))?;
    Ok(json!({
        "content":[{"type":"text","text":text}],
        "structuredContent": data,
        "isError": false
    }))
}

fn tool_json_compact(data: Value) -> Result<Value, (i64, String)> {
    let text = serde_json::to_string(&data).map_err(|e| (-32603, e.to_string()))?;
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
    fn batch_tool_schemas_match_runtime_limit() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        for name in ["skb_find_ids", "skb_find_paths"] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"].as_str() == Some(name))
                .unwrap();
            assert_eq!(
                tool.pointer("/inputSchema/properties/names/maxItems")
                    .and_then(Value::as_u64),
                Some(MAX_MCP_BATCH as u64)
            );
        }
        let resolve = tools
            .iter()
            .find(|tool| tool["name"].as_str() == Some("skb_resolve_paths"))
            .unwrap();
        assert_eq!(
            resolve
                .pointer("/inputSchema/properties/file_ids/maxItems")
                .and_then(Value::as_u64),
            Some(MAX_MCP_BATCH as u64)
        );
    }

    #[test]
    fn compact_tool_output_is_single_line() {
        let response = tool_json_compact(json!({"results":[1, null, 3]})).unwrap();
        let text = response["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains('\n'));
    }
}
