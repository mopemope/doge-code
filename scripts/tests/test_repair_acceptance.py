"""Deterministic acceptance harness tests; no model/API or Go/Rust toolchain."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('repair_acceptance', Path(__file__).parents[1] / 'repair_acceptance.py')
repair = importlib.util.module_from_spec(spec)
spec.loader.exec_module(repair)

FAKE = '''import os, pathlib, sys
assert not any(k in os.environ for k in ('OPENAI_API_KEY', 'AWS_SECRET_ACCESS_KEY'))
assert pathlib.Path.home() != pathlib.Path(sys.argv[1])
source=pathlib.Path('block.go').read_text()
if source=='zero': print('no tests executed'); sys.exit(0)
if source=='timeout': import time; time.sleep(5)
if source=='mutate': pathlib.Path('block_test.go').write_text('changed')
if source=='fail': print('--- FAIL: TestIndependent'); sys.exit(1)
print('--- PASS: TestIndependent')
'''

class RepairAcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.cases = self.root/'cases'; self.cases.mkdir()
        fixture = self.cases/'fixture'; fixture.mkdir()
        (fixture/'block.go').write_text('correct')
        (fixture/'block_test.go').write_text('original test')
        (self.cases/'oracle').write_text('independent test')
        self.tool = self.root/'fake.py'; self.tool.write_text(FAKE)
        self.case = {'case_id':'go-test', 'language':'go', 'provenance':{'kind':'synthetic'}, 'fixture':'fixture', 'oracle':'oracle', 'prompt':'Fix the task.', 'allowed_paths':['block.go'], 'acceptance':['Independent tests and human review'], 'checks':[{'name':'all','argv':[sys.executable,str(self.tool),str(Path.home())],'min_tests':1,'timeout_seconds':0.2}]}
        (self.cases/'cases.json').write_text(json.dumps([self.case]))
        self.patcher=patch.object(repair,'CASES',self.cases);self.patcher.start();self.addCleanup(self.patcher.stop)
        self.versions_patcher=patch.object(repair,'tool_versions',return_value={'fixture':{'version':'deterministic fake checker'}})
        self.versions_patcher.start();self.addCleanup(self.versions_patcher.stop)
        self.output=self.root/'output'
        repair.prepare(self.output,self.root,['candidate'],1)
        self.run=self.output/'candidate--go-test--001'

    def evidence(self, accepted=True, status='completed'):
        return {'accepted':accepted,'notes':'Independent operator review, offline replay only.', 'execution':{'status':status,'elapsed_seconds':10,'review_seconds':2,'rework_seconds':3,'interventions':[{'kind':'manual_edit','note':'offline reference replay','seconds':3}], 'cost':{'estimated_usd':0,'confirmed_usd':0}}}

    def test_offline_import_does_not_load_live_model_entrypoint_or_budget(self):
        # Isolated interpreter: fail immediately if the offline entrypoint asks
        # to import the live runner's model-budget dependency.
        code = """import builtins, runpy, sys
original = builtins.__import__
def guarded(name, *args, **kwargs):
    if name in ('agent_eval_api_budget', 'agent_eval_common'):
        raise AssertionError('offline import crossed the model evaluation boundary')
    return original(name, *args, **kwargs)
builtins.__import__ = guarded
runpy.run_path(sys.argv[1])
assert not any(name in sys.modules for name in ('runtime_evals', 'agent_eval_api_budget'))
"""
        result = subprocess.run([sys.executable, '-c', code, str(Path(repair.__file__))], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_prepared_checker_binds_process_and_review_contract_sources(self):
        checker = repair.read(self.output/'plan.json')['checker']
        self.assertEqual(set(checker), {'repair_acceptance.py', 'agent_eval_process.py', 'repair_acceptance_contracts.py'})
        for name, recorded in checker.items():
            self.assertEqual(recorded, repair.digest((Path(repair.__file__).parent/name).read_bytes()))

    def test_default_selection_excludes_opt_in_history_and_other_cases(self):
        external=copy.deepcopy(self.case)
        external.update(case_id='external',default=False,provenance={'kind':'historical','base_commit':'f'*40})
        (self.cases/'cases.json').write_text(json.dumps([self.case,external]))
        output=self.root/'defaults'
        with patch.object(repair.subprocess,'run') as git:
            plan=repair.prepare(output,self.root,['candidate'],1)
        git.assert_not_called()
        self.assertEqual(plan['case_ids'],['go-test'])
        self.assertEqual(len(plan['runs']),1)

    def test_unknown_duplicate_and_empty_selection_rejected_before_output_or_git(self):
        for ids in [['unknown'],['go-test','go-test'],[]]:
            output=self.root/'invalid-selection'
            with patch.object(repair.subprocess,'run') as git,self.assertRaises(ValueError):
                repair.prepare(output,self.root,['candidate'],1,case_ids=ids)
            git.assert_not_called()
            self.assertFalse(output.exists())

    def test_selected_only_denominator_excludes_other_catalog_case(self):
        other=copy.deepcopy(self.case);other['case_id']='other'
        (self.cases/'cases.json').write_text(json.dumps([self.case,other]))
        output=self.root/'selected'
        plan=repair.prepare(output,self.root,['candidate'],1,case_ids=['go-test'])
        self.assertEqual(plan['case_ids'],['go-test'])
        run=output/self.run.name
        repair.check(run);repair.review(run,self.evidence())
        data=repair.summarize(output)['variants']['candidate']
        self.assertEqual(data['planned'],1)
        self.assertEqual(data['accepted_fraction_of_planned'],1)
        self.assertFalse((output/'candidate--other--001').exists())

    def test_missing_selected_history_fails_before_creating_output(self):
        external=copy.deepcopy(self.case)
        external.update(case_id='external',default=False,provenance={'kind':'historical','base_commit':'f'*40})
        (self.cases/'cases.json').write_text(json.dumps([self.case,external]))
        output=self.root/'missing-history'
        with patch.object(repair.subprocess,'run',side_effect=subprocess.CalledProcessError(128,['git'])),self.assertRaises(subprocess.CalledProcessError):
            repair.prepare(output,self.root,['candidate'],1,case_ids=['external'])
        self.assertFalse(output.exists())

    def test_explicit_historical_selection_exports_only_pre_fix_candidate_inputs(self):
        repo=self.root/'external-repo';repo.mkdir()
        subprocess.run(['git','init','-q',str(repo)],check=True)
        (repo/'block.go').write_text('before repair')
        (repo/'block_test.go').write_text('pre-existing tests')
        (repo/'go.mod').write_text('module example.invalid/fixture')
        subprocess.run(['git','-C',str(repo),'add','.'],check=True)
        env={**os.environ,'GIT_AUTHOR_NAME':'fixture','GIT_AUTHOR_EMAIL':'fixture@example.invalid','GIT_COMMITTER_NAME':'fixture','GIT_COMMITTER_EMAIL':'fixture@example.invalid','GIT_CONFIG_NOSYSTEM':'1','GIT_CONFIG_GLOBAL':os.devnull}
        subprocess.run(['git','-C',str(repo),'-c','core.hooksPath=/dev/null','commit','-qm','before'],env=env,check=True)
        commit=subprocess.check_output(['git','-C',str(repo),'rev-parse','HEAD'],text=True).strip()
        (repo/'block.go').write_text('future solution')
        (repo/'future_fix_test.go').write_text('future regression')
        external=copy.deepcopy(self.case)
        external.update(case_id='external',default=False,provenance={'kind':'historical','base_commit':commit,'reference_fix':'future-reference-sentinel'})
        (self.cases/'cases.json').write_text(json.dumps([self.case,external]))
        output=self.root/'external-experiment'
        plan=repair.prepare(output,repo,['candidate'],1,case_ids=['external'])
        self.assertEqual(len(plan['runs']),1)
        run=output/'candidate--external--001';workspace=run/'workspace'
        self.assertEqual((workspace/'block.go').read_text(),'before repair')
        self.assertFalse((workspace/'.git').exists())
        self.assertFalse((workspace/'future_fix_test.go').exists())
        self.assertFalse((workspace/'repair_acceptance_test.go').exists())
        self.assertNotIn('future-reference-sentinel',(workspace/'REPAIR_TASK.txt').read_text())
        (workspace/'go.mod').write_text('changed build configuration')
        result=repair.check(run)
        self.assertEqual(result['scope_violations'],['go.mod'])
        self.assertEqual(result['checks'][0]['status'],'scope_denied')

    def test_vet_is_separate_and_does_not_turn_zero_reported_tests_into_pass(self):
        self.case['checks'].append({'name':'vet','kind':'vet','argv':['go','vet','./...'],'timeout_seconds':1})
        (self.cases/'cases.json').write_text(json.dumps([self.case]))
        original=repair.runtime.stream_child
        def vet_output(argv,cwd,env,timeout,grace,stdout,stderr):
            if argv==['go','vet','./...']:
                stdout.write_text('');stderr.write_text('')
                return 0,False,0.01,None,False
            return original(argv,cwd,env,timeout,grace,stdout,stderr)
        for source,expected in [('correct',True),('zero',False)]:
            output=self.root/('vet-'+source)
            repair.prepare(output,self.root,['candidate'],1)
            run=output/self.run.name;(run/'workspace/block.go').write_text(source)
            with patch.object(repair.runtime,'stream_child',side_effect=vet_output):
                result=repair.check(run)
            self.assertEqual(result['machine_passed'],expected)
            self.assertEqual(result['checks'][1]['reported_tests'],0)
            self.assertEqual(result['checks'][1]['test_count_source'],'not_applicable')
            self.assertTrue(result['checks'][1]['passed'])

    def test_vet_kind_cannot_bypass_test_count_for_arbitrary_commands(self):
        self.case['checks'][0].update(kind='vet',argv=['go','test','./...'])
        (self.cases/'cases.json').write_text(json.dumps([self.case]))
        with self.assertRaisesRegex(ValueError,'fixed go vet'):
            repair.prepare(self.root/'bad-vet',self.root,['candidate'],1)
        self.case['checks'][0].update(kind='test',min_tests=0)
        (self.cases/'cases.json').write_text(json.dumps([self.case]))
        with self.assertRaisesRegex(ValueError,'positive minimum'):
            repair.prepare(self.root/'bad-minimum',self.root,['candidate'],1)

    def test_version_probe_records_observation_and_missing_tool(self):
        self.versions_patcher.stop()
        try:
            with patch.object(repair.subprocess,'run',return_value=subprocess.CompletedProcess(['go','version'],0,'go version fixture','')),patch.object(repair.shutil,'which',return_value='/fixture/go'):
                versions=repair.tool_versions('go',{'PATH':'/fixture'},self.root)
            self.assertEqual(versions['go']['version'],'go version fixture')
            with patch.object(repair.subprocess,'run',side_effect=FileNotFoundError):
                versions=repair.tool_versions('go',{'PATH':'/fixture'},self.root)
            self.assertIsNone(versions['go']['version'])
        finally:
            self.versions_patcher.start()

    def test_checks_are_separate_from_acceptance_and_no_credentials_inherited(self):
        with patch.dict(os.environ,{'OPENAI_API_KEY':'test-not-a-key','AWS_SECRET_ACCESS_KEY':'test-not-a-secret'}):
            result=repair.check(self.run)
        self.assertTrue(result['machine_passed'])
        self.assertIsNone(result['accepted'])
        self.assertEqual((self.run/'workspace/block_test.go').read_text(),'original test')
        self.assertFalse((self.run/'workspace/repair_acceptance_test.go').exists())
        self.assertFalse((self.run/'workspace/.git').exists())
        review=repair.review(self.run,self.evidence())
        self.assertTrue(review['accepted'])
        self.assertEqual(repair.summarize(self.output)['variants']['candidate']['accepted'],1)

    def test_zero_tests_is_incomplete_and_cannot_be_accepted(self):
        (self.run/'workspace/block.go').write_text('zero')
        result=repair.check(self.run)
        self.assertFalse(result['machine_passed'])
        self.assertEqual(result['checks'][0]['status'],'incomplete')
        with self.assertRaises(ValueError):repair.review(self.run,self.evidence())
        repair.review(self.run,self.evidence(False))
        data=repair.summarize(self.output)['variants']['candidate']
        self.assertIsNone(data['confirmed_usd_per_accepted'])
        self.assertEqual(data['reviewed_interventions'],1)

    def test_stdout_counts_are_reported_not_execution_attestation(self):
        result=repair.check(self.run)
        self.assertTrue(result['machine_passed'])
        self.assertEqual(result['checks'][0]['test_count_source'],'stdout_heuristic')
        self.assertFalse(result['checks'][0]['test_execution_attested'])
        self.assertIsNone(result['accepted'])

    def test_inspection_is_read_only_and_never_launches_checks(self):
        repair.check(self.run)
        before=repair.evidence_files(self.run)
        with patch.object(repair.runtime,'stream_child',side_effect=AssertionError('must not launch')),patch.object(repair.subprocess,'run',side_effect=AssertionError('must not probe')):
            result=repair.inspect_run(self.run,include_patch=True)
        self.assertIsNone(result['accepted'])
        self.assertTrue(result['machine_passed'])
        self.assertEqual(result['patch']['content'],'')
        self.assertEqual(result['checks'][0]['output_evidence']['stdout']['hash_binding'],'verified')
        self.assertEqual(repair.evidence_files(self.run),before)
        self.assertFalse((self.run/'review.json').exists())

    def test_historical_inspection_and_summary_do_not_rescore_after_checker_update(self):
        repair.check(self.run);repair.review(self.run,self.evidence())
        before=repair.evidence_files(self.run)
        with patch.object(repair,'checker_identity',return_value={'different':'reader'}):
            result=repair.inspect_run(self.run)
            summary=repair.summarize(self.output)
            self.assertFalse(result['checker']['matches_current'])
            self.assertTrue(result['accepted'])
            self.assertEqual(summary['variants']['candidate']['accepted'],1)
            self.assertEqual(summary['checker']['interpretation'],'saved_results_not_rescored')
            with self.assertRaisesRegex(ValueError,'checker changed'):repair.check(self.run)
            with self.assertRaisesRegex(ValueError,'checker changed'):repair.review(self.run,self.evidence())
        self.assertEqual(repair.evidence_files(self.run),before)

    def test_new_recorded_log_tampering_blocks_review_inspection_and_summary(self):
        repair.check(self.run)
        repair.review(self.run,self.evidence())
        (self.run/'all.stdout.txt').write_text('forged pass output')
        for operation in [lambda:repair.inspect_run(self.run),lambda:repair.summarize(self.output),lambda:repair.review(self.run,self.evidence())]:
            with self.assertRaisesRegex(ValueError,'output evidence changed'):operation()

    def test_new_log_hash_deletion_cannot_downgrade_to_legacy(self):
        repair.check(self.run)
        path=self.run/'checks.json';checks=repair.read(path)
        checks['checks'][0].pop('stdout_sha256')
        path.write_text(json.dumps(checks))
        (self.run/'all.stdout.txt').write_text('forged log')
        for operation in [lambda:repair.inspect_run(self.run),lambda:repair.review(self.run,self.evidence())]:
            with self.assertRaisesRegex(ValueError,'hash binding is missing'):operation()

    def test_old_unbound_logs_are_explicit_not_promoted_to_verified(self):
        repair.check(self.run)
        path=self.run/'checks.json';checks=repair.read(path)
        checks['checks'][0].pop('stdout_sha256');checks['checks'][0].pop('stderr_sha256')
        checks.pop('output_binding')
        # A saved legacy fixture has no prepared binding contract; changing only
        # a new checks file must never downgrade it to this old record shape.
        criteria=repair.read(self.run/'criteria.json');criteria.pop('output_binding')
        (self.run/'criteria.json').write_text(json.dumps(criteria))
        criteria_digest=repair.digest((self.run/'criteria.json').read_bytes())
        plan=repair.read(self.output/'plan.json');plan['runs'][0]['criteria_digest']=criteria_digest
        (self.output/'plan.json').write_text(json.dumps(plan))
        checks['criteria_digest']=criteria_digest
        path.write_text(json.dumps(checks))
        result=repair.inspect_run(self.run)
        self.assertEqual(result['checks'][0]['output_evidence']['stdout']['hash_binding'],'unrecorded_legacy_or_not_captured')
        self.assertIsNone(result['accepted'])

    def test_historical_inspection_preserves_criteria_patch_and_candidate_guards(self):
        repair.check(self.run)
        with patch.object(repair,'checker_identity',return_value={'different':'reader'}):
            (self.run/'candidate.patch').write_text('changed')
            with self.assertRaisesRegex(ValueError,'patch evidence changed'):repair.inspect_run(self.run)
            (self.run/'candidate.patch').write_text('')
            (self.run/'workspace/block.go').write_text('later patch')
            with self.assertRaisesRegex(ValueError,'candidate changed'):repair.inspect_run(self.run)

    def test_inspection_rejects_changed_files_during_read(self):
        repair.check(self.run)
        original=repair.saved_evidence
        calls=0
        def change_after_read(*args):
            nonlocal calls
            result=original(*args);calls+=1
            if calls==2:(self.run/'all.stderr.txt').write_text('later output')
            return result
        with patch.object(repair,'saved_evidence',side_effect=change_after_read),self.assertRaisesRegex(ValueError,'evidence changed during inspection'):
            repair.inspect_run(self.run)

    def test_inspection_inline_patch_limit_and_identity_mismatch(self):
        (self.run/'workspace/block.go').write_text('correct'+' '*70000)
        repair.check(self.run)
        self.assertIsNone(repair.inspect_run(self.run)['patch']['content'])
        with self.assertRaisesRegex(ValueError,'64 KiB'):repair.inspect_run(self.run,include_patch=True)
        repair.review(self.run,self.evidence())
        row=repair.read(self.run/'review.json');row['variant']='different'
        (self.run/'review.json').write_text(json.dumps(row))
        with self.assertRaisesRegex(ValueError,'different trial identity'):repair.inspect_run(self.run)

    def test_candidate_change_after_review_rejects_summary(self):
        repair.check(self.run)
        repair.review(self.run,self.evidence())
        (self.run/'workspace/block.go').write_text('unverified later patch')
        with self.assertRaisesRegex(ValueError,'reviewed candidate changed'):repair.summarize(self.output)

    def test_checker_change_requires_new_experiment(self):
        with patch.object(repair,'checker_identity',return_value={'changed':'checker'}):
            with self.assertRaisesRegex(ValueError,'checker changed'):repair.check(self.run)

    def test_aba_change_during_copy_is_invalidated_before_checks(self):
        original=repair.shutil.copytree
        source=self.run/'workspace/block.go'
        def changing_copy(src,dest,*args,**kwargs):
            if Path(src)==self.run/'workspace':
                source.write_text('different candidate')
                result=original(src,dest,*args,**kwargs)
                source.write_text('correct')
                return result
            return original(src,dest,*args,**kwargs)
        with patch.object(repair.shutil,'copytree',side_effect=changing_copy):
            result=repair.check(self.run)
        self.assertFalse(result['machine_passed'])
        self.assertEqual(result['checks'][0]['status'],'invalidated')
        self.assertFalse((self.run/'all.stdout.txt').exists())
        self.assertEqual((self.run/'candidate.patch').read_text(),'')

    def test_workspace_change_during_check_invalidates_pass(self):
        original=repair.runtime.stream_child
        def change_after_check(*args,**kwargs):
            result=original(*args,**kwargs)
            (self.run/'workspace/block.go').write_text('new candidate')
            return result
        with patch.object(repair.runtime,'stream_child',side_effect=change_after_check):
            result=repair.check(self.run)
        self.assertFalse(result['machine_passed'])
        self.assertEqual(result['checks'][0]['status'],'invalidated')

    def test_unknown_operator_times_are_not_zero_or_per_acceptance(self):
        repair.check(self.run)
        evidence=self.evidence()
        for key in ('elapsed_seconds','review_seconds','rework_seconds'):
            evidence['execution'][key]=None
        evidence['execution']['interventions'][0]['seconds']=None
        repair.review(self.run,evidence)
        data=repair.summarize(self.output)['variants']['candidate']
        self.assertIsNone(data['elapsed_seconds_per_accepted'])
        self.assertEqual(data['elapsed_seconds_coverage'],0)
        self.assertIsNone(data['reviewed_intervention_seconds'])

    def test_candidate_patch_tampering_is_rejected(self):
        repair.check(self.run)
        (self.run/'candidate.patch').write_text('unverified patch')
        with self.assertRaisesRegex(ValueError,'patch evidence changed'):repair.review(self.run,self.evidence())

    def test_existing_tests_or_new_files_outside_scope_deny_before_spawn(self):
        (self.run/'workspace/block_test.go').write_text('removed')
        (self.run/'workspace/new.go').write_text('unexpected')
        result=repair.check(self.run)
        self.assertEqual(result['scope_violations'],['block_test.go','new.go'])
        self.assertEqual(result['checks'][0]['status'],'scope_denied')
        self.assertFalse((self.run/'all.stdout.txt').exists())

    def test_changed_oracle_and_changed_criteria_are_rejected(self):
        oracle=self.run/'oracle';oracle.write_text('tampered')
        with self.assertRaisesRegex(ValueError,'oracle changed'):repair.check(self.run)
        oracle.write_text('independent test')
        criteria=repair.read(self.run/'criteria.json');criteria['allowed_paths'].append('block_test.go')
        (self.run/'criteria.json').write_text(json.dumps(criteria))
        with self.assertRaisesRegex(ValueError,'frozen'):repair.check(self.run)

    def test_candidate_patch_preserved_and_post_check_change_blocks_review(self):
        (self.run/'workspace/block.go').write_text('correct\n')
        repair.check(self.run)
        self.assertIn('+correct', (self.run/'candidate.patch').read_text())
        (self.run/'workspace/block.go').write_text('later change')
        with self.assertRaisesRegex(ValueError,'changed after checks'):repair.review(self.run,self.evidence())

    def test_checks_mutating_source_are_invalidated(self):
        (self.run/'workspace/block.go').write_text('mutate')
        result=repair.check(self.run)
        self.assertEqual(result['checks'][0]['status'],'invalidated')
        self.assertFalse(result['machine_passed'])
        self.assertEqual((self.run/'workspace/block_test.go').read_text(),'original test')

    def test_timeout_and_failure_are_distinct(self):
        (self.run/'workspace/block.go').write_text('timeout')
        result=repair.check(self.run)
        self.assertEqual(result['checks'][0]['status'],'timed_out')
        repair.review(self.run,self.evidence(False,'timed_out'))
        other=self.root/'other';repair.prepare(other,self.root,['candidate'],1)
        run=other/self.run.name
        (run/'workspace/block.go').write_text('fail')
        result=repair.check(run)
        self.assertEqual(result['checks'][0]['status'],'failed')
        self.assertEqual(result['checks'][0]['reported_tests'],1)

    def test_missing_tool_is_blocked(self):
        self.case['checks'][0]['argv']=[str(self.root/'missing')]
        (self.cases/'cases.json').write_text(json.dumps([self.case]))
        other=self.root/'other';repair.prepare(other,self.root,['candidate'],1)
        result=repair.check(other/self.run.name)
        self.assertEqual(result['checks'][0]['status'],'blocked')
        self.assertFalse(result['machine_passed'])

    def test_append_only_stages_and_bad_variant_names(self):
        with self.assertRaises(ValueError):repair.prepare(self.output,self.root,['candidate'],1)
        with self.assertRaises(ValueError):repair.prepare(self.root/'bad',self.root,['../escape'],1)
        repair.check(self.run)
        with self.assertRaises(ValueError):repair.check(self.run)
        repair.review(self.run,self.evidence())
        with self.assertRaises(FileExistsError):repair.review(self.run,self.evidence())

    def test_nonfinite_negative_boolean_and_huge_accounting_rejected(self):
        repair.check(self.run)
        for v in [True,-1,float('nan'),float('inf'),10**1000]:
            evidence=self.evidence();evidence['execution']['elapsed_seconds']=v
            with self.subTest(value=type(v).__name__),self.assertRaises(ValueError):repair.review(self.run,evidence)
        evidence=self.evidence();evidence['execution']['cost']['estimated_usd']=0.1
        with self.assertRaises(ValueError):repair.review(self.run,evidence)

    def test_all_planned_failures_costs_and_human_effort_count(self):
        other=self.root/'matrix';repair.prepare(other,self.root,['candidate'],2,'model-evaluation')
        for trial in (1,2):
            run=other/f'candidate--go-test--{trial:03d}'
            if trial==2:(run/'workspace/block.go').write_text('fail')
            repair.check(run)
            evidence=self.evidence(trial==1,'completed' if trial==1 else 'failed')
            evidence['execution']['identity']={k:'explicit actual setting' for k in ('model','provider','configuration','permissions','environment')}
            evidence['execution']['cost']={'estimated_usd':0.2,'confirmed_usd':None}
            repair.review(run,evidence)
        data=repair.summarize(other)['variants']['candidate']
        self.assertEqual(data['accepted_fraction_of_planned'],0.5)
        self.assertAlmostEqual(data['estimated_usd_per_accepted'],0.4)
        self.assertIsNone(data['confirmed_usd_per_accepted'])
        self.assertEqual(data['review_seconds_per_accepted'],4)
        self.assertEqual(data['elapsed_seconds_per_accepted'],20)
        self.assertEqual(data['statuses']['failed'],1)
        self.assertEqual(data['reviewed_interventions'],2)

    def test_unreviewed_planned_runs_never_disappear_or_look_free(self):
        data=repair.summarize(self.output)['variants']['candidate']
        self.assertEqual(data['planned'],1)
        self.assertEqual(data['unreviewed'],1)
        self.assertIsNone(data['total_estimated_usd'])
        self.assertIsNone(data['elapsed_seconds_per_accepted'])

    def test_escaping_symlink_is_rejected(self):
        (self.run/'workspace/block.go').unlink()
        (self.run/'workspace/block.go').symlink_to(self.tool)
        with self.assertRaisesRegex(ValueError,'escaping'):repair.check(self.run)

    def test_historical_archive_has_pre_fix_identity_and_no_future_git_history(self):
        repo=self.root/'repo';repo.mkdir()
        subprocess.run(['git','init','-q',str(repo)],check=True)
        (repo/'bug.txt').write_text('before')
        subprocess.run(['git','-C',str(repo),'add','.'],check=True)
        env={**os.environ,'GIT_AUTHOR_NAME':'fixture','GIT_AUTHOR_EMAIL':'fixture@example.invalid','GIT_COMMITTER_NAME':'fixture','GIT_COMMITTER_EMAIL':'fixture@example.invalid','GIT_CONFIG_NOSYSTEM':'1','GIT_CONFIG_GLOBAL':os.devnull}
        subprocess.run(['git','-C',str(repo),'-c','core.hooksPath=/dev/null','commit','-qm','before'],env=env,check=True)
        commit=subprocess.check_output(['git','-C',str(repo),'rev-parse','HEAD'],text=True).strip()
        (repo/'bug.txt').write_text('future solution')
        destination=self.root/'archive';destination.mkdir()
        repair.archive(repo,commit,destination)
        self.assertEqual((destination/'bug.txt').read_text(),'before')
        self.assertFalse((destination/'.git').exists())

if __name__=='__main__':unittest.main()
