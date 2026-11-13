//! Guarded AI parameter writes; tests never use a probe.
use crate::dwarf::types::ExtendType;
use crate::model::VariablePool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn parse_scalar(text: &str, kind: &ExtendType) -> Result<u64, String> {
    let text = text.trim();
    macro_rules! signed {
        ($t:ty) => {{
            text.parse::<$t>()
                .map(|value| value as u64)
                .map_err(|_| "invalid signed integer".to_owned())
        }};
    }
    macro_rules! unsigned {
        ($t:ty) => {{
            text.parse::<$t>()
                .map(|value| value as u64)
                .map_err(|_| "invalid unsigned integer".to_owned())
        }};
    }
    match kind {
        ExtendType::I8 => signed!(i8),
        ExtendType::I16 => signed!(i16),
        ExtendType::I32 => signed!(i32),
        ExtendType::I64 => signed!(i64),
        ExtendType::U8 => unsigned!(u8),
        ExtendType::U16 => unsigned!(u16),
        ExtendType::U32 => unsigned!(u32),
        ExtendType::U64 => unsigned!(u64),
        ExtendType::Float => {
            let value = text.parse::<f32>().map_err(|_| "invalid float")?;
            if !value.is_finite() {
                return Err("non-finite float is forbidden".into());
            }
            Ok(u64::from(value.to_bits()))
        }
        ExtendType::Double => {
            let value = text.parse::<f64>().map_err(|_| "invalid double")?;
            if !value.is_finite() {
                return Err("non-finite double is forbidden".into());
            }
            Ok(value.to_bits())
        }
        ExtendType::Other => Err("only scalar numeric variables are writable".into()),
    }
}

fn type_bounds(kind: &ExtendType) -> (&'static str, &'static str) {
    match kind {
        ExtendType::I8 => ("-128", "127"),
        ExtendType::U8 => ("0", "255"),
        ExtendType::I16 => ("-32768", "32767"),
        ExtendType::U16 => ("0", "65535"),
        ExtendType::I32 => ("-2147483648", "2147483647"),
        ExtendType::U32 => ("0", "4294967295"),
        ExtendType::I64 => ("-9223372036854775808", "9223372036854775807"),
        ExtendType::U64 => ("0", "18446744073709551615"),
        ExtendType::Float => ("-3.4028234663852886e38", "3.4028234663852886e38"),
        ExtendType::Double => ("-1.7976931348623157e308", "1.7976931348623157e308"),
        ExtendType::Other => ("0", "0"),
    }
}

fn scalar_size(kind: &ExtendType) -> u32 {
    match kind {
        ExtendType::I8 | ExtendType::U8 => 1,
        ExtendType::I16 | ExtendType::U16 => 2,
        ExtendType::I32 | ExtendType::U32 | ExtendType::Float => 4,
        ExtendType::I64 | ExtendType::U64 | ExtendType::Double => 8,
        ExtendType::Other => 0,
    }
}
fn mask(size: u32) -> u64 {
    if size == 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    variable_id: usize,     // Loaded variable ID, never an address.
    value: String,          // Decimal value in the declared type.
    expected_value: String, // Required pre-write comparison value.
    #[serde(default)]
    permission_epoch: Option<u64>, // Optional caller authorization generation.
    request_key: String,    // Unique key for retry deduplication.
}

pub struct WriteSpec {
    pub request_id: u64,                // Probe worker request ID.
    pub address: u64,                   // Address resolved from the variable pool.
    pub size: u32,                      // Exact scalar byte size.
    pub expected: u64,                  // Expected raw low bits.
    pub value: u64,                     // New raw low bits.
    pub expires_at: std::time::Instant, // End-to-end request execution deadline.
    pub gate: Arc<AtomicBool>,          // Revocable submission-generation permission.
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteOutcome {
    pub before: Option<u64>,   // Raw pre-write read, if available.
    pub after: Option<u64>,    // Raw readback, if available.
    pub attempted: bool,       // A write operation was invoked.
    pub verified: bool,        // The immediate readback matched.
    pub error: Option<String>, // Failure or uncertainty explanation.
}
impl WriteOutcome {
    pub fn rejected(error: impl Into<String>) -> Self {
        Self {
            before: None,
            after: None,
            attempted: false,
            verified: false,
            error: Some(error.into()),
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub enum Access {
    Read,
    Write(u64),
}

/// Serialization is provided by ProbeWorker; the MCU may change data between accesses.
pub fn guarded_io(
    size: u32,
    expected: u64,
    value: u64,
    gate: &AtomicBool,
    expires_at: std::time::Instant,
    mut access: impl FnMut(Access) -> Result<u64, String>,
) -> WriteOutcome {
    if ![1, 2, 4, 8].contains(&size) {
        return WriteOutcome::rejected("invalid scalar size");
    }
    if !gate.load(Ordering::Acquire) || std::time::Instant::now() >= expires_at {
        return WriteOutcome::rejected("write permission revoked or request expired");
    }
    let before = match access(Access::Read) {
        Ok(value) => value & mask(size),
        Err(error) => return WriteOutcome::rejected(error),
    };
    let mut outcome = WriteOutcome {
        before: Some(before),
        after: None,
        attempted: false,
        verified: false,
        error: None,
    };
    if before != expected & mask(size) {
        outcome.error = Some("expected value mismatch; no write performed".into());
        return outcome;
    }
    if !gate.load(Ordering::Acquire) || std::time::Instant::now() >= expires_at {
        outcome.error =
            Some("write permission revoked or request expired; no write performed".into());
        return outcome;
    }
    outcome.attempted = true;
    if let Err(error) = access(Access::Write(value & mask(size))) {
        outcome.error = Some(format!(
            "write failed, outcome uncertain; do not retry automatically: {error}"
        ));
        return outcome;
    }
    match access(Access::Read) {
        Ok(after) => {
            outcome.after = Some(after & mask(size));
            outcome.verified = after & mask(size) == value & mask(size);
            if !outcome.verified {
                outcome.error = Some(
                    "write was attempted but readback differs; do not retry automatically".into(),
                );
            }
        }
        Err(error) => {
            outcome.error = Some(format!(
                "write was attempted but readback failed; do not retry automatically: {error}"
            ))
        }
    }
    outcome
}

pub struct WriteController {
    permission_epoch: u64, // Invalidates host tuning checkpoints.
    gate: Arc<AtomicBool>,
    operations: HashMap<String, Value>,
    pending: HashMap<u64, String>,
}
impl Default for WriteController {
    fn default() -> Self {
        Self {
            permission_epoch: 0,
            gate: Arc::new(AtomicBool::new(true)),
            operations: HashMap::new(),
            pending: HashMap::new(),
        }
    }
}
impl WriteController {
    pub fn enabled(&self) -> bool {
        self.gate.load(Ordering::Acquire)
    }

    pub fn invalidate(&mut self) {
        self.gate.store(false, Ordering::Release);
        self.permission_epoch = self.permission_epoch.wrapping_add(1);
        self.gate = Arc::new(AtomicBool::new(true));
    }

    pub fn shutdown(&mut self) {
        self.gate.store(false, Ordering::Release);
        self.permission_epoch = self.permission_epoch.wrapping_add(1);
    }

    pub fn permissions(&self, pool: &VariablePool) -> Value {
        let variables: Vec<_> = pool.iter().filter_map(|var| {
            if scalar_size(&var.ext_type) == 0 || scalar_size(&var.ext_type) != var.size { return None; }
            let (min, max) = type_bounds(&var.ext_type);
            Some(json!({"variable_id":var.id,"name":var.name,"type":var.ext_type,"min":min,"max":max,
                "valid_bounds":true,"bounds_source":"numeric_type_limits"}))
        }).collect();
        json!({"enabled":self.enabled(),"access_mode":"all_loaded_numeric","permission_epoch":self.permission_epoch,
            "allowed_variables":variables,"pending":self.pending.len(),"record_count":self.operations.len(),"record_limit":256})
    }
    pub fn result(&self, args: &Value) -> Result<Value, String> {
        let key = args
            .get("request_key")
            .and_then(Value::as_str)
            .ok_or("request_key is required")?;
        self.operations.get(key).cloned().ok_or_else(|| {
            "unknown request_key; do not infer whether an older process performed a write".into()
        })
    }
    pub fn submit(
        &mut self,
        args: &Value,
        pool: &VariablePool,
        status: &Value,
        request_id: u64,
        mut send: impl FnMut(WriteSpec) -> Result<(), String>,
    ) -> Result<Value, String> {
        let request: WriteArgs = serde_json::from_value(args.clone()).map_err(|e| e.to_string())?;
        if request.request_key.is_empty()
            || request.request_key.len() > 128
            || !request
                .request_key
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            return Err(
                "request_key must be 1..128 ASCII letters, digits, dash, dot or underscore".into(),
            );
        }
        if let Some(record) = self.operations.get(&request.request_key) {
            if record["request"] != *args {
                return Err("request_key already used with different arguments".into());
            }
            return Ok(record.clone());
        }
        if request
            .permission_epoch
            .is_some_and(|epoch| epoch != self.permission_epoch)
        {
            return Err("permission generation changed; no write submitted".into());
        }
        if !self.enabled() {
            return Err("application is shutting down; no write submitted".into());
        }
        if status["connected"] != true
            || status["hardware_busy"] == true
            || status["connection_pending"] == true
        {
            return Err("target disconnected, transitioning or busy".into());
        }
        if !self.pending.is_empty() {
            return Err("another AI write is pending; query its result first".into());
        }
        if self.operations.len() >= 256 {
            return Err("operation record limit reached; no write submitted".into());
        }
        let var = pool.get(request.variable_id).ok_or("unknown variable ID")?;
        if scalar_size(&var.ext_type) == 0 || scalar_size(&var.ext_type) != var.size {
            return Err("variable type and size do not match".into());
        }
        let bits = parse_scalar(&request.value, &var.ext_type)?;
        let expected = parse_scalar(&request.expected_value, &var.ext_type)?;
        let mut record = json!({"request_key":request.request_key,"request":args,"variable_name":var.name,"variable_type":var.ext_type,
            "address":format!("0x{:08X}",var.address),"submitted_at_ms":crate::ai_readonly::now_ms(),"permission_epoch":self.permission_epoch,
            "state":"pending","probe_request_id":request_id,"warning":"poll get_write_result; never retry an uncertain write with a new key"});
        // Reserve the idempotency record before handing the command to the worker.
        self.operations
            .insert(request.request_key.clone(), record.clone());
        match send(WriteSpec {
            request_id,
            address: var.address,
            size: var.size,
            expected: expected & mask(var.size),
            value: bits & mask(var.size),
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(2),
            gate: self.gate.clone(),
        }) {
            Ok(()) => {
                self.pending.insert(request_id, request.request_key);
            }
            Err(error) => {
                record["state"] = json!("not_submitted");
                record["error"] = json!(error);
                self.operations.insert(request.request_key, record.clone());
            }
        }
        Ok(record)
    }
    pub fn finish(&mut self, request_id: u64, outcome: WriteOutcome) {
        let Some(key) = self.pending.remove(&request_id) else {
            return;
        };
        if let Some(record) = self.operations.get_mut(&key) {
            record["state"] = json!(if outcome.verified {
                "verified"
            } else if outcome.attempted {
                "uncertain"
            } else {
                "rejected"
            });
            record["finished_at_ms"] = json!(crate::ai_readonly::now_ms());
            if let Ok(kind) = serde_json::from_value::<ExtendType>(record["variable_type"].clone())
            {
                record["before_value"] = outcome
                    .before
                    .map(|bits| crate::ai_readonly::decode(&bits.to_le_bytes(), &kind))
                    .unwrap_or(Value::Null);
                record["after_value"] = outcome
                    .after
                    .map(|bits| crate::ai_readonly::decode(&bits.to_le_bytes(), &kind))
                    .unwrap_or(Value::Null);
            }
            record["outcome"] = json!(outcome);
        }
    }
}

impl Drop for WriteController {
    fn drop(&mut self) {
        self.gate.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwarf::types::{ExtendConfig, ExtendType};
    fn pool() -> VariablePool {
        let mut pool = VariablePool::default();
        pool.add(&ExtendConfig {
            name: "motor.kp".into(),
            address: 0x20000000,
            ext_type: ExtendType::Float,
            size: 4,
            array_index: None,
            array_count: None,
        });
        pool
    }
    #[test]
    fn scalar_parsing_preserves_integer_limits_and_rejects_nonfinite() {
        assert_eq!(
            parse_scalar("18446744073709551615", &ExtendType::U64).unwrap(),
            u64::MAX
        );
        assert!(parse_scalar("18446744073709551616", &ExtendType::U64).is_err());
        for text in ["NaN", "inf", "-inf"] {
            assert!(parse_scalar(text, &ExtendType::Float).is_err());
        }
    }
    #[test]
    fn default_access_and_duplicate_requests_dispatch_only_once() {
        let pool = pool();
        let mut controller = WriteController::default();
        let args =
            json!({"variable_id":0,"value":"123.5","expected_value":"1","request_key":"test-1"});
        let status = json!({"connected":true,"hardware_busy":false});
        let mut calls = 0;
        for id in 1..=2 {
            assert!(
                controller
                    .submit(&args, &pool, &status, id, |_| {
                        calls += 1;
                        Ok(())
                    })
                    .is_ok()
            );
        }
        assert_eq!(calls, 1);
        let other =
            json!({"variable_id":0,"value":"456","expected_value":"1","request_key":"test-2"});
        assert!(
            controller
                .submit(&other, &pool, &status, 3, |_| panic!("overlapping write"))
                .is_err()
        );
    }
}

#[cfg(test)]
mod execution_tests {
    use super::*;
    use crate::dwarf::types::ExtendConfig;
    #[test]
    fn compare_and_revocation_never_invoke_write() {
        let gate = AtomicBool::new(true);
        let mut accesses = 0;
        let result = guarded_io(
            4,
            1,
            2,
            &gate,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
            |action| {
                accesses += 1;
                assert!(matches!(action, Access::Read));
                Ok(3)
            },
        );
        assert_eq!(accesses, 1);
        assert!(!result.attempted);
        let result = guarded_io(
            4,
            1,
            2,
            &gate,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
            |action| {
                assert!(matches!(action, Access::Read));
                gate.store(false, Ordering::Release);
                Ok(1)
            },
        );
        assert!(!result.attempted);
        let result = guarded_io(
            4,
            1,
            2,
            &gate,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
            |_| panic!("revoked request accessed memory"),
        );
        assert!(!result.attempted);
    }
    #[test]
    fn verified_write_and_uncertain_readback_are_distinct() {
        let gate = AtomicBool::new(true);
        let mut value = 1;
        let result = guarded_io(
            1,
            1,
            255,
            &gate,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
            |action| match action {
                Access::Read => Ok(value),
                Access::Write(next) => {
                    value = next;
                    Ok(0)
                }
            },
        );
        assert!(result.verified && result.attempted);
        assert_eq!(result.after, Some(255));
        let mut calls = 0;
        let result = guarded_io(
            4,
            1,
            2,
            &gate,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
            |_| {
                calls += 1;
                if calls == 3 {
                    Err("readback failed".into())
                } else {
                    Ok(1)
                }
            },
        );
        assert!(result.attempted && !result.verified);
        assert_eq!(calls, 3);
        let mut calls = 0;
        let result = guarded_io(
            4,
            1,
            2,
            &gate,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
            |_| {
                calls += 1;
                if calls == 2 {
                    Err("write bus error".into())
                } else {
                    Ok(1)
                }
            },
        );
        assert!(result.attempted && !result.verified);
        assert_eq!(calls, 2);
    }
    #[test]
    fn completed_duplicate_and_changed_key_do_not_resubmit() {
        let mut pool = VariablePool::default();
        pool.add(&ExtendConfig {
            name: "kp".into(),
            address: 0x20000000,
            ext_type: ExtendType::Float,
            size: 4,
            array_index: None,
            array_count: None,
        });
        let mut controller = WriteController::default();
        let args = json!({"variable_id":0,"value":"2","expected_value":"1","request_key":"op1"});
        let status = json!({"connected":true});
        controller
            .submit(&args, &pool, &status, 8, |_| Ok(()))
            .unwrap();
        let mut different = args.clone();
        different["value"] = json!("3");
        assert!(
            controller
                .submit(&different, &pool, &status, 9, |_| panic!(
                    "duplicate dispatched"
                ))
                .is_err()
        );
        let mut another = args.clone();
        another["request_key"] = json!("op2");
        assert!(
            controller
                .submit(&another, &pool, &status, 9, |_| panic!(
                    "parallel dispatched"
                ))
                .is_err()
        );
        controller.finish(
            8,
            WriteOutcome {
                before: Some(1f32.to_bits() as u64),
                after: Some(2f32.to_bits() as u64),
                attempted: true,
                verified: true,
                error: None,
            },
        );
        controller.invalidate();
        let result = controller
            .submit(&args, &pool, &status, 10, |_| {
                panic!("completed duplicate dispatched")
            })
            .unwrap();
        assert_eq!(result["state"], "verified");
        assert_eq!(result["after_value"], 2.0);
    }
    #[test]
    fn invalid_values_and_offline_requests_never_dispatch() {
        let mut pool = VariablePool::default();
        pool.add(&ExtendConfig {
            name: "kp".into(),
            address: 0x20000000,
            ext_type: ExtendType::Float,
            size: 4,
            array_index: None,
            array_count: None,
        });
        let mut controller = WriteController::default();
        for (value, connected) in [("1e100", true), ("NaN", true), ("2", false)] {
            let args = json!({"variable_id":0,"value":value,"expected_value":"1","request_key":format!("op-{connected}-{value}")});
            assert!(
                controller
                    .submit(
                        &args,
                        &pool,
                        &json!({"connected":connected}),
                        1,
                        |_| panic!("invalid request dispatched")
                    )
                    .is_err()
            );
        }
    }
}

#[cfg(test)]
mod teardown_tests {
    use super::*;
    #[test]
    fn dropping_controller_revokes_shared_write_generation() {
        let controller = WriteController::default();
        let gate = controller.gate.clone();
        drop(controller);
        assert!(!gate.load(Ordering::Acquire));
    }
}

#[cfg(test)]
mod expiry_io_tests {
    use super::*;
    #[test]
    fn expired_queue_and_expiry_during_compare_never_write() {
        let gate = AtomicBool::new(true);
        let result = guarded_io(
            4,
            1,
            2,
            &gate,
            std::time::Instant::now() - std::time::Duration::from_millis(1),
            |_| panic!("expired command touched target"),
        );
        assert!(!result.attempted);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2);
        let result = guarded_io(4, 1, 2, &gate, deadline, |action| {
            assert!(matches!(action, Access::Read));
            std::thread::sleep(std::time::Duration::from_millis(5));
            Ok(1)
        });
        assert!(!result.attempted);
    }
}

#[cfg(test)]
mod permission_epoch_tests {
    use super::*;
    #[test]
    fn target_change_cancels_old_requests_but_default_access_remains_available() {
        let mut controller = WriteController::default();
        let old_epoch = controller.permission_epoch;
        let old_gate = controller.gate.clone();
        controller.invalidate();
        assert_ne!(controller.permission_epoch, old_epoch);
        assert!(!old_gate.load(Ordering::Acquire));
        assert!(controller.enabled());
        controller.shutdown();
        assert!(!controller.enabled());
    }
}

#[cfg(test)]
mod epoch_submission_tests {
    use super::*;
    use crate::dwarf::types::ExtendConfig;
    #[test]
    fn submit_compares_caller_epoch_before_dispatch() {
        let mut pool = VariablePool::default();
        pool.add(&ExtendConfig {
            name: "kp".into(),
            address: 0x20000000,
            ext_type: ExtendType::Float,
            size: 4,
            array_index: None,
            array_count: None,
        });
        let mut controller = WriteController::default();
        let mut args = json!({"variable_id":0,"value":"2","expected_value":"1","request_key":"epoch-bound","permission_epoch":controller.permission_epoch+1});
        assert!(
            controller
                .submit(&args, &pool, &json!({"connected":true}), 1, |_| panic!(
                    "old epoch dispatched"
                ))
                .is_err()
        );
        args["permission_epoch"] = json!(controller.permission_epoch);
        assert!(
            controller
                .submit(&args, &pool, &json!({"connected":true}), 2, |_| Ok(()))
                .is_ok()
        );
    }
}

#[cfg(test)]
mod default_access_tests {
    use super::*;
    use crate::dwarf::types::ExtendConfig;
    #[test]
    fn all_loaded_numeric_parameters_are_writable_without_ui_authorization() {
        let mut pool = VariablePool::default();
        pool.add(&ExtendConfig {
            name: "kp".into(),
            address: 0x20000000,
            ext_type: ExtendType::Float,
            size: 4,
            array_index: None,
            array_count: None,
        });
        pool.add(&ExtendConfig {
            name: "counter".into(),
            address: 0x20000004,
            ext_type: ExtendType::U64,
            size: 8,
            array_index: None,
            array_count: None,
        });
        let mut controller = WriteController::default();
        assert!(controller.enabled());
        let permissions = controller.permissions(&pool);
        assert_eq!(
            permissions["allowed_variables"].as_array().unwrap().len(),
            2
        );
        let args = json!({"variable_id":0,"value":"123.5","expected_value":"1","request_key":"default-access"});
        assert!(
            controller
                .submit(&args, &pool, &json!({"connected":true}), 1, |_| Ok(()))
                .is_ok()
        );
    }
}

#[cfg(test)]
mod numeric_access_tests {
    use super::*;
    use crate::dwarf::types::ExtendConfig;
    #[test]
    fn reported_type_bounds_are_valid_in_the_declared_type() {
        for kind in [
            ExtendType::I8,
            ExtendType::I16,
            ExtendType::I32,
            ExtendType::I64,
            ExtendType::U8,
            ExtendType::U16,
            ExtendType::U32,
            ExtendType::U64,
            ExtendType::Float,
            ExtendType::Double,
        ] {
            let (min, max) = type_bounds(&kind);
            assert!(
                parse_scalar(min, &kind).is_ok(),
                "invalid minimum for {kind:?}"
            );
            assert!(
                parse_scalar(max, &kind).is_ok(),
                "invalid maximum for {kind:?}"
            );
        }
    }
    #[test]
    fn unsupported_or_mismatched_variables_are_not_reported_as_writable() {
        let mut pool = VariablePool::default();
        for (name, kind, size) in [
            ("group", ExtendType::Other, 4),
            ("bad_float", ExtendType::Float, 8),
        ] {
            pool.add(&ExtendConfig {
                name: name.into(),
                address: 0x20000000,
                ext_type: kind,
                size,
                array_index: None,
                array_count: None,
            });
        }
        let mut controller = WriteController::default();
        assert!(
            controller.permissions(&pool)["allowed_variables"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        for id in 0..2 {
            let args = json!({"variable_id":id,"value":"1","expected_value":"0","request_key":format!("bad-{id}")});
            assert!(
                controller
                    .submit(
                        &args,
                        &pool,
                        &json!({"connected":true}),
                        id as u64,
                        |_| panic!("invalid scalar dispatched")
                    )
                    .is_err()
            );
        }
    }
}
