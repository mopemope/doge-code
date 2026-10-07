"""No paid network: conservative reservation and loopback regression coverage."""
import importlib.util
import json
import tempfile
import sys
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
spec = importlib.util.spec_from_file_location("budget_common", SCRIPTS / "agent_eval_common.py")
common = importlib.util.module_from_spec(spec)
spec.loader.exec_module(common)


class BudgetManifestTests(unittest.TestCase):
    def test_unknown_model_cannot_be_assumed_cheap(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            (root / "cases.json").write_text("[]")
            (root / "config.toml").write_text("mcp_servers=[]\n")
            manifest = {"schema_version":1,"base_ref":"HEAD","cases":"cases.json",
                        "trials":1,"model":"unknown-cheap-model","provider":"openai-compatible",
                        "environment_id":"fixture","timeout_seconds":30,
                        "variants":[{"name":"test","agent_command":["/bin/true"],"config":"config.toml"}],
                        "api_budget":{"max_cost_micro_usd":5000000,"max_requests":128,
                                      "max_requests_per_run":32,"max_output_tokens":4096,
                                      "input_nano_usd_per_token":150,"output_nano_usd_per_token":600,
                                      "pricing_reviewed_on":"2026-10-07"}}
            path=root/"manifest.json"
            path.write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, "model"):
                common.load_manifest(path)

import datetime
import http.client
import os
import threading
from unittest.mock import patch
import agent_eval_api_budget as budget_module

MODEL = "gpt-4o-mini-2024-07-18"


def limits(requests=128, per_run=32, output=4096):
    return {"max_cost_micro_usd":5000000,"max_requests":requests,
            "max_requests_per_run":per_run,"max_output_tokens":output,
            "input_nano_usd_per_token":150,"output_nano_usd_per_token":600,
            "pricing_reviewed_on":datetime.datetime.now(datetime.timezone.utc).date().isoformat(),
            "model":MODEL,"context_tokens":128000,"reservation_nano_usd":21657600,
            "planned_upper_nano_usd":21657600 * requests}


def request(**kwargs):
    return dict({"model":MODEL,"messages":[{"role":"user","content":"test"}]}, **kwargs)


def response(finish="stop", usage=True):
    body={"model":MODEL,"service_tier":"default","choices":[{"index":0,"finish_reason":finish,
          "message":{"role":"assistant","content":"local response"}}]}
    if usage:
        body["usage"]={"prompt_tokens":100,"completion_tokens":10,"total_tokens":110}
    return json.dumps(body).encode()


class ReservationTests(unittest.TestCase):
    def test_full_context_bound_is_not_discounted_by_tiny_reported_usage(self):
        seen=[]
        guard=budget_module.ApiBudgetGuard(limits(), "not-a-provider-key",
            upstream=lambda req:(seen.append(req) or (200,response())))
        for _ in range(4):
            guard.begin_run()
            for _ in range(32):
                guard.request(request())
            guard.end_run()
        self.assertEqual(len(seen),128)
        self.assertEqual(guard.snapshot()["reserved_upper_nano_usd"],2772172800)
        guard.begin_run()
        with self.assertRaisesRegex(budget_module.BudgetStopped,"request_limit"):
            guard.request(request())
        self.assertEqual(len(seen),128)
        self.assertIsNone(guard.snapshot()["actual_cost"])
        self.assertEqual(guard.snapshot()["observed_prompt_tokens"],12800)
        self.assertTrue(all(req["max_completion_tokens"]==4096 for req in seen))

    def test_smaller_explicit_caps_are_preserved_but_conflicts_never_forward(self):
        for field in ["max_tokens","max_completion_tokens"]:
            seen=[]
            guard=budget_module.ApiBudgetGuard(limits(),"dummy",upstream=lambda req:(seen.append(req) or (200,response())))
            guard.begin_run();guard.request(request(**{field:1024}))
            self.assertEqual(seen[0][field],1024)
            self.assertEqual(guard.snapshot()["reserved_upper_nano_usd"],21657600)
        for changes in [{"max_completion_tokens":5000},{"max_tokens":True},
                        {"max_tokens":100,"max_completion_tokens":100},
                        {"max_output_tokens":100},{"n":2},{"service_tier":"fast"},
                        {"stream":True},{"model":"gpt-6-astra"},
                        {"messages":[{"role":"user","content":[{"type":"image_url","image_url":"x"}]}]}]:
            seen=[]
            guard=budget_module.ApiBudgetGuard(limits(),"dummy",upstream=lambda req:(seen.append(req) or (200,response())))
            guard.begin_run()
            with self.assertRaises(budget_module.BudgetStopped):guard.request(request(**changes))
            self.assertEqual(seen,[])
            self.assertEqual(guard.snapshot()["reserved_upper_nano_usd"],0)

    def test_standard_tier_is_forced_and_unknown_response_tier_stops(self):
        for tier in (None, "priority", "auto", "flex"):
            seen = []
            body = json.loads(response())
            body["service_tier"] = tier
            guard = budget_module.ApiBudgetGuard(limits(), "dummy",
                upstream=lambda req: (seen.append(req) or (200, json.dumps(body).encode())))
            guard.begin_run()
            with self.assertRaisesRegex(budget_module.BudgetStopped, "service_tier"):
                guard.request(request())
            self.assertEqual(seen[0]["service_tier"], "default")
            with self.assertRaises(budget_module.BudgetStopped):
                guard.request(request())
            self.assertEqual(len(seen), 1)
            self.assertEqual(guard.snapshot()["reserved_upper_nano_usd"], 21657600)

    def test_uncertainty_is_not_refunded_or_retried(self):
        invalids=[(429,b"busy"),(503,b"error"),(200,b"not JSON"),
                  (200,response(usage=False)),(200,json.dumps({"model":"wrong"}).encode())]
        for status,body in invalids:
            seen=[]
            guard=budget_module.ApiBudgetGuard(limits(),"dummy",upstream=lambda req:(seen.append(req) or (status,body)))
            guard.begin_run()
            with self.assertRaises(budget_module.BudgetStopped):guard.request(request())
            with self.assertRaises(budget_module.BudgetStopped):guard.request(request())
            self.assertEqual(len(seen),1)
            self.assertEqual(guard.snapshot()["reserved_upper_nano_usd"],21657600)
        def timeout(_req):raise TimeoutError("never reflect this body or key")
        guard=budget_module.ApiBudgetGuard(limits(),"dummy",upstream=timeout)
        guard.begin_run()
        with self.assertRaisesRegex(budget_module.BudgetStopped,"upstream_transport_or_timeout"):
            guard.request(request())
        self.assertEqual(guard.snapshot()["requests"],1)

    def test_invalid_usage_and_output_limit_stop_future_calls(self):
        for mutate in [lambda x:x.update(prompt_tokens=128001),
                       lambda x:x.update(completion_tokens=4097),
                       lambda x:x.update(total_tokens=0),
                       lambda x:x.update(prompt_tokens=True),
                       lambda x:x.update(completion_tokens=-1)]:
            body=json.loads(response());mutate(body["usage"])
            guard=budget_module.ApiBudgetGuard(limits(),"dummy",upstream=lambda _: (200,json.dumps(body).encode()))
            guard.begin_run()
            with self.assertRaisesRegex(budget_module.BudgetStopped,"unknown_or_invalid_usage"):guard.request(request())
        guard=budget_module.ApiBudgetGuard(limits(),"dummy",upstream=lambda _: (200,response(finish="length")))
        guard.begin_run();guard.request(request())
        self.assertEqual(guard.stopped,"output_limit")
        with self.assertRaises(budget_module.BudgetStopped):guard.request(request())
        self.assertEqual(guard.snapshot()["requests"],1)

    def test_http_bridge_rejects_wrong_routes_and_keeps_original_key_out_of_child(self):
        seen=[]
        guard=budget_module.ApiBudgetGuard(limits(),"parent-provider-secret",upstream=lambda req:(seen.append(req) or (200,response()))).start()
        try:
            guard.begin_run()
            env=guard.environment({"OPENAI_API_KEY":"parent-provider-secret","PATH":"x"})
            self.assertNotEqual(env["OPENAI_API_KEY"],"parent-provider-secret")
            port=guard._server.server_port
            connection=http.client.HTTPConnection("127.0.0.1",port,timeout=5)
            connection.request("POST","/v1/responses",json.dumps(request()),headers={"Authorization":"Bearer wrong-local-token"})
            self.assertEqual(connection.getresponse().status,403);connection.close()
            connection=http.client.HTTPConnection("127.0.0.1",port,timeout=5)
            connection.request("POST","/v1/chat/completions",json.dumps(request()),headers={"Authorization":"Bearer "+env["OPENAI_API_KEY"]})
            result=connection.getresponse();self.assertEqual(result.status,200);result.read();connection.close()
            self.assertEqual(len(seen),1)
            self.assertNotIn("parent-provider-secret",json.dumps(guard.snapshot()))
        finally:guard.close()

    def test_overlapping_requests_do_not_queue_paid_work(self):
        started,finish=threading.Event(),threading.Event()
        def upstream(_req):
            started.set();self.assertTrue(finish.wait(3));return 200,response()
        guard=budget_module.ApiBudgetGuard(limits(),"dummy",upstream=upstream)
        guard.begin_run()
        worker=threading.Thread(target=lambda:guard.request(request()))
        worker.start();self.assertTrue(started.wait(3))
        try:
            with self.assertRaisesRegex(budget_module.BudgetStopped,"concurrent_request"):guard.request(request())
        finally:finish.set();worker.join(3)
        self.assertEqual(guard.snapshot()["requests"],1)
        self.assertEqual(guard.stopped,"concurrent_request")
        with self.assertRaises(budget_module.BudgetStopped):guard.request(request())

    def test_authenticated_malformed_local_request_stops_later_paid_work(self):
        seen=[]
        guard=budget_module.ApiBudgetGuard(limits(),"dummy",upstream=lambda req:(seen.append(req) or (200,response()))).start()
        try:
            guard.begin_run()
            env=guard.environment({})
            connection=http.client.HTTPConnection("127.0.0.1",guard._server.server_port,timeout=5)
            connection.request("POST","/v1/chat/completions",b"{broken",headers={"Authorization":"Bearer "+env["OPENAI_API_KEY"]})
            self.assertEqual(connection.getresponse().status,403);connection.close()
            self.assertEqual(guard.stopped,"invalid_local_request")
            with self.assertRaises(budget_module.BudgetStopped):guard.request(request())
            self.assertEqual(seen,[])
        finally:guard.close()

from test_agent_evals import HarnessCase, changed_dir, git, runner

RAW_KEYS = {"max_cost_micro_usd","max_requests","max_requests_per_run","max_output_tokens",
            "input_nano_usd_per_token","output_nano_usd_per_token","pricing_reviewed_on"}


class PriceValidationTests(unittest.TestCase):
    def test_known_profile_and_unknown_prices_endpoint_or_date_fail_closed(self):
        with tempfile.TemporaryDirectory() as name:
            config=Path(name)/"eval.toml"
            config.write_text('base_url="https://api.openai.com/v1"\nmcp_servers=[]\n[llm]\nmax_retries=0\n')
            variants=[{"config":config,"agent_command":["/bin/true","--base-url","https://api.openai.com/v1"]}]
            raw={key:limits()[key] for key in RAW_KEYS}
            parsed=budget_module.validate_budget(raw,MODEL,"openai-compatible",variants)
            self.assertEqual(parsed["planned_upper_nano_usd"],2772172800)
            self.assertLessEqual(parsed["planned_upper_nano_usd"],5000000000)
            for changes in [{"input_nano_usd_per_token":0},{"output_nano_usd_per_token":599},
                            {"max_requests":231},{"max_output_tokens":16385},
                            {"max_cost_micro_usd":True},{"pricing_reviewed_on":"2000-01-01"}]:
                with self.assertRaises(ValueError):
                    budget_module.validate_budget(dict(raw,**changes),MODEL,"openai-compatible",variants)
            with self.assertRaises(ValueError):budget_module.validate_budget(raw,MODEL,"openai",variants)
            variants[0]["agent_command"]=["/bin/true","--base-url","https://custom.invalid/v1"]
            with self.assertRaises(ValueError):budget_module.validate_budget(raw,MODEL,"openai-compatible",variants)


GUARDED_AGENT = r'''#!/usr/bin/env python3
import json, os, sys, urllib.request, urllib.error
from pathlib import Path
args=sys.argv[1:]
phase="probe" if args==["--version"] else "evidence" if args[:2]==["session","evidence"] else "agent"
with open(os.environ["DUMMY_PHASE_LOG"],"a") as out:
    out.write(json.dumps({"phase":phase,"backend_key_seen":os.environ.get("OPENAI_API_KEY")=="dummy-parent-secret"})+"\n")
if phase=="probe":print("guarded dummy 1");sys.exit(0)
if phase=="evidence":print('{"status":"dummy"}');sys.exit(0)
url=args[args.index("--base-url")+1]+"/chat/completions"
model=args[args.index("--model")+1]
request=urllib.request.Request(url,data=json.dumps({"model":model,"messages":[{"role":"user","content":"fixture"}]}).encode(),headers={"Authorization":"Bearer "+os.environ["OPENAI_API_KEY"],"Content-Type":"application/json"})
try:
    with urllib.request.urlopen(request,timeout=5) as response: result=json.load(response)
except urllib.error.HTTPError:
    print('{"success":false,"status":"error"}');sys.exit(1)
session=Path('.doge/sessions/guarded-fixture');session.mkdir(parents=True,exist_ok=True)
(session/'session.json').write_text('{}')
usage=result['usage']
usage.update(attempts=1,usage_records=1,unknown_usage_attempts=0,all_tracked_attempts_reported=True)
print(json.dumps({'success':True,'status':'completed','response':'dummy complete','usage':usage}))
'''


class GuardedHarnessTests(HarnessCase):
    def prepared(self, case_count=2):
        repo=self.make_repo()
        fake=self.root/"guarded-dgc"
        fake.write_text(GUARDED_AGENT);fake.chmod(0o755)
        config=self.root/"eval.toml"
        config.write_text('base_url="https://api.openai.com/v1"\nmcp_servers=[]\n[llm]\nmax_retries=0\n')
        check={"name":"verify-env","argv":[sys.executable,"-c", "import os; assert not os.environ.get('OPENAI_API_KEY', '').startswith('dummy-parent-')"]}
        cases=self.write_cases([self.base_case(case_id=f"case-{i}",post_checks=[check]) for i in range(case_count)])
        manifest=self.write_manifest(fake,cases,model=MODEL,provider="openai-compatible",
            variants=[{"name":"guarded","agent_command":[str(fake)],"config":str(config)}],
            api_budget={key:limits()[key] for key in RAW_KEYS})
        return repo,manifest

    def run_guarded(self, repo, manifest, upstream):
        out=self.root/"out"
        real_guard=budget_module.ApiBudgetGuard
        factory=lambda budget,key:real_guard(budget,key,upstream=upstream)
        phase_log=self.root/"phases.jsonl"
        with changed_dir(repo), patch.dict(os.environ,{"OPENAI_API_KEY":"dummy-parent-secret", "DUMMY_PHASE_LOG":str(phase_log)}), patch.object(runner,"ApiBudgetGuard",side_effect=factory):
            code=runner.main(["--manifest",str(manifest),"--output",str(out)])
        rows=[json.loads(row) for row in (out/"guarded/measurements.jsonl").read_text().splitlines()]
        return code,rows,out,phase_log

    def test_unknown_usage_stops_matrix_and_preserves_independent_checks(self):
        repo,manifest=self.prepared()
        seen=[]
        code,rows,out,phases=self.run_guarded(repo,manifest,lambda req:(seen.append(req) or (200,response(usage=False))))
        self.assertEqual(code,1);self.assertEqual(len(seen),1)
        self.assertEqual(rows[0]["verification_status"],"passed")
        self.assertEqual(rows[1]["run_status"],"harness_error")
        self.assertEqual(rows[1]["verification_status"],"not_run")
        self.assertIn("api_budget_stopped",rows[1]["harness_error"])
        summary=json.loads((out/"api-budget.json").read_text())
        self.assertEqual(summary["usage"]["reserved_upper_nano_usd"],21657600)
        self.assertIsNone(summary["usage"]["actual_cost"])
        self.assertTrue(all(not json.loads(line)["backend_key_seen"] for line in phases.read_text().splitlines()))
        self.assertTrue(all(row["accepted"] is None for row in rows))

    def test_keys_are_not_inherited_by_git_hooks_probes_agents_evidence_or_checks(self):
        repo,manifest=self.prepared()
        hooks=self.root/"hooks";hooks.mkdir()
        hook_log=self.root/"hook-result.txt"
        hook=hooks/"post-checkout"
        hook.write_text('#!/bin/sh\nif [ -n "$OPENAI_API_KEY" ]; then echo unsafe; else echo safe; fi >> '+str(hook_log)+'\n');hook.chmod(0o755)
        git("config","core.hooksPath",str(hooks),cwd=repo)
        seen=[]
        code,rows,out,phases=self.run_guarded(repo,manifest,lambda req:(seen.append(req) or (200,response())))
        self.assertEqual(code,0);self.assertEqual(len(seen),2)
        self.assertEqual(set(hook_log.read_text().splitlines()),{"safe"})
        logs=[json.loads(line) for line in phases.read_text().splitlines()]
        self.assertEqual({row["phase"] for row in logs},{"probe","agent","evidence"})
        self.assertTrue(all(not row["backend_key_seen"] for row in logs))
        self.assertTrue(all(row["verification_status"]=="passed" for row in rows))
        for file in out.rglob('*'):
            if file.is_file():self.assertNotIn("dummy-parent-secret",file.read_text(errors="replace"))

    def test_project_config_is_rejected_before_any_upstream_call(self):
        repo,manifest=self.prepared()
        directory=repo/".doge";directory.mkdir()
        (directory/"config.toml").write_text('[llm]\nmax_retries=5\n')
        git("add","-f",".doge/config.toml",cwd=repo);git("commit","-m","project override",cwd=repo)
        seen=[]
        code,rows,_out,_phases=self.run_guarded(repo,manifest,lambda req:(seen.append(req) or (200,response())))
        self.assertEqual(code,1);self.assertEqual(seen,[])
        self.assertEqual(rows[0]["harness_error"],"api_budget_project_config_not_allowed")
        self.assertIn("api_budget_stopped",rows[1]["harness_error"])
