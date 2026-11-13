"""Generic PID trial/checkpoint orchestration. No direct hardware access or background writes."""
import contextlib
from decimal import Decimal, InvalidOperation
import json
import math
import os
import pathlib
import re
import struct
import tempfile
import time
import uuid

ROLES = ("kp", "ki", "kd", "target", "feedback", "output")
GAINS = ("kp", "ki", "kd")


def state_dir():
    base = os.environ.get("LOCALAPPDATA") if os.name == "nt" else os.environ.get("XDG_STATE_HOME", os.path.expanduser("~/.local/state"))
    if not base:
        raise ValueError("local state directory unavailable")
    return pathlib.Path(base) / "MemRW3" / "pid-sessions"


def number(value):
    if not isinstance(value, str) or not value.strip() or len(value) > 128:
        raise ValueError("numeric values must be strings of at most 128 characters")
    try:
        parsed = Decimal(value)
    except InvalidOperation:
        raise ValueError("invalid numeric value") from None
    if not parsed.is_finite():
        raise ValueError("non-finite numeric value forbidden")
    return parsed


def encode(value, kind):
    number(value)
    fmt = {"Float":"<f", "Double":"<d"}.get(kind)
    if fmt is None:
        raise ValueError("PID gains must be Float or Double")
    try:
        raw = struct.pack(fmt, float(value))
        rounded = struct.unpack(fmt, raw)[0]
    except (OverflowError, ValueError, struct.error):
        raise ValueError("PID value exceeds its floating-point type") from None
    if not math.isfinite(rounded):
        raise ValueError("PID value rounds to a non-finite float")
    return repr(rounded), raw.hex().upper()


def path_for(tuning_id):
    if not isinstance(tuning_id, str) or not re.fullmatch(r"[a-f0-9]{32}", tuning_id):
        raise ValueError("invalid tuning_id")
    return state_dir() / (tuning_id + ".json")


def save(state):
    path = path_for(state["tuning_id"])
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".pid-", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(state, stream, ensure_ascii=False, allow_nan=False, indent=2)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


@contextlib.contextmanager
def locked(tuning_id):
    path = path_for(tuning_id)
    lock = path.with_suffix(".lock")
    if not path.exists():
        raise ValueError("PID session does not exist")
    try:
        fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError:
        raise ValueError("PID session busy or interrupted; do not remove its lock before checking prior operation results") from None
    try:
        os.close(fd)
        if path.stat().st_size > 2 * 1024 * 1024:
            raise ValueError("oversized PID session file")
        state = json.loads(path.read_text(encoding="utf-8"))
        if state.get("tuning_id") != tuning_id or state.get("schema_version") != 1:
            raise ValueError("invalid PID session file")
        yield state
    finally:
        lock.unlink(missing_ok=True)


def query(backend, state, method, arguments=None):
    return backend(method, {**(arguments or {}), "session_id":state["session_id"]})


def metadata(backend, state, name):
    if not isinstance(name, str) or not name or len(name) > 1024:
        raise ValueError("variable name must contain 1..1024 characters")
    result = query(backend, state, "list_variables", {"query":name,"limit":200})
    matches = [v for v in result.get("variables", []) if v.get("name") == name]
    if len(matches) != 1:
        raise ValueError(f"variable name must resolve uniquely: {name}")
    return {key:matches[0][key] for key in ("id","name","address","type","size_bytes")}


def validate_status(status, session_id):
    if not isinstance(session_id, str) or not session_id:
        raise ValueError("software session identity is missing")
    if status.get("session_id") != session_id:
        raise ValueError("software session changed; create a new PID session")
    if status.get("connected") is not True or status.get("running") is not True:
        raise ValueError("PID tuning requires a connected, acquiring system")
    if status.get("hardware_busy") or status.get("connection_pending") or status.get("target_halted"):
        raise ValueError("system busy, transitioning or halted")


def permissions(backend, state):
    result = query(backend, state, "get_write_permissions")
    if result.get("enabled") is not True:
        raise ValueError("AI write interface is unavailable")
    epoch = result.get("permission_epoch")
    if type(epoch) is not int:
        raise ValueError("software lacks permission_epoch; use the PID online build")
    if "permission_epoch" in state and state["permission_epoch"] != epoch:
        raise ValueError("permissions, target or configuration changed; begin a new PID session")
    allowed = {v["variable_id"]:v for v in result.get("allowed_variables", []) if v.get("valid_bounds") is True}
    for role in GAINS:
        if state["bindings"][role]["id"] not in allowed:
            raise ValueError(f"PID gain is not a supported loaded numeric variable: {role}")
    return result, allowed


def read_live(backend, state):
    status = query(backend, state, "get_status")
    validate_status(status, state["session_id"])
    for item in state["bindings"].values():
        if metadata(backend, state, item["name"]) != item:
            raise ValueError("variable identity changed; create a new PID session")
    ids = sorted({item["id"] for item in state["bindings"].values()})
    response = query(backend, state, "read_variables", {"ids":ids})
    validate_status(response.get("status", {}), state["session_id"])
    values = {v["id"]:v for v in response.get("variables", [])}
    result = {}
    for role, item in state["bindings"].items():
        value = values.get(item["id"])
        if not value or value.get("stale") is not False or value.get("has_sample") is not True:
            raise ValueError(f"missing or stale sample: {item['name']}")
        age = value.get("observation_age_ms")
        if type(age) is not int or age < 0 or age > state["max_age_ms"]:
            raise ValueError(f"host observation too old or unknown: {item['name']}")
        number(value.get("value_text"))
        if not isinstance(value.get("raw_hex"), str):
            raise ValueError("raw sample missing")
        result[role] = value
    for guard in state["guards"]:
        actual, expected = number(result[guard["role"]]["value_text"]), number(guard["value"])
        accepted = {"eq":actual == expected,"le":actual <= expected,"ge":actual >= expected}[guard["op"]]
        if not accepted:
            raise ValueError(f"safety condition failed: {guard['name']}")
    return result


def gain_snapshot(values):
    return {role:{"value_text":values[role]["value_text"],"raw_hex":values[role]["raw_hex"]} for role in GAINS}


def same_gains(values, expected):
    return all(values[role]["raw_hex"] == expected[role]["raw_hex"] for role in GAINS)


def expected_trial(state):
    expected = {key:dict(value) for key,value in state["checkpoint"].items()}
    trial = state.get("trial")
    if trial and trial["state"] == "observing":
        expected[trial["gain"]] = {"value_text":trial["value"],"raw_hex":trial["raw_hex"]}
    return expected


def poll(backend, state):
    trial = state.get("trial")
    if not trial or trial["state"] not in ("pending", "rollback_pending", "submitting", "rollback_submitting"):
        return
    rollback = trial["state"].startswith("rollback_")
    intent = trial["rollback_intent"] if rollback else trial["apply_intent"]
    key = intent["request_key"]
    try:
        result = query(backend, state, "get_write_result", {"request_key":key})
    except (ValueError, OSError):
        # A timeout or missing record must not cause a new write or guessed rollback.
        return
    trial["native_result"] = result
    if result.get("request") != intent or result.get("permission_epoch") != state["permission_epoch"]:
        trial["state"] = "result_conflict"
        trial["error"] = "native result does not match the intended write; inspect manually"
        save(state)
        return
    status = result.get("state")
    if status == "verified":
        trial["state"] = "rolled_back" if rollback else "observing"
        trial["confirmed_at"] = time.time()
        trial["confirmed_monotonic"] = time.monotonic()
    elif status in ("rejected", "not_submitted", "uncertain"):
        trial["state"] = "rollback_" + status if rollback else status
    elif status == "pending":
        trial["state"] = "rollback_pending" if rollback else "pending"
    save(state)


def seconds(args, name, default):
    value = args.get(name, default)
    if isinstance(value, bool) or not isinstance(value, (float,int)) or not math.isfinite(value) or not 0.2 <= value <= 60:
        raise ValueError(f"{name} must be 0.2..60 seconds")
    return float(value)


def begin(args, backend):
    status = backend("get_status", {"session_id":args["session_id"]} if args.get("session_id") else {})
    sid = status.get("session_id")
    validate_status(status, sid)
    names = args.get("bindings")
    if not isinstance(names, dict) or set(names) != set(ROLES):
        raise ValueError("bindings must name kp, ki, kd, target, feedback and output")
    max_age_ms = args.get("max_age_ms",200)
    if type(max_age_ms) is not int or not 20 <= max_age_ms <= 1000:
        raise ValueError("max_age_ms must be 20..1000")
    state = {"schema_version":1,"tuning_id":uuid.uuid4().hex,"session_id":sid,
             "created_at":time.time(),"max_age_ms":max_age_ms,"state":"active","guards":[],"bindings":{},"history":[],"trial":None,
             "observe_seconds":seconds(args,"observe_seconds",2),"min_interval_seconds":seconds(args,"min_interval_seconds",1),
             "last_submission_at":0,"last_submission_monotonic":0}
    for role in ROLES:
        state["bindings"][role] = metadata(backend,state,names[role])
    if len({v["id"] for v in state["bindings"].values()}) != len(ROLES):
        raise ValueError("PID gains and signal roles must refer to distinct variables")
    steps = args.get("max_step")
    if not isinstance(steps, dict) or set(steps) != set(GAINS):
        raise ValueError("max_step must specify kp, ki and kd; no implicit physical limits")
    state["max_step"] = {}
    for role in GAINS:
        if state["bindings"][role]["type"] not in ("Float","Double"):
            raise ValueError("PID gain must be Float or Double; fixed-point scaling needs explicit adaptation")
        if state["bindings"][role]["size_bytes"] != {"Float":4,"Double":8}[state["bindings"][role]["type"]]:
            raise ValueError("PID gain type and byte size do not match")
        if number(steps[role]) <= 0:
            raise ValueError("max_step must be positive")
        state["max_step"][role] = steps[role]
    guards = args.get("guards", [])
    if not isinstance(guards,list) or len(guards)>16:
        raise ValueError("at most 16 safety conditions are supported")
    for i, guard in enumerate(guards):
        if not isinstance(guard,dict) or set(guard)!={"name","op","value"} or guard["op"] not in ("eq","le","ge"):
            raise ValueError("guard requires name, op (eq/le/ge) and decimal string value")
        number(guard["value"])
        role = f"guard_{i}"
        state["bindings"][role] = metadata(backend,state,guard["name"])
        state["guards"].append({**guard,"role":role})
    allowed, _ = permissions(backend,state)
    if allowed.get("pending", 0):
        raise ValueError("another write is in flight; cannot take a PID baseline")
    state["permission_epoch"] = allowed["permission_epoch"]
    values = read_live(backend,state)
    state["initial"] = gain_snapshot(values)
    state["checkpoint"] = gain_snapshot(values)
    save(state)
    return {"tuning_id":state["tuning_id"],"session_id":sid,"checkpoint":state["checkpoint"],
            "observe_seconds":state["observe_seconds"],"min_interval_seconds":state["min_interval_seconds"],
            "note":"Checkpoint is the initial operator-selected baseline, not a proof of stability. No MCU write performed."}


def dispatch_write(backend, state, trial, rollback):
    # Durable checkpoint and pending intent must precede every hardware-affecting request.
    save(state)
    gain = trial["gain"]
    value = state["checkpoint"][gain]["value_text"] if rollback else trial["value"]
    expected = trial["value"] if rollback else state["checkpoint"][gain]["value_text"]
    key = trial["rollback_key"] if rollback else trial["request_key"]
    intent = {"variable_id":state["bindings"][gain]["id"], "value":value, "expected_value":expected,
              "permission_epoch":state["permission_epoch"], "request_key":f"pid.{state['tuning_id']}.{key}"}
    trial["rollback_intent" if rollback else "apply_intent"] = intent
    save(state)
    try:
        response = query(backend,state,"write_variable",intent)
        trial["state"] = "rollback_pending" if rollback else "pending"
        trial["native_result"] = response
    except (ValueError,OSError) as error:
        trial["transport_error"] = str(error)
        # Remain in submitting state: query the same operation key; never resend automatically.
    save(state)
    return {"tuning_id":state["tuning_id"],"trial":trial,"checkpoint":state["checkpoint"]}


def call(method, args, backend):
    try:
        if method == "pid_begin":
            return begin(args, backend)
        with locked(args.get("tuning_id")) as state:
            if args.get("session_id") is not None and args["session_id"] != state["session_id"]:
                raise ValueError("explicit software session does not match the saved PID session")
            if state["state"] != "active":
                raise ValueError("PID session is closed")
            poll(backend,state)
            if method == "pid_snapshot":
                values = read_live(backend,state)
                permit, _ = permissions(backend,state)
                save(state)
                return {"tuning_id":state["tuning_id"],"trial":state["trial"],"checkpoint":state["checkpoint"],
                        "values":{role:values[role] for role in ROLES},"guards":{g["name"]:values[g["role"]] for g in state["guards"]},
                        "permission_epoch":permit["permission_epoch"],"matches_expected_gains":same_gains(values,expected_trial(state)),"note":"The six reads are sequential software observations, not an atomic sample frame."}
            if method == "pid_end":
                if state.get("trial") and state["trial"]["state"] not in ("accepted","rolled_back","rejected","not_submitted"):
                    raise ValueError("trial unresolved; inspect/accept/rollback before ending session")
                state["state"]="closed";save(state)
                return {"state":"closed","note":"No MCU action; accepted parameters remain as they are."}
            values = read_live(backend,state)
            _, allowed = permissions(backend,state)
            trial = state.get("trial")
            if method == "pid_apply":
                gain, key = args.get("gain"), args.get("request_key")
                if gain not in GAINS:
                    raise ValueError("gain must be kp, ki or kd")
                value, raw = encode(args.get("value"),state["bindings"][gain]["type"])
                if trial and key == trial.get("request_key"):
                    if gain != trial["gain"] or raw != trial["raw_hex"]:
                        raise ValueError("request_key reused with different PID parameters")
                    return {"tuning_id":state["tuning_id"],"trial":trial}
                if key in used_keys(state):
                    raise ValueError("request_key already used in this PID session")
                if trial and trial["state"] not in ("accepted","rolled_back","rejected","not_submitted"):
                    raise ValueError("previous trial must be observed and accepted or rolled back first")
                if len(state["history"])>=100:
                    raise ValueError("PID trial history limit reached; create a new session")
                if not same_gains(values,state["checkpoint"]):
                    raise ValueError("PID gains changed outside this trial; do not overwrite, begin a new session")
                if time.monotonic()-state["last_submission_monotonic"]<state["min_interval_seconds"]:
                    raise ValueError("minimum tuning interval has not elapsed")
                target=number(value);previous=number(state["checkpoint"][gain]["value_text"])
                if abs(target-previous)>number(state["max_step"][gain]):
                    raise ValueError("PID change exceeds the configured per-trial max_step")
                rule=allowed[state["bindings"][gain]["id"]]
                if not number(rule["min"])<=target<=number(rule["max"]):
                    raise ValueError("PID target outside its numeric type limits")
                if raw==values[gain]["raw_hex"]:
                    raise ValueError("target equals current PID gain")
                validate_key(key)
                if trial:state["history"].append(trial)
                trial={"state":"submitting","gain":gain,"value":value,"raw_hex":raw,"request_key":key,"submitted_at":time.time()}
                state["trial"]=trial;state["last_submission_at"]=time.time();state["last_submission_monotonic"]=time.monotonic()
                return dispatch_write(backend,state,trial,False)
            if not trial:
                raise ValueError("there is no active PID trial")
            if method == "pid_accept":
                if trial["state"]!="observing":
                    raise ValueError("only a verified trial may be accepted")
                if time.monotonic()-trial["confirmed_monotonic"]<state["observe_seconds"]:
                    raise ValueError("observation interval has not elapsed")
                if not same_gains(values,expected_trial(state)):
                    raise ValueError("PID parameters changed since the verified trial")
                reason=args.get("reason")
                if not isinstance(reason,str) or not 8<=len(reason)<=2000:
                    raise ValueError("provide an evaluation reason of 8..2000 characters")
                state["checkpoint"]=gain_snapshot(values);trial["state"]="accepted";trial["reason"]=reason;save(state)
                return {"state":"accepted","checkpoint":state["checkpoint"],"note":"Accepted by caller; not an automatic stability certification."}
            if method == "pid_rollback":
                key=args.get("request_key");validate_key(key)
                if trial["state"] in ("rollback_pending","rollback_submitting","rolled_back") and key==trial.get("rollback_key"):
                    return {"tuning_id":state["tuning_id"],"trial":trial}
                if key in used_keys(state):
                    raise ValueError("rollback requires a new request_key; never reuse an apply key")
                if trial["state"]!="observing":
                    raise ValueError("rollback requires a verified trial; uncertain/submitting outcomes need manual inspection")
                if not same_gains(values,expected_trial(state)):
                    raise ValueError("external PID change detected; refusing to overwrite another writer")
                baseline=number(state["checkpoint"][trial["gain"]]["value_text"])
                rule=allowed[state["bindings"][trial["gain"]]["id"]]
                if not number(rule["min"])<=baseline<=number(rule["max"]):
                    raise ValueError("checkpoint is outside the current numeric type limits")
                trial["rollback_key"]=key;trial["state"]="rollback_submitting"
                return dispatch_write(backend,state,trial,True)
            raise ValueError("unknown PID tool")
    except (KeyError, TypeError, InvalidOperation, struct.error) as error:
        raise ValueError(f"invalid PID session or backend response: {error}") from None


def validate_key(key):
    if not isinstance(key,str) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,64}",key):
        raise ValueError("request_key must contain 1..64 ASCII key characters")


def used_keys(state):
    return {record[key] for record in [*state["history"], *([state["trial"]] if state.get("trial") else [])]
            for key in ("request_key", "rollback_key") if key in record}
