//! Authenticated local transport; guarded writes are dispatched by the application.
use crate::dwarf::types::ExtendType;
use crate::model::VariablePool;
use crate::ui::plugin::{FrameData, MemRWPlugin};
use eframe::egui;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
#[cfg(test)]
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Default)]
pub struct ReadState {
    observed: HashMap<usize, Observation>,
}
struct Observation {
    identity: String,            // Name, address and type identity.
    sequence: Option<u64>,       // Latest-value publication sequence.
    bytes: [u8; 8],              // Last observed raw bytes.
    observed_at_ms: Option<u64>, // Host observation time, not sample time.
    sample_time_s: Option<f64>,  // Acquisition-relative stream timestamp.
    source: &'static str,        // latest or stream.
}

impl ReadState {
    pub fn observe(&mut self, pool: &VariablePool, data: &FrameData) {
        self.observed.retain(|id, _| pool.contains(*id));
        for var in pool.iter() {
            let identity = format!(
                "{}:{}:{:?}:{}",
                var.name, var.address, var.ext_type, var.size
            );
            let latest = var.latest.load();
            let stream = data.get(&var.id).and_then(|batch| batch.last());
            if latest.is_none() && stream.is_none() {
                continue;
            }
            if self
                .observed
                .get(&var.id)
                .is_some_and(|old| old.identity != identity)
            {
                self.observed.remove(&var.id);
            }
            let old = self.observed.entry(var.id).or_insert_with(|| Observation {
                identity,
                sequence: None,
                bytes: [0; 8],
                observed_at_ms: None,
                sample_time_s: None,
                source: "latest",
            });
            let latest_changed = latest.is_some_and(|(sequence, _)| old.sequence != Some(sequence));
            let use_latest = latest_changed && (var.latest_readers > 0 || stream.is_none());
            if use_latest {
                let (sequence, bytes) = latest.unwrap();
                // First sight of an old LatestValue does not prove its freshness.
                if old.sequence.is_some() {
                    old.observed_at_ms = Some(now_ms());
                }
                old.sequence = Some(sequence);
                old.bytes = bytes;
                old.sample_time_s = None;
                old.source = "latest";
            } else if let Some((timestamp, bytes)) = stream {
                old.bytes = *bytes;
                old.observed_at_ms = Some(now_ms());
                old.sample_time_s = Some(*timestamp);
                old.source = "stream";
                if let Some((sequence, _)) = latest {
                    old.sequence = Some(sequence);
                }
            }
        }
    }

    pub fn query(
        &self,
        method: &str,
        args: &Value,
        pool: &VariablePool,
        plugins: &[Box<dyn MemRWPlugin>],
        status: &Value,
    ) -> Result<Value, String> {
        if !args.is_object() {
            return Err("arguments must be an object".into());
        }
        let generated_at = now_ms();
        match method {
            "get_status" => Ok(status.clone()),
            "list_variables" => {
                let offset = integer(args, "offset", 0, 0, usize::MAX)?;
                let limit = integer(args, "limit", 100, 1, 200)?;
                let query = match args.get("query") {
                    Some(Value::String(text)) if text.len() <= 1024 => text.to_lowercase(),
                    None => String::new(),
                    _ => return Err("query must be a string up to 1024 bytes".into()),
                };
                let annotations: HashMap<_, _> = plugins
                    .iter()
                    .flat_map(|p| p.readonly_annotations())
                    .map(|(id, label, comment)| (id, (label, comment)))
                    .collect();
                let mut total = 0;
                let mut variables = Vec::new();
                for var in pool.iter() {
                    let (label, comment) = annotations.get(&var.id).cloned().unwrap_or_default();
                    let address = format!("0x{:08X}", var.address);
                    if !query.is_empty()
                        && ![&var.name, &label, &comment, &address]
                            .iter()
                            .any(|s| s.to_lowercase().contains(&query))
                    {
                        continue;
                    }
                    if total >= offset && variables.len() < limit {
                        variables.push(json!({"id":var.id,"name":var.name,"label":label,"comment":comment,
                            "address":address,"type":var.ext_type,"size_bytes":var.size,
                            "stream_enabled":var.stream_readers>0,"table_enabled":var.latest_readers>0}));
                    }
                    total += 1;
                }
                Ok(
                    json!({"generated_at_ms":generated_at,"total":total,"offset":offset,"variables":variables,
                    "next_offset":if offset.saturating_add(limit)<total {Some(offset+limit)} else {None}}),
                )
            }
            "read_variables" => {
                let ids = args
                    .get("ids")
                    .and_then(Value::as_array)
                    .ok_or("ids must be an array")?;
                if ids.is_empty() || ids.len() > 128 {
                    return Err("ids must contain 1..128 variable IDs".into());
                }
                let live = status["connected"] == true
                    && status["running"] == true
                    && status["hardware_busy"] != true;
                let mut values = Vec::new();
                for id in ids {
                    let id = id
                        .as_u64()
                        .and_then(|id| usize::try_from(id).ok())
                        .ok_or("invalid variable ID")?;
                    let var = pool.get(id).ok_or_else(|| {
                        format!("unknown variable ID {id}; refresh list_variables")
                    })?;
                    let observation = self.observed.get(&id);
                    let value = observation
                        .map(|o| decode(&o.bytes, &var.ext_type))
                        .unwrap_or(Value::Null);
                    let observed_at_ms = observation.and_then(|o| o.observed_at_ms);
                    let age = observed_at_ms.map(|t| generated_at.saturating_sub(t));
                    let stale = !live || age.is_none_or(|age| age > 1000);
                    values.push(json!({"id":id,"name":var.name,"type":var.ext_type,"value":value,
                        "value_text":if value.is_null() {None} else {Some(value.to_string())},
                        "raw_hex":observation.map(|o| o.bytes[..(var.size as usize).min(8)].iter().map(|b|format!("{b:02X}")).collect::<String>()),
                        "has_sample":observation.is_some(),"stale":stale,"observed_at_ms":observed_at_ms,"observation_age_ms":age,
                        "sample_time_s":observation.and_then(|o| o.sample_time_s),"source":observation.map(|o|o.source)}));
                }
                Ok(
                    json!({"generated_at_ms":generated_at,"status":status,"variables":values,
                    "time_note":"observed_at_ms is host UI observation time, not precise MCU sampling time; first-sight latest values have unknown age. Non-finite floats have null value; inspect raw_hex."}),
                )
            }
            "get_history" => {
                let id = integer(args, "variable_id", usize::MAX, 0, usize::MAX)?;
                if pool.get(id).is_none() {
                    return Err("unknown variable_id".into());
                }
                let limit = integer(args, "limit", 1000, 1, 10000)?;
                let seconds = args
                    .get("seconds")
                    .map(|v| v.as_f64().unwrap_or(f64::NAN))
                    .unwrap_or(10.0);
                if !seconds.is_finite() || seconds <= 0.0 || seconds > 86400.0 {
                    return Err("seconds must be in (0,86400]".into());
                }
                for plugin in plugins {
                    if let Some(history) = plugin.readonly_history(id, seconds, limit) {
                        return Ok(
                            json!({"variable_id":id,"generated_at_ms":generated_at,"status":status,"history":history,
                            "time_note":"time_s is relative to the acquisition timer, not UTC. Historical points do not prove current target state."}),
                        );
                    }
                }
                Err("variable is not present in Chart; add it manually to collect history".into())
            }
            _ => Err("unsupported operation; read-only queries only".into()),
        }
    }
}

fn integer(
    args: &Value,
    name: &str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize, String> {
    let value = match args.get(name) {
        None => default,
        Some(v) => v
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| format!("invalid {name}"))?,
    };
    if value < min || value > max {
        return Err(format!("{name} out of range"));
    }
    Ok(value)
}

pub(crate) fn decode(bytes: &[u8; 8], kind: &ExtendType) -> Value {
    match kind {
        ExtendType::U8 => json!(bytes[0]),
        ExtendType::I8 => json!(bytes[0] as i8),
        ExtendType::U16 => json!(u16::from_le_bytes(bytes[..2].try_into().unwrap())),
        ExtendType::I16 => json!(i16::from_le_bytes(bytes[..2].try_into().unwrap())),
        ExtendType::U32 => json!(u32::from_le_bytes(bytes[..4].try_into().unwrap())),
        ExtendType::I32 => json!(i32::from_le_bytes(bytes[..4].try_into().unwrap())),
        ExtendType::U64 => json!(u64::from_le_bytes(*bytes)),
        ExtendType::I64 => json!(i64::from_le_bytes(*bytes)),
        ExtendType::Float => json!(f32::from_le_bytes(bytes[..4].try_into().unwrap())),
        ExtendType::Double => json!(f64::from_le_bytes(*bytes)),
        ExtendType::Other => Value::Null,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    token: String,    // Per-session local credential.
    method: String,   // Explicitly allowed read operation.
    arguments: Value, // Bounded query parameters.
}
struct Pending {
    cancelled: Arc<AtomicBool>,
    deadline: std::time::Instant,
    request: Request,
    response: mpsc::SyncSender<Value>,
}

pub struct ReadService {
    receiver: mpsc::Receiver<Pending>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    path: PathBuf,
    pub session_id: String,
}

impl ReadService {
    pub fn start(ctx: egui::Context) -> Result<Self, String> {
        let token = random_token().map_err(|e| format!("create credential: {e}"))?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let pid = std::process::id();
        let session_id = format!("{pid}-{}", &token[..12]);
        let base = if cfg!(windows) {
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        } else {
            std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state")))
        }
        .ok_or("local application data directory unavailable")?;
        let directory = base.join("MemRW3/ai-readonly");
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let path = directory.join(format!("session-{session_id}.json"));
        // The credential lives in the current user's application-data directory.
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|e| e.to_string())?;
        let metadata = json!({"session_id":session_id,"pid":pid,"port":port,"token":token,"started_at_ms":now_ms(),"server_version":"2.2.0","supports_guarded_write":true});
        if let Err(error) = file.write_all(metadata.to_string().as_bytes()) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(error.to_string());
        }
        drop(file);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let (sender, receiver) = mpsc::sync_channel::<Pending>(8);
        let spawn = std::thread::Builder::new()
            .name("memrw3-readonly".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = serve(stream, &token, &sender, &ctx, &stopping);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        Err(_) => break,
                    }
                }
            });
        let worker = match spawn {
            Ok(worker) => worker,
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                return Err(error.to_string());
            }
        };
        Ok(Self {
            receiver,
            stop,
            worker: Some(worker),
            path,
            session_id,
        })
    }

    pub fn process(
        &self,
        mut query: impl FnMut(&str, &Value, std::time::Instant) -> Result<Value, String>,
    ) {
        // Bounded processing also works when acquisition is paused.
        for _ in 0..8 {
            let Ok(pending) = self.receiver.try_recv() else {
                break;
            };
            process_pending(pending, &mut query);
        }
    }
}

fn process_pending(
    pending: Pending,
    mut query: impl FnMut(&str, &Value, std::time::Instant) -> Result<Value, String>,
) {
    if pending.cancelled.load(Ordering::Acquire) || std::time::Instant::now() >= pending.deadline {
        let _ = pending
            .response
            .try_send(json!({"ok":false,"error":"request expired before execution"}));
        return;
    }
    let response = match query(
        &pending.request.method,
        &pending.request.arguments,
        pending.deadline,
    ) {
        Ok(data) => json!({"ok":true,"data":data}),
        Err(error) => json!({"ok":false,"error":error}),
    };
    let _ = pending.response.try_send(response);
}

impl Drop for ReadService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::fs::remove_file(&self.path);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve(
    mut stream: TcpStream,
    token: &str,
    sender: &mpsc::SyncSender<Pending>,
    ctx: &egui::Context,
    stop: &AtomicBool,
) -> Result<(), String> {
    stream.set_nonblocking(false).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_millis(250)))
        .map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    let mut raw = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if stop.load(Ordering::Acquire) || std::time::Instant::now() >= deadline {
            return Err("request read cancelled or timed out".into());
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err("incomplete request".into()),
            Ok(count) => {
                let count = chunk[..count]
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(count, |i| i + 1);
                raw.extend_from_slice(&chunk[..count]);
                if raw.len() > 65536 {
                    return Err("request too large".into());
                }
                if raw.last() == Some(&b'\n') {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    let parsed = serde_json::from_slice::<Request>(&raw);
    let result = match parsed {
        Ok(request) if token_matches(&request.token, token) => {
            if ![
                "get_status",
                "list_variables",
                "read_variables",
                "get_history",
                "get_write_permissions",
                "write_variable",
                "get_write_result",
            ]
            .contains(&request.method.as_str())
            {
                json!({"ok":false,"error":"operation not supported"})
            } else {
                let (reply, receiver) = mpsc::sync_channel(1);
                let cancelled = Arc::new(AtomicBool::new(false));
                if sender
                    .try_send(Pending {
                        request,
                        cancelled: cancelled.clone(),
                        deadline: std::time::Instant::now() + Duration::from_secs(2),
                        response: reply,
                    })
                    .is_err()
                {
                    json!({"ok":false,"error":"query queue busy"})
                } else {
                    ctx.request_repaint();
                    let started = std::time::Instant::now();
                    loop {
                        match receiver.recv_timeout(Duration::from_millis(20)) {
                            Ok(value) => break value,
                            Err(mpsc::RecvTimeoutError::Disconnected) => {
                                break json!({"ok":false,"error":"UI closed"});
                            }
                            Err(_)
                                if stop.load(Ordering::Acquire)
                                    || started.elapsed() > Duration::from_secs(2) =>
                            {
                                cancelled.store(true, Ordering::Release);
                                break json!({"ok":false,"error":"UI query timed out; for a write, query its request_key result before any retry"});
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
        }
        _ => json!({"ok":false,"error":"invalid request or authentication failed"}),
    };
    serde_json::to_writer(&mut stream, &result).map_err(|e| e.to_string())?;
    stream.write_all(b"\n").map_err(|e| e.to_string())
}

fn token_matches(input: &str, expected: &str) -> bool {
    input.len() == expected.len()
        && input
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

fn random_token() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        unsafe extern "system" {
            fn BCryptGenRandom(
                handle: *mut std::ffi::c_void,
                buffer: *mut u8,
                len: u32,
                flags: u32,
            ) -> i32;
        }
        // SAFETY: null handle with BCRYPT_USE_SYSTEM_PREFERRED_RNG; buffer is writable for exactly 32 bytes.
        let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), bytes.as_mut_ptr(), 32, 2) };
        if status < 0 {
            return Err(std::io::Error::other("system random generator failed"));
        }
    }
    #[cfg(not(windows))]
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwarf::types::{ExtendConfig, ExtendType};

    fn pool() -> VariablePool {
        let mut pool = VariablePool::default();
        pool.add(&ExtendConfig {
            name: "motor.count".into(),
            address: 0x20000000,
            ext_type: ExtendType::U64,
            size: 8,
            array_index: None,
            array_count: None,
        });
        pool
    }

    #[test]
    fn exact_integer_and_no_sample_are_distinct() {
        let pool = pool();
        let reader = ReadState::default();
        let response = reader
            .query(
                "read_variables",
                &serde_json::json!({"ids":[0]}),
                &pool,
                &[],
                &serde_json::json!({"connected":false,"running":false}),
            )
            .unwrap();
        assert!(response["variables"][0]["value"].is_null());
        assert!(response["variables"][0]["stale"].as_bool().unwrap());
        assert_eq!(
            decode(&u64::MAX.to_le_bytes(), &ExtendType::U64),
            serde_json::json!(u64::MAX)
        );
    }

    #[test]
    fn unsupported_writes_and_unbounded_reads_are_rejected() {
        let pool = pool();
        let reader = ReadState::default();
        for (method, args) in [
            ("write_variable", serde_json::json!({"id":0,"value":1})),
            ("read_variables", serde_json::json!({"ids":[]})),
            ("list_variables", serde_json::json!({"limit":100000})),
            (
                "get_history",
                serde_json::json!({"variable_id":0,"seconds":-1}),
            ),
        ] {
            assert!(
                reader
                    .query(method, &args, &pool, &[], &serde_json::json!({}))
                    .is_err()
            );
        }
    }
}

#[cfg(test)]
mod extended_tests {
    use super::*;
    use crate::dwarf::types::ExtendConfig;
    fn pool() -> VariablePool {
        let mut pool = VariablePool::default();
        pool.add(&ExtendConfig {
            name: "speed".into(),
            address: 0x20000000,
            ext_type: ExtendType::Float,
            size: 4,
            array_index: None,
            array_count: None,
        });
        pool
    }
    #[test]
    fn first_sight_has_unknown_age_then_updates_and_pause_stales() {
        let pool = pool();
        let var = pool.get(0).unwrap();
        var.latest.store(1u64.to_le_bytes());
        let mut state = ReadState::default();
        state.observe(&pool, &FrameData::default());
        let args = json!({"ids":[0]});
        let live = json!({"connected":true,"running":true});
        let first = state
            .query("read_variables", &args, &pool, &[], &live)
            .unwrap();
        assert_eq!(first["variables"][0]["stale"], true);
        assert!(first["variables"][0]["observation_age_ms"].is_null());
        var.latest.store(2u64.to_le_bytes());
        state.observe(&pool, &FrameData::default());
        assert_eq!(
            state
                .query("read_variables", &args, &pool, &[], &live)
                .unwrap()["variables"][0]["stale"],
            false
        );
        let paused = json!({"connected":true,"running":false});
        assert_eq!(
            state
                .query("read_variables", &args, &pool, &[], &paused)
                .unwrap()["variables"][0]["stale"],
            true
        );
        let bad_float = decode(
            &u64::from(f32::NAN.to_bits()).to_le_bytes(),
            &ExtendType::Float,
        );
        assert!(bad_float.is_null());
    }
    #[test]
    fn stream_values_preserve_relative_time_and_search_paginates() {
        let pool = pool();
        let mut state = ReadState::default();
        let data = HashMap::from([(0, vec![(12.5, u64::from(3.5f32.to_bits()).to_le_bytes())])]);
        state.observe(&pool, &data);
        let result = state
            .query(
                "read_variables",
                &json!({"ids":[0]}),
                &pool,
                &[],
                &json!({"connected":true,"running":true}),
            )
            .unwrap();
        assert_eq!(result["variables"][0]["value"], 3.5);
        assert_eq!(result["variables"][0]["sample_time_s"], 12.5);
        let list = state
            .query(
                "list_variables",
                &json!({"query":"0x20000000","limit":1}),
                &pool,
                &[],
                &json!({}),
            )
            .unwrap();
        assert_eq!(list["total"], 1);
        assert_eq!(
            state
                .query(
                    "list_variables",
                    &json!({"offset":1}),
                    &pool,
                    &[],
                    &json!({})
                )
                .unwrap()["variables"],
            json!([])
        );
    }
    #[test]
    fn loopback_authentication_and_read_only_dispatch() {
        let service = ReadService::start(egui::Context::default()).unwrap();
        let path = service.path.clone();
        let endpoint: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let port = endpoint["port"].as_u64().unwrap() as u16;
        let token = endpoint["token"].as_str().unwrap().to_owned();
        let call = |token: &str, method: &str| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            writeln!(
                stream,
                "{}",
                json!({"token":token,"method":method,"arguments":{}})
            )
            .unwrap();
            let mut text = String::new();
            BufReader::new(stream).read_line(&mut text).unwrap();
            serde_json::from_str::<Value>(&text).unwrap()
        };
        assert_eq!(call("incorrect", "get_status")["ok"], false);
        assert_eq!(call(&token, "write_memory")["ok"], false);
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            writeln!(
                stream,
                "{}",
                json!({"token":token,"method":"get_status","arguments":{}})
            )
            .unwrap();
            let mut text = String::new();
            BufReader::new(stream).read_line(&mut text).unwrap();
            serde_json::from_str::<Value>(&text).unwrap()
        });
        for _ in 0..100 {
            service.process(|method, _, _| {
                assert_eq!(method, "get_status");
                Ok(json!({"readonly":true,"connected":false}))
            });
            if client.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(client.join().unwrap()["data"]["readonly"], true);
        drop(service);
        assert!(!path.exists());
    }
}

#[cfg(test)]
mod review_regression_tests {
    use super::*;
    use crate::dwarf::types::ExtendConfig;
    #[test]
    fn disabled_table_does_not_hide_new_stream_samples() {
        let mut pool = VariablePool::default();
        let id = pool.add(&ExtendConfig {
            name: "speed".into(),
            address: 0x20000000,
            ext_type: ExtendType::U64,
            size: 8,
            array_index: None,
            array_count: None,
        });
        pool.get(id).unwrap().latest.store(1u64.to_le_bytes());
        let mut state = ReadState::default();
        state.observe(&pool, &FrameData::default());
        let data = HashMap::from([(id, vec![(1.0, 2u64.to_le_bytes())])]);
        state.observe(&pool, &data);
        state.observe(&pool, &FrameData::default());
        let result = state
            .query(
                "read_variables",
                &json!({"ids":[id]}),
                &pool,
                &[],
                &json!({"connected":true,"running":true}),
            )
            .unwrap();
        assert_eq!(result["variables"][0]["value"], 2);
        assert_eq!(result["variables"][0]["source"], "stream");
    }
    #[test]
    fn slow_unauthenticated_client_cannot_block_service_shutdown() {
        let service = ReadService::start(egui::Context::default()).unwrap();
        let endpoint: Value =
            serde_json::from_slice(&std::fs::read(&service.path).unwrap()).unwrap();
        let port = endpoint["port"].as_u64().unwrap() as u16;
        let started = std::sync::Arc::new(AtomicBool::new(false));
        let notified = started.clone();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let until = std::time::Instant::now() + Duration::from_millis(1100);
            while std::time::Instant::now() < until {
                if stream.write_all(b"x").is_err() {
                    break;
                }
                notified.store(true, Ordering::Release);
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        while !started.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(30));
        let closing = std::time::Instant::now();
        drop(service);
        let elapsed = closing.elapsed();
        client.join().unwrap();
        assert!(
            elapsed < Duration::from_millis(400),
            "shutdown took {elapsed:?}"
        );
    }
}

#[cfg(test)]
mod expiration_tests {
    use super::*;
    #[test]
    fn expired_mutation_is_not_dispatched_when_ui_recovers() {
        let service = ReadService::start(egui::Context::default()).unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        // Inject a queued request without any hardware or transport callback.
        let pending = Pending {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: std::time::Instant::now() - Duration::from_secs(1),
            request: Request {
                token: String::new(),
                method: "write_variable".into(),
                arguments: json!({}),
            },
            response: tx,
        };
        // The production receiver is private, so exercise the same expiry predicate directly.
        assert!(std::time::Instant::now() >= pending.deadline);
        let mut invoked = false;
        process_pending(pending, |_, _, _| {
            invoked = true;
            Ok(json!({}))
        });
        assert!(!invoked);
        assert_eq!(rx.recv().unwrap()["ok"], false);
        drop(service);
    }
}
