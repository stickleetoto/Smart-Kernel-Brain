use crate::{FileIndex, SearchEngine, SkbPaths};
use crate::state::UsageState;
use serde::Serialize;
use std::env;
use std::io;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Instant;

pub const RESIDENT_VERSION: &str = "1.0.0";

#[cfg(windows)]
pub const DEFAULT_RESIDENT_ENDPOINT: &str = r"\\.\pipe\smart-kernel-brain-v1";
#[cfg(not(windows))]
pub const DEFAULT_RESIDENT_ENDPOINT: &str = "127.0.0.1:48731";

/// v0.6 prefers SKB_PIPE_NAME on Windows. SKB_DAEMON_ADDR remains a compatibility
/// override so existing scripts do not silently stop working.
pub fn resident_addr() -> String {
    #[cfg(windows)]
    {
        if let Ok(pipe) = env::var("SKB_PIPE_NAME") {
            return pipe;
        }
        if let Ok(legacy) = env::var("SKB_DAEMON_ADDR") {
            if legacy.starts_with("\\\\.\\pipe\\") {
                return legacy;
            }
        }
        DEFAULT_RESIDENT_ENDPOINT.to_string()
    }
    #[cfg(not(windows))]
    {
        env::var("SKB_DAEMON_ADDR").unwrap_or_else(|_| DEFAULT_RESIDENT_ENDPOINT.to_string())
    }
}

fn load_engine(paths: &SkbPaths) -> io::Result<SearchEngine> {
    let index = FileIndex::load(&paths.index).map_err(|e| {
        io::Error::new(e.kind(), format!("cannot load compact index (run `skb scan <root>` first): {e}"))
    })?;
    let state = UsageState::load(&paths.state)?;
    Ok(SearchEngine::new(index, state, paths.state.clone()))
}

#[derive(Debug, Clone, Serialize)]
pub struct ResidentPing {
    pub ok: bool,
    pub version: String,
    pub files: usize,
    pub transport: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ResidentFindResult {
    pub file_id: u32,
    pub hot_cache_hit: bool,
    pub server_ns: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ResidentBatchItem {
    pub file_id: u32,
    pub hot_cache_hit: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResidentBatchFindResult {
    pub results: Vec<Option<ResidentBatchItem>>,
    pub server_ns: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResidentResolveResult {
    pub file_id: u32,
    pub name: String,
    pub path: String,
    pub server_ns: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResidentStats {
    pub ok: bool,
    pub version: String,
    pub transport: String,
    pub root: String,
    pub files: usize,
    pub directories: usize,
    pub compact_payload_bytes: usize,
    pub hot_names: usize,
    pub hot_entries: usize,
    pub hot_slots: usize,
    pub content_indexing: bool,
    pub resident_core: bool,
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use std::thread;

    type Handle = *mut c_void;

    const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
    const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
    const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
    const PIPE_READMODE_BYTE: u32 = 0x0000_0000;
    const PIPE_WAIT: u32 = 0x0000_0000;
    const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
    const PIPE_UNLIMITED_INSTANCES: u32 = 255;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const OPEN_EXISTING: u32 = 3;
    const ERROR_PIPE_CONNECTED: u32 = 535;
    const ERROR_PIPE_BUSY: u32 = 231;
    const ERROR_BROKEN_PIPE: u32 = 109;
    const ERROR_NO_DATA: u32 = 232;
    const MAX_FRAME: usize = 1 << 20;

    const OP_PING: u8 = 1;
    const OP_FIND_FIRST: u8 = 2;
    const OP_RESOLVE: u8 = 3;
    const OP_STATS: u8 = 4;
    const OP_SHUTDOWN: u8 = 5;
    const OP_FIND_BATCH: u8 = 6;

    const MAX_BATCH: usize = 4096;

    const STATUS_OK: u8 = 0;
    const STATUS_ERROR: u8 = 1;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateNamedPipeW(
            lpName: *const u16,
            dwOpenMode: u32,
            dwPipeMode: u32,
            nMaxInstances: u32,
            nOutBufferSize: u32,
            nInBufferSize: u32,
            nDefaultTimeOut: u32,
            lpSecurityAttributes: *mut c_void,
        ) -> Handle;
        fn ConnectNamedPipe(hNamedPipe: Handle, lpOverlapped: *mut c_void) -> i32;
        fn DisconnectNamedPipe(hNamedPipe: Handle) -> i32;
        fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: u32,
            dwShareMode: u32,
            lpSecurityAttributes: *mut c_void,
            dwCreationDisposition: u32,
            dwFlagsAndAttributes: u32,
            hTemplateFile: Handle,
        ) -> Handle;
        fn WaitNamedPipeW(lpNamedPipeName: *const u16, nTimeOut: u32) -> i32;
        fn ReadFile(
            hFile: Handle,
            lpBuffer: *mut c_void,
            nNumberOfBytesToRead: u32,
            lpNumberOfBytesRead: *mut u32,
            lpOverlapped: *mut c_void,
        ) -> i32;
        fn WriteFile(
            hFile: Handle,
            lpBuffer: *const c_void,
            nNumberOfBytesToWrite: u32,
            lpNumberOfBytesWritten: *mut u32,
            lpOverlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(hObject: Handle) -> i32;
        fn GetLastError() -> u32;
    }

    struct OwnedHandle(Handle);
    unsafe impl Send for OwnedHandle {}

    impl OwnedHandle {
        fn raw(&self) -> Handle { self.0 }
    }

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe { CloseHandle(self.0); }
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(std::iter::once(0)).collect()
    }

    fn last_error(context: &str) -> io::Error {
        let code = unsafe { GetLastError() } as i32;
        io::Error::new(io::ErrorKind::Other, format!("{context}: {}", io::Error::from_raw_os_error(code)))
    }

    fn create_server_pipe(endpoint: &str) -> io::Result<OwnedHandle> {
        let name = wide(endpoint);
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateNamedPipeW failed"));
        }
        Ok(OwnedHandle(handle))
    }

    fn accept_pipe(handle: Handle) -> io::Result<()> {
        let ok = unsafe { ConnectNamedPipe(handle, ptr::null_mut()) };
        if ok != 0 {
            return Ok(());
        }
        let code = unsafe { GetLastError() };
        if code == ERROR_PIPE_CONNECTED {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::Other, format!(
                "ConnectNamedPipe failed: {}",
                io::Error::from_raw_os_error(code as i32)
            )))
        }
    }

    fn connect_pipe(endpoint: &str) -> io::Result<OwnedHandle> {
        let name = wide(endpoint);
        for attempt in 0..2 {
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    ptr::null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                return Ok(OwnedHandle(handle));
            }
            let code = unsafe { GetLastError() };
            if code == ERROR_PIPE_BUSY && attempt == 0 {
                let waited = unsafe { WaitNamedPipeW(name.as_ptr(), 5000) };
                if waited != 0 {
                    continue;
                }
            }
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, format!(
                "cannot connect to SKB resident pipe at {endpoint}: {}",
                io::Error::from_raw_os_error(code as i32)
            )));
        }
        Err(io::Error::new(io::ErrorKind::ConnectionRefused, "resident pipe unavailable"))
    }

    fn write_all(handle: Handle, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let request = bytes.len().min(u32::MAX as usize) as u32;
            let mut written = 0u32;
            let ok = unsafe {
                WriteFile(
                    handle,
                    bytes.as_ptr().cast(),
                    request,
                    &mut written,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(last_error("WriteFile failed"));
            }
            if written == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "named pipe wrote zero bytes"));
            }
            bytes = &bytes[written as usize..];
        }
        Ok(())
    }

    fn read_exact(handle: Handle, bytes: &mut [u8]) -> io::Result<bool> {
        let mut offset = 0usize;
        while offset < bytes.len() {
            let request = (bytes.len() - offset).min(u32::MAX as usize) as u32;
            let mut read = 0u32;
            let ok = unsafe {
                ReadFile(
                    handle,
                    bytes[offset..].as_mut_ptr().cast(),
                    request,
                    &mut read,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                let code = unsafe { GetLastError() };
                if (code == ERROR_BROKEN_PIPE || code == ERROR_NO_DATA) && offset == 0 {
                    return Ok(false);
                }
                return Err(io::Error::new(io::ErrorKind::Other, format!(
                    "ReadFile failed: {}",
                    io::Error::from_raw_os_error(code as i32)
                )));
            }
            if read == 0 {
                return Ok(false);
            }
            offset += read as usize;
        }
        Ok(true)
    }

    fn write_frame(handle: Handle, op: u8, payload: &[u8], scratch: &mut Vec<u8>) -> io::Result<()> {
        let body_len = 1usize.checked_add(payload.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
        if body_len > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame exceeds 1 MiB"));
        }
        scratch.clear();
        scratch.reserve(4 + body_len);
        scratch.extend_from_slice(&(body_len as u32).to_le_bytes());
        scratch.push(op);
        scratch.extend_from_slice(payload);
        write_all(handle, scratch)
    }

    fn read_frame(handle: Handle, scratch: &mut Vec<u8>) -> io::Result<Option<(u8, &[u8])>> {
        let mut header = [0u8; 4];
        if !read_exact(handle, &mut header)? {
            return Ok(None);
        }
        let len = u32::from_le_bytes(header) as usize;
        if len == 0 || len > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("invalid frame length: {len}")));
        }
        scratch.resize(len, 0);
        if !read_exact(handle, scratch)? {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "resident pipe closed mid-frame"));
        }
        Ok(Some((scratch[0], &scratch[1..])))
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) { out.extend_from_slice(&value.to_le_bytes()); }
    fn push_u64(out: &mut Vec<u8>, value: u64) { out.extend_from_slice(&value.to_le_bytes()); }

    fn push_u16(out: &mut Vec<u8>, value: u16) { out.extend_from_slice(&value.to_le_bytes()); }

    fn take_u32(bytes: &mut &[u8]) -> io::Result<u32> {
        if bytes.len() < 4 { return Err(io::Error::new(io::ErrorKind::InvalidData, "short u32 field")); }
        let value = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        *bytes = &bytes[4..];
        Ok(value)
    }

    fn take_u16(bytes: &mut &[u8]) -> io::Result<u16> {
        if bytes.len() < 2 { return Err(io::Error::new(io::ErrorKind::InvalidData, "short u16 field")); }
        let value = u16::from_le_bytes(bytes[..2].try_into().unwrap());
        *bytes = &bytes[2..];
        Ok(value)
    }

    fn take_u64(bytes: &mut &[u8]) -> io::Result<u64> {
        if bytes.len() < 8 { return Err(io::Error::new(io::ErrorKind::InvalidData, "short u64 field")); }
        let value = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        *bytes = &bytes[8..];
        Ok(value)
    }

    fn take_string(bytes: &mut &[u8]) -> io::Result<String> {
        let len = take_u32(bytes)? as usize;
        if bytes.len() < len { return Err(io::Error::new(io::ErrorKind::InvalidData, "short string field")); }
        let value = std::str::from_utf8(&bytes[..len])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
            .to_owned();
        *bytes = &bytes[len..];
        Ok(value)
    }

    fn push_string(out: &mut Vec<u8>, value: &str) {
        push_u32(out, value.len() as u32);
        out.extend_from_slice(value.as_bytes());
    }

    fn write_response_frame(
        handle: Handle,
        op: u8,
        status: u8,
        server_ns: u64,
        payload: &[u8],
        scratch: &mut Vec<u8>,
    ) -> io::Result<()> {
        // One synchronous WriteFile call for the complete response frame.
        let body_len = 1usize + 1 + 1 + 8 + payload.len(); // wire op + status + echoed op + ns + payload
        if body_len > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame exceeds 1 MiB"));
        }
        scratch.clear();
        scratch.reserve(4 + body_len);
        scratch.extend_from_slice(&(body_len as u32).to_le_bytes());
        scratch.push(op);
        scratch.push(status);
        scratch.push(op);
        push_u64(scratch, server_ns);
        scratch.extend_from_slice(payload);
        write_all(handle, scratch)
    }

    fn write_ok(handle: Handle, op: u8, server_ns: u64, payload: &[u8], scratch: &mut Vec<u8>) -> io::Result<()> {
        write_response_frame(handle, op, STATUS_OK, server_ns, payload, scratch)
    }

    fn write_error(handle: Handle, op: u8, message: &str, scratch: &mut Vec<u8>) -> io::Result<()> {
        write_response_frame(handle, op, STATUS_ERROR, 0, message.as_bytes(), scratch)
    }

    pub fn run_daemon(paths: &SkbPaths, endpoint: &str) -> io::Result<()> {
        let engine = Arc::new(load_engine(paths)?);
        let shutdown = Arc::new(AtomicBool::new(false));

        println!("SKB Resident Core v{RESIDENT_VERSION}");
        println!("transport     : Windows Named Pipe + binary framing");
        println!("endpoint      : {endpoint}");
        println!("files         : {}", engine.index.entry_count());
        println!("compact RAM   : {} bytes lowerbound", engine.index.payload_bytes());
        println!("hot slots     : {}", engine.hot_slot_count());
        println!("mode          : read-only locator IPC; file contents are never read");

        while !shutdown.load(Ordering::Acquire) {
            let pipe = create_server_pipe(endpoint)?;
            accept_pipe(pipe.raw())?;
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            let engine = Arc::clone(&engine);
            let shutdown = Arc::clone(&shutdown);
            let wake_endpoint = endpoint.to_string();
            thread::spawn(move || {
                if let Err(e) = handle_connection(pipe, &engine, &shutdown, &wake_endpoint) {
                    eprintln!("skb daemon client error: {e}");
                }
            });
        }
        Ok(())
    }

    fn handle_connection(
        pipe: OwnedHandle,
        engine: &SearchEngine,
        shutdown: &AtomicBool,
        wake_endpoint: &str,
    ) -> io::Result<()> {
        let mut rx = Vec::with_capacity(256);
        let mut tx = Vec::with_capacity(256);
        let mut out = Vec::with_capacity(256);
        loop {
            let Some((op, payload)) = read_frame(pipe.raw(), &mut rx)? else { break };
            match op {
                OP_PING => {
                    out.clear();
                    push_u64(&mut out, engine.index.entry_count() as u64);
                    write_ok(pipe.raw(), op, 0, &out, &mut tx)?;
                }
                OP_FIND_FIRST => {
                    let name = match std::str::from_utf8(payload) {
                        Ok(v) if !v.is_empty() => v,
                        Ok(_) => {
                            write_error(pipe.raw(), op, "empty filename", &mut tx)?;
                            continue;
                        }
                        Err(e) => {
                            write_error(pipe.raw(), op, &format!("invalid UTF-8 filename: {e}"), &mut tx)?;
                            continue;
                        }
                    };
                    let start = Instant::now();
                    let result = engine.find_first_ref(name);
                    let server_ns = start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                    out.clear();
                    match result {
                        Some(hit) => {
                            out.push(1);
                            push_u32(&mut out, hit.file_id);
                            out.push(u8::from(hit.hot_cache_hit));
                        }
                        None => out.push(0),
                    }
                    write_ok(pipe.raw(), op, server_ns, &out, &mut tx)?;
                }
                OP_FIND_BATCH => {
                    let mut rest = payload;
                    let count = match take_u32(&mut rest) {
                        Ok(v) => v as usize,
                        Err(e) => {
                            write_error(pipe.raw(), op, &format!("invalid batch header: {e}"), &mut tx)?;
                            continue;
                        }
                    };
                    if count == 0 || count > MAX_BATCH {
                        write_error(pipe.raw(), op, "batch count must be 1..4096", &mut tx)?;
                        continue;
                    }

                    // Fixed-width response entry: found:u8 + file_id:u32 + hot:u8.
                    // The response preserves request ordering exactly.
                    out.clear();
                    out.reserve(4 + count * 6);
                    push_u32(&mut out, count as u32);
                    let start = Instant::now();
                    let mut valid = true;
                    for _ in 0..count {
                        let name_len = match take_u16(&mut rest) {
                            Ok(v) => v as usize,
                            Err(_) => {
                                valid = false;
                                break;
                            }
                        };
                        if name_len == 0 || rest.len() < name_len {
                            valid = false;
                            break;
                        }
                        let name_bytes = &rest[..name_len];
                        rest = &rest[name_len..];
                        let Ok(name) = std::str::from_utf8(name_bytes) else {
                            valid = false;
                            break;
                        };
                        match engine.find_first_ref(name) {
                            Some(hit) => {
                                out.push(1);
                                push_u32(&mut out, hit.file_id);
                                out.push(u8::from(hit.hot_cache_hit));
                            }
                            None => {
                                out.push(0);
                                push_u32(&mut out, 0);
                                out.push(0);
                            }
                        }
                    }
                    if !valid || !rest.is_empty() {
                        write_error(pipe.raw(), op, "malformed batch payload", &mut tx)?;
                        continue;
                    }
                    let server_ns = start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                    write_ok(pipe.raw(), op, server_ns, &out, &mut tx)?;
                }
                OP_RESOLVE => {
                    if payload.len() != 4 {
                        write_error(pipe.raw(), op, "resolve requires 4-byte file_id", &mut tx)?;
                        continue;
                    }
                    let file_id = u32::from_le_bytes(payload.try_into().unwrap());
                    let start = Instant::now();
                    let result = engine.resolve_file(file_id);
                    let server_ns = start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                    out.clear();
                    match result {
                        Some(file) => {
                            out.push(1);
                            push_u32(&mut out, file.file_id);
                            push_string(&mut out, &file.name);
                            push_string(&mut out, &file.path);
                        }
                        None => out.push(0),
                    }
                    write_ok(pipe.raw(), op, server_ns, &out, &mut tx)?;
                }
                OP_STATS => {
                    out.clear();
                    if out.capacity() < 128 + engine.index.root.len() {
                        out.reserve(128 + engine.index.root.len() - out.capacity());
                    }
                    push_string(&mut out, &engine.index.root);
                    push_u64(&mut out, engine.index.entry_count() as u64);
                    push_u64(&mut out, engine.index.directory_count() as u64);
                    push_u64(&mut out, engine.index.payload_bytes() as u64);
                    push_u64(&mut out, engine.hot_name_count() as u64);
                    push_u64(&mut out, engine.hot_entry_count() as u64);
                    push_u64(&mut out, engine.hot_slot_count() as u64);
                    write_ok(pipe.raw(), op, 0, &out, &mut tx)?;
                }
                OP_SHUTDOWN => {
                    shutdown.store(true, Ordering::Release);
                    write_ok(pipe.raw(), op, 0, &[], &mut tx)?;
                    let _ = connect_pipe(wake_endpoint);
                    break;
                }
                _ => write_error(pipe.raw(), op, "unknown resident opcode", &mut tx)?,
            }
        }
        unsafe { DisconnectNamedPipe(pipe.raw()); }
        Ok(())
    }

    pub struct ResidentClient {
        pipe: OwnedHandle,
        tx: Vec<u8>,
        rx: Vec<u8>,
        batch_payload: Vec<u8>,
    }

    impl ResidentClient {
        pub fn connect(endpoint: &str) -> io::Result<Self> {
            Ok(Self {
                pipe: connect_pipe(endpoint)?,
                tx: Vec::with_capacity(256),
                rx: Vec::with_capacity(256),
                batch_payload: Vec::with_capacity(1024),
            })
        }

        fn request(&mut self, op: u8, payload: &[u8]) -> io::Result<(u64, &[u8])> {
            write_frame(self.pipe.raw(), op, payload, &mut self.tx)?;
            let Some((_wire_op, body)) = read_frame(self.pipe.raw(), &mut self.rx)? else {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "resident core closed connection"));
            };
            if body.len() < 10 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "short resident response"));
            }
            let status = body[0];
            let response_op = body[1];
            if response_op != op {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "resident opcode mismatch"));
            }
            let server_ns = u64::from_le_bytes(body[2..10].try_into().unwrap());
            if status == STATUS_ERROR {
                let message = String::from_utf8_lossy(&body[10..]).into_owned();
                return Err(io::Error::new(io::ErrorKind::Other, message));
            }
            if status != STATUS_OK {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown resident response status"));
            }
            Ok((server_ns, &body[10..]))
        }

        pub fn ping(&mut self) -> io::Result<ResidentPing> {
            let (_, payload) = self.request(OP_PING, &[])?;
            let mut rest = payload;
            let files = take_u64(&mut rest)? as usize;
            Ok(ResidentPing {
                ok: true,
                version: RESIDENT_VERSION.to_string(),
                files,
                transport: "windows-named-pipe/binary".to_string(),
            })
        }

        pub fn find_first(&mut self, name: &str) -> io::Result<Option<ResidentFindResult>> {
            let (server_ns, payload) = self.request(OP_FIND_FIRST, name.as_bytes())?;
            if payload.is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "short find response"));
            }
            if payload[0] == 0 {
                return Ok(None);
            }
            if payload.len() < 6 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "short find hit response"));
            }
            Ok(Some(ResidentFindResult {
                file_id: u32::from_le_bytes(payload[1..5].try_into().unwrap()),
                hot_cache_hit: payload[5] != 0,
                server_ns,
            }))
        }

        /// Batch exact-filename lookup over one resident IPC request.
        ///
        /// The caller supplies reusable output storage. After the first call, both
        /// request and result buffers can be reused without per-batch heap churn.
        pub fn find_batch_reuse(
            &mut self,
            names: &[&str],
            out: &mut Vec<Option<ResidentBatchItem>>,
        ) -> io::Result<u64> {
            if names.is_empty() || names.len() > MAX_BATCH {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "batch size must be 1..4096"));
            }

            self.batch_payload.clear();
            let estimated = 4usize.saturating_add(names.iter().map(|name| 2 + name.len()).sum::<usize>());
            if estimated > MAX_FRAME - 1 {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "batch request exceeds resident frame limit"));
            }
            if self.batch_payload.capacity() < estimated {
                self.batch_payload.reserve(estimated);
            }
            push_u32(&mut self.batch_payload, names.len() as u32);
            for name in names {
                let bytes = name.as_bytes();
                if bytes.is_empty() || bytes.len() > u16::MAX as usize {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "every filename must be 1..65535 UTF-8 bytes"));
                }
                push_u16(&mut self.batch_payload, bytes.len() as u16);
                self.batch_payload.extend_from_slice(bytes);
            }

            // Borrow disjoint client fields so the reusable payload does not need
            // to be cloned before issuing the request.
            let pipe = self.pipe.raw();
            write_frame(pipe, OP_FIND_BATCH, &self.batch_payload, &mut self.tx)?;
            let Some((_wire_op, body)) = read_frame(pipe, &mut self.rx)? else {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "resident core closed connection"));
            };
            if body.len() < 10 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "short resident batch response"));
            }
            let status = body[0];
            let response_op = body[1];
            if response_op != OP_FIND_BATCH {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "resident batch opcode mismatch"));
            }
            let server_ns = u64::from_le_bytes(body[2..10].try_into().unwrap());
            if status == STATUS_ERROR {
                let message = String::from_utf8_lossy(&body[10..]).into_owned();
                return Err(io::Error::new(io::ErrorKind::Other, message));
            }
            if status != STATUS_OK {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown resident batch response status"));
            }

            let payload = &body[10..];
            if payload.len() < 4 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "short resident batch payload"));
            }
            let count = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
            if count != names.len() || payload.len() != 4 + count * 6 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "resident batch result count mismatch"));
            }
            out.clear();
            if out.capacity() < count { out.reserve(count); }
            let mut cursor = 4usize;
            for _ in 0..count {
                let found = payload[cursor] != 0;
                let file_id = u32::from_le_bytes(payload[cursor + 1..cursor + 5].try_into().unwrap());
                let hot = payload[cursor + 5] != 0;
                out.push(found.then_some(ResidentBatchItem { file_id, hot_cache_hit: hot }));
                cursor += 6;
            }
            Ok(server_ns)
        }

        pub fn find_batch(&mut self, names: &[&str]) -> io::Result<ResidentBatchFindResult> {
            let mut results = Vec::with_capacity(names.len());
            let server_ns = self.find_batch_reuse(names, &mut results)?;
            Ok(ResidentBatchFindResult { results, server_ns })
        }

        pub fn resolve(&mut self, file_id: u32) -> io::Result<Option<ResidentResolveResult>> {
            let (server_ns, payload) = self.request(OP_RESOLVE, &file_id.to_le_bytes())?;
            if payload.is_empty() { return Err(io::Error::new(io::ErrorKind::InvalidData, "short resolve response")); }
            if payload[0] == 0 { return Ok(None); }
            let mut rest = &payload[1..];
            let resolved_id = take_u32(&mut rest)?;
            let name = take_string(&mut rest)?;
            let path = take_string(&mut rest)?;
            Ok(Some(ResidentResolveResult { file_id: resolved_id, name, path, server_ns }))
        }

        pub fn stats(&mut self) -> io::Result<ResidentStats> {
            let (_, payload) = self.request(OP_STATS, &[])?;
            let mut rest = payload;
            let root = take_string(&mut rest)?;
            Ok(ResidentStats {
                ok: true,
                version: RESIDENT_VERSION.to_string(),
                transport: "windows-named-pipe/binary".to_string(),
                root,
                files: take_u64(&mut rest)? as usize,
                directories: take_u64(&mut rest)? as usize,
                compact_payload_bytes: take_u64(&mut rest)? as usize,
                hot_names: take_u64(&mut rest)? as usize,
                hot_entries: take_u64(&mut rest)? as usize,
                hot_slots: take_u64(&mut rest)? as usize,
                content_indexing: false,
                resident_core: true,
            })
        }

        pub fn shutdown(&mut self) -> io::Result<bool> {
            let _ = self.request(OP_SHUTDOWN, &[])?;
            Ok(true)
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    pub fn run_daemon(paths: &SkbPaths, endpoint: &str) -> io::Result<()> {
        let engine = Arc::new(load_engine(paths)?);
        let listener = TcpListener::bind(endpoint)?;
        let shutdown = Arc::new(AtomicBool::new(false));
        println!("SKB Resident Core v{RESIDENT_VERSION}");
        println!("transport     : loopback TCP + JSON fallback");
        println!("endpoint      : {endpoint}");
        while !shutdown.load(Ordering::Acquire) {
            let (stream, _) = listener.accept()?;
            if shutdown.load(Ordering::Acquire) { break; }
            let engine = Arc::clone(&engine);
            let shutdown = Arc::clone(&shutdown);
            let wake = endpoint.to_string();
            thread::spawn(move || { let _ = handle(stream, &engine, &shutdown, &wake); });
        }
        Ok(())
    }

    fn handle(stream: TcpStream, engine: &SearchEngine, shutdown: &AtomicBool, wake: &str) -> io::Result<()> {
        stream.set_nodelay(true)?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 { break; }
            let req: Value = serde_json::from_str(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            let op = req.get("op").and_then(Value::as_str).unwrap_or("");
            let response = match op {
                "ping" => json!({"ok":true,"version":RESIDENT_VERSION,"files":engine.index.entry_count()}),
                "find_first" => {
                    let name = req.get("name").and_then(Value::as_str).unwrap_or("");
                    let start = Instant::now();
                    let result = engine.find_first_ref(name);
                    json!({"ok":true,"result":result,"server_ns":start.elapsed().as_nanos() as u64})
                }
                "find_batch" => {
                    let names = req.get("names").and_then(Value::as_array).cloned().unwrap_or_default();
                    if names.is_empty() || names.len() > 4096 {
                        json!({"ok":false,"error":"batch size must be 1..4096"})
                    } else {
                        let start = Instant::now();
                        let results: Vec<Value> = names.iter().map(|value| {
                            let name = value.as_str().unwrap_or("");
                            match engine.find_first_ref(name) {
                                Some(hit) => json!({"file_id":hit.file_id,"hot_cache_hit":hit.hot_cache_hit}),
                                None => Value::Null,
                            }
                        }).collect();
                        json!({"ok":true,"results":results,"server_ns":start.elapsed().as_nanos() as u64})
                    }
                }
                "resolve" => {
                    let id = req.get("file_id").and_then(Value::as_u64).unwrap_or(u64::MAX);
                    let start = Instant::now();
                    let result = u32::try_from(id).ok().and_then(|v| engine.resolve_file(v));
                    json!({"ok":true,"result":result,"server_ns":start.elapsed().as_nanos() as u64})
                }
                "stats" => json!({"ok":true,"root":engine.index.root.clone(),"files":engine.index.entry_count(),"directories":engine.index.directory_count(),"compact_payload_bytes":engine.index.payload_bytes(),"hot_names":engine.hot_name_count(),"hot_entries":engine.hot_entry_count(),"hot_slots":engine.hot_slot_count()}),
                "shutdown" => { shutdown.store(true, Ordering::Release); let _ = TcpStream::connect(wake); json!({"ok":true}) },
                _ => json!({"ok":false,"error":"unknown op"}),
            };
            serde_json::to_writer(&mut writer, &response)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            if op == "shutdown" { break; }
        }
        Ok(())
    }

    pub struct ResidentClient {
        reader: BufReader<TcpStream>,
        writer: TcpStream,
    }

    impl ResidentClient {
        pub fn connect(endpoint: &str) -> io::Result<Self> {
            let stream = TcpStream::connect(endpoint)?;
            stream.set_nodelay(true)?;
            Ok(Self { reader: BufReader::new(stream.try_clone()?), writer: stream })
        }
        fn request(&mut self, value: &Value) -> io::Result<Value> {
            serde_json::to_writer(&mut self.writer, value)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            self.writer.write_all(b"\n")?;
            self.writer.flush()?;
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 { return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "resident closed")); }
            serde_json::from_str(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
        }
        pub fn ping(&mut self) -> io::Result<ResidentPing> {
            let v = self.request(&json!({"op":"ping"}))?;
            Ok(ResidentPing { ok:true, version:RESIDENT_VERSION.to_string(), files:v.get("files").and_then(Value::as_u64).unwrap_or(0) as usize, transport:"tcp/json-fallback".into() })
        }
        pub fn find_first(&mut self, name:&str) -> io::Result<Option<ResidentFindResult>> {
            let v = self.request(&json!({"op":"find_first","name":name}))?;
            let ns = v.get("server_ns").and_then(Value::as_u64).unwrap_or(0);
            let Some(r)=v.get("result").filter(|v| !v.is_null()) else { return Ok(None); };
            Ok(Some(ResidentFindResult { file_id:r.get("file_id").and_then(Value::as_u64).unwrap_or(0) as u32, hot_cache_hit:r.get("hot_cache_hit").and_then(Value::as_bool).unwrap_or(false), server_ns:ns }))
        }
        pub fn find_batch_reuse(&mut self, names:&[&str], out:&mut Vec<Option<ResidentBatchItem>>) -> io::Result<u64> {
            if names.is_empty() || names.len() > 4096 {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "batch size must be 1..4096"));
            }
            let v=self.request(&json!({"op":"find_batch","names":names}))?;
            if !v.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                return Err(io::Error::new(io::ErrorKind::Other, v.get("error").and_then(Value::as_str).unwrap_or("batch lookup failed")));
            }
            let ns=v.get("server_ns").and_then(Value::as_u64).unwrap_or(0);
            let results=v.get("results").and_then(Value::as_array)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing batch results"))?;
            if results.len()!=names.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "batch result count mismatch"));
            }
            out.clear();
            if out.capacity()<results.len(){ out.reserve(results.len()); }
            for value in results {
                if value.is_null() { out.push(None); continue; }
                out.push(Some(ResidentBatchItem {
                    file_id:value.get("file_id").and_then(Value::as_u64).unwrap_or(0) as u32,
                    hot_cache_hit:value.get("hot_cache_hit").and_then(Value::as_bool).unwrap_or(false),
                }));
            }
            Ok(ns)
        }
        pub fn find_batch(&mut self, names:&[&str]) -> io::Result<ResidentBatchFindResult> {
            let mut results=Vec::with_capacity(names.len());
            let server_ns=self.find_batch_reuse(names,&mut results)?;
            Ok(ResidentBatchFindResult{results,server_ns})
        }
        pub fn resolve(&mut self, file_id:u32) -> io::Result<Option<ResidentResolveResult>> {
            let v=self.request(&json!({"op":"resolve","file_id":file_id}))?;
            let ns=v.get("server_ns").and_then(Value::as_u64).unwrap_or(0);
            let Some(r)=v.get("result").filter(|v| !v.is_null()) else { return Ok(None); };
            Ok(Some(ResidentResolveResult { file_id, name:r.get("name").and_then(Value::as_str).unwrap_or("").to_owned(), path:r.get("path").and_then(Value::as_str).unwrap_or("").to_owned(), server_ns:ns }))
        }
        pub fn stats(&mut self) -> io::Result<ResidentStats> {
            let v=self.request(&json!({"op":"stats"}))?;
            Ok(ResidentStats { ok:true, version:RESIDENT_VERSION.to_string(), transport:"tcp/json-fallback".into(), root:v.get("root").and_then(Value::as_str).unwrap_or("").to_owned(), files:v.get("files").and_then(Value::as_u64).unwrap_or(0) as usize, directories:v.get("directories").and_then(Value::as_u64).unwrap_or(0) as usize, compact_payload_bytes:v.get("compact_payload_bytes").and_then(Value::as_u64).unwrap_or(0) as usize, hot_names:v.get("hot_names").and_then(Value::as_u64).unwrap_or(0) as usize, hot_entries:v.get("hot_entries").and_then(Value::as_u64).unwrap_or(0) as usize, hot_slots:v.get("hot_slots").and_then(Value::as_u64).unwrap_or(0) as usize, content_indexing:false, resident_core:true })
        }
        pub fn shutdown(&mut self)->io::Result<bool>{ let _=self.request(&json!({"op":"shutdown"}))?; Ok(true) }
    }
}

pub use platform::{run_daemon, ResidentClient};

#[cfg(test)]
mod tests {
    use super::resident_addr;
    #[test]
    fn resident_endpoint_is_nonempty() { assert!(!resident_addr().is_empty()); }
}
