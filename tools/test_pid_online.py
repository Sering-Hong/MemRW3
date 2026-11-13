import importlib.util
import pathlib
import tempfile
import unittest
from unittest import mock

spec=importlib.util.spec_from_file_location("pid_online",pathlib.Path(__file__).with_name("pid_online.py"))
pid=importlib.util.module_from_spec(spec)
spec.loader.exec_module(pid)

class Backend:
    def __init__(self):
        self.values={"kp":"1.0","ki":"0.1","kd":"0.0","target":"10.0","feedback":"9.0","output":"2.0","fault":"0"}
        self.names=list(self.values)
        self.writes=[]
        self.results={}
        self.stale=False
        self.connected=True
        self.epoch=1
    def __call__(self,method,args):
        if method=="get_status":return {"session_id":"test-session","connected":self.connected,"running":True,"hardware_busy":False,"connection_pending":False}
        if method=="list_variables":
            name=args["query"]
            return {"variables":[{"id":self.names.index(name),"name":name,"address":hex(0x20000000+self.names.index(name)*4),"type":"Float","size_bytes":4}]}
        if method=="read_variables":
            return {"status":self("get_status",{}),"variables":[{"id":i,"value_text":self.values[self.names[i]],"raw_hex":pid.encode(self.values[self.names[i]],"Float")[1],"stale":self.stale,"has_sample":True,"observation_age_ms":0} for i in args["ids"]]}
        if method=="get_write_permissions":
            return {"enabled":True,"permission_epoch":self.epoch,"allowed_variables":[{"variable_id":i,"min":"0","max":"20","valid_bounds":True} for i in range(3)]}
        if method=="write_variable":
            if args.get("permission_epoch")!=self.epoch:raise ValueError("epoch mismatch")
            self.writes.append(dict(args));self.values[self.names[args["variable_id"]]]=args["value"]
            self.results[args["request_key"]]={"state":"verified","finished_at_ms":0,"permission_epoch":args.get("permission_epoch"),"request":{k:v for k,v in args.items() if k!="session_id"}}
            return {"state":"pending"}
        if method=="get_write_result":return self.results[args["request_key"]]
        raise AssertionError(method)

class PidTests(unittest.TestCase):
    def setUp(self):
        self.directory=tempfile.TemporaryDirectory()
        self.patch=mock.patch.object(pid,"state_dir",return_value=pathlib.Path(self.directory.name));self.patch.start()
        self.backend=Backend()
        self.args={"bindings":{k:k for k in ["kp","ki","kd","target","feedback","output"]},"max_step":{"kp":"0.5","ki":"0.1","kd":"0.1"},"observe_seconds":0.2,"min_interval_seconds":0.2,
            "guards":[{"name":"fault","op":"eq","value":"0"}]}
    def tearDown(self):self.patch.stop();self.directory.cleanup()
    def begin(self):return pid.call("pid_begin",self.args,self.backend)["tuning_id"]
    def test_apply_and_rollback_restore_checkpoint(self):
        key=self.begin()
        pid.call("pid_apply",{"tuning_id":key,"gain":"kp","value":"1.25","request_key":"apply-1"},self.backend)
        snapshot=pid.call("pid_snapshot",{"tuning_id":key},self.backend)
        self.assertEqual(snapshot["trial"]["state"],"observing")
        self.assertEqual(len(self.backend.writes),1)
        pid.call("pid_rollback",{"tuning_id":key,"request_key":"undo-1"},self.backend)
        snapshot=pid.call("pid_snapshot",{"tuning_id":key},self.backend)
        self.assertEqual(snapshot["trial"]["state"],"rolled_back")
        self.assertEqual(float(self.backend.values["kp"]),1.0)
    def test_stale_fault_and_large_steps_do_not_write(self):
        key=self.begin()
        for stale,fault,value in [(True,"0","1.1"),(False,"1","1.1"),(False,"0","2")]:
            self.backend.stale=stale;self.backend.values["fault"]=fault
            with self.assertRaises(ValueError):pid.call("pid_apply",{"tuning_id":key,"gain":"kp","value":value,"request_key":"attempt"},self.backend)
        self.assertEqual(self.backend.writes,[])

    # Regression cases for native result and authorization identity.
    def test_rollback_cannot_reuse_apply_key(self):
        key=self.begin()
        pid.call("pid_apply",{"tuning_id":key,"gain":"kp","value":"1.25","request_key":"same"},self.backend)
        pid.call("pid_snapshot",{"tuning_id":key},self.backend)
        with self.assertRaises(ValueError):
            pid.call("pid_rollback",{"tuning_id":key,"request_key":"same"},self.backend)
        self.assertEqual(len(self.backend.writes),1)
    def test_native_request_binds_the_permission_epoch(self):
        key=self.begin()
        pid.call("pid_apply",{"tuning_id":key,"gain":"kp","value":"1.25","request_key":"bound"},self.backend)
        self.assertEqual(self.backend.writes[0].get("permission_epoch"),self.backend.epoch)

    def test_external_gain_change_prevents_rollback(self):
        key=self.begin()
        pid.call("pid_apply",{"tuning_id":key,"gain":"kp","value":"1.25","request_key":"change"},self.backend)
        pid.call("pid_snapshot",{"tuning_id":key},self.backend)
        self.backend.values["ki"]="0.5"
        snapshot=pid.call("pid_snapshot",{"tuning_id":key},self.backend)
        self.assertFalse(snapshot["matches_expected_gains"])
        with self.assertRaises(ValueError):pid.call("pid_rollback",{"tuning_id":key,"request_key":"undo"},self.backend)
        self.assertEqual(len(self.backend.writes),1)
    def test_permission_change_and_wrong_instance_prevent_writes(self):
        key=self.begin();self.backend.epoch+=1
        with self.assertRaises(ValueError):pid.call("pid_apply",{"tuning_id":key,"gain":"kp","value":"1.25","request_key":"change"},self.backend)
        with self.assertRaises(ValueError):pid.call("pid_snapshot",{"tuning_id":key,"session_id":"other-instance"},self.backend)
        self.assertEqual(self.backend.writes,[])
    def test_wrong_native_record_never_reports_rollback_success(self):
        key=self.begin()
        pid.call("pid_apply",{"tuning_id":key,"gain":"kp","value":"1.25","request_key":"change"},self.backend)
        pid.call("pid_snapshot",{"tuning_id":key},self.backend)
        pid.call("pid_rollback",{"tuning_id":key,"request_key":"undo"},self.backend)
        result=self.backend.results[self.backend.writes[-1]["request_key"]]
        result["request"]={"wrong":"intent"}
        snapshot=pid.call("pid_snapshot",{"tuning_id":key},self.backend)
        self.assertEqual(snapshot["trial"]["state"],"result_conflict")
    def test_checkpoint_exists_before_dispatch_and_duplicate_never_resends(self):
        key=self.begin();backend=self.backend
        def checked(method,args):
            if method=="write_variable":
                state=pid.json.loads(pid.path_for(key).read_text(encoding="utf-8"))
                self.assertEqual(state["trial"]["state"],"submitting")
                self.assertEqual(state["trial"]["apply_intent"]["request_key"],args["request_key"])
            return backend(method,args)
        args={"tuning_id":key,"gain":"kp","value":"1.25","request_key":"once"}
        pid.call("pid_apply",args,checked);pid.call("pid_apply",args,checked)
        self.assertEqual(len(backend.writes),1)
    def test_uncertain_write_is_not_blindly_rolled_back(self):
        key=self.begin()
        pid.call("pid_apply",{"tuning_id":key,"gain":"kp","value":"1.25","request_key":"change"},self.backend)
        self.backend.results[self.backend.writes[0]["request_key"]]["state"]="uncertain"
        snapshot=pid.call("pid_snapshot",{"tuning_id":key},self.backend)
        self.assertEqual(snapshot["trial"]["state"],"uncertain")
        with self.assertRaises(ValueError):pid.call("pid_rollback",{"tuning_id":key,"request_key":"undo"},self.backend)
        self.assertEqual(len(self.backend.writes),1)
    def test_observe_accept_and_end_without_extra_writes(self):
        key=self.begin()
        pid.call("pid_apply",{"tuning_id":key,"gain":"kp","value":"1.25","request_key":"change"},self.backend)
        pid.call("pid_snapshot",{"tuning_id":key},self.backend)
        with self.assertRaises(ValueError):pid.call("pid_accept",{"tuning_id":key,"reason":"Measured response is acceptable"},self.backend)
        with mock.patch.object(pid.time,"monotonic",return_value=pid.time.monotonic()+1):
            result=pid.call("pid_accept",{"tuning_id":key,"reason":"Measured response is acceptable"},self.backend)
        self.assertEqual(result["state"],"accepted")
        self.assertEqual(pid.call("pid_end",{"tuning_id":key},self.backend)["state"],"closed")
        self.assertEqual(len(self.backend.writes),1)
    def test_file_lock_and_invalid_session_id_block_concurrency(self):
        key=self.begin()
        with pid.locked(key):
            with self.assertRaises(ValueError):pid.call("pid_snapshot",{"tuning_id":key},self.backend)
        with self.assertRaises(ValueError):pid.call("pid_snapshot",{"tuning_id":"../elsewhere"},self.backend)
    def test_transport_failure_keeps_intent_and_does_not_retry(self):
        key=self.begin();counter=[];backend=self.backend
        def failed(method,args):
            if method=="write_variable":counter.append(args);raise ValueError("transport unavailable")
            if method=="get_write_result":raise ValueError("unknown operation")
            return backend(method,args)
        args={"tuning_id":key,"gain":"kp","value":"1.25","request_key":"once"}
        result=pid.call("pid_apply",args,failed)
        self.assertEqual(result["trial"]["state"],"submitting")
        pid.call("pid_apply",args,failed)
        self.assertEqual(len(counter),1)
        with self.assertRaises(ValueError):pid.call("pid_rollback",{"tuning_id":key,"request_key":"undo"},failed)

    def test_age_limit_rejects_delayed_host_observations(self):
        key=self.begin();backend=self.backend
        def old(method,args):
            result=backend(method,args)
            if method=="read_variables":
                for value in result["variables"]:value["observation_age_ms"]=300
            return result
        with self.assertRaises(ValueError):pid.call("pid_apply",{"tuning_id":key,"gain":"kp","value":"1.25","request_key":"old"},old)
        self.assertEqual(self.backend.writes,[])

if __name__=="__main__":unittest.main()
