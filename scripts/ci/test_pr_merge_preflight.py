#!/usr/bin/env python3
"""Real Git fixtures for the PR preflight; no GitHub or Rust build needed."""
import os
import json
import shlex
import re
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/ci/pr_merge_preflight.sh'


class MergePreflight(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.repo = self.root / 'checkout'
        self.repo.mkdir()
        self.env = dict(os.environ, HOME=str(self.root), GIT_CONFIG_NOSYSTEM='1',
                        GIT_AUTHOR_NAME='CI fixture', GIT_COMMITTER_NAME='CI fixture',
                        GIT_AUTHOR_EMAIL='ci@example.invalid',
                        GIT_COMMITTER_EMAIL='ci@example.invalid')
        self.git('init', '-q')
        self.base = self.commit()
        self.head = self.commit(self.base)
        self.other = self.commit(self.head)
        self.merge = self.commit(self.base, self.head)
        self.git('checkout', '-q', '--detach', self.merge)
        self.remote = self.root / 'origin.git'
        self.git('init', '--bare', '-q', str(self.remote))
        self.git('remote', 'add', 'origin', str(self.remote))
        self.publish(self.base, self.head)
        self.output = self.root / 'output'
        self.api = self.root / 'pr.json'
        self.identity = dict(number=7, state='open',
                             base=dict(ref='main', repo=dict(full_name='owner/repo')),
                             head=dict(sha=self.head))
        self.api.write_text(json.dumps(self.identity))
        bindir = self.root / 'api-bin'
        bindir.mkdir()
        curl = bindir / 'curl'
        curl.write_text('#!/bin/sh\ncat "$PR_FIXTURE"\n')
        curl.chmod(0o700)
        self.env.update(PATH=str(bindir) + os.pathsep + self.env['PATH'],
                        PR_FIXTURE=str(self.api))

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.repo,
                                       env=self.env, input='', text=True).strip()

    def commit(self, *parents):
        tree = self.git('hash-object', '-t', 'tree', '--stdin')
        args = ['commit-tree', tree, '-m', 'fixture']
        for parent in parents:
            args += ['-p', parent]
        return self.git(*args)

    def publish(self, base, head):
        self.git('push', '-q', '--force', 'origin',
                 f'{base}:refs/heads/main', f'{head}:refs/pull/7/head')

    def run_preflight(self, **env):
        self.output.write_text('')
        settings = dict(self.env, PR_BASE_REF='main', PR_NUMBER='7',
                        GITHUB_OUTPUT=str(self.output), PR_HEAD_SHA=self.head,
                        GITHUB_REPOSITORY='owner/repo', PR_MERGE_TIMEOUT_SECONDS='1')
        settings.update(env)
        result = subprocess.run(['bash', str(SCRIPT)], cwd=self.repo,
                                env=settings,
                                text=True, capture_output=True, timeout=30)
        return result

    def assert_refused(self):
        result = self.run_preflight()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.output.read_text(), '')
        self.assertIn('observed', result.stdout)
        self.assertIn('expected', result.stdout)
        return result

    def test_matching_pair_exports_actual_merge(self):
        result = self.run_preflight()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text(), f'merge={self.merge}\nbase={self.base}\n')
        for sha in [self.base, self.head, self.merge]:
            self.assertIn(sha, result.stdout)

    def test_stale_event_resolves_current_merge(self):
        current = self.commit(self.other, self.head)
        self.publish(self.other, self.head)
        self.git('push', '-q', 'origin', f'{current}:refs/pull/7/merge')
        result = self.run_preflight()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text(), f'merge={current}\nbase={self.other}\n')

    def test_retarget_requires_new_event(self):
        self.identity['base']['ref'] = 'release'
        self.api.write_text(json.dumps(self.identity))
        self.assert_refused()

    def test_wrong_pr_identity_is_refused(self):
        self.identity['number'] = 8
        self.api.write_text(json.dumps(self.identity))
        self.assert_refused()

    def test_changed_api_head_is_refused_even_if_refs_lag(self):
        self.identity['head']['sha'] = self.other
        self.api.write_text(json.dumps(self.identity))
        result = self.assert_refused()
        self.assertIn('expected event', result.stdout)
        self.assertIn('observed PR', result.stdout)
        self.assertIn(self.other, result.stdout)

    def shim_git(self, before='', after=''):
        real_git = subprocess.check_output(['which', 'git'], text=True).strip()
        bindir = self.root / 'git-bin'
        bindir.mkdir()
        shim = bindir / 'git'
        shim.write_text(f'#!/bin/bash\nreal_git={shlex.quote(real_git)}\n' + before +
                        '\n"$real_git" "$@"\nstatus=$?\n' + after + '\nexit "$status"\n')
        shim.chmod(0o700)
        self.env['PATH'] = str(bindir) + os.pathsep + self.env['PATH']

    def test_fast_path_does_not_fetch(self):
        self.shim_git(before='[[ "$1" != fetch ]] || exit 99')
        result = self.run_preflight()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_delayed_publication(self):
        current = self.commit(self.other, self.head)
        self.publish(self.other, self.head)
        self.git('push', '-q', 'origin', f'{self.merge}:refs/pull/7/merge')
        marker = shlex.quote(str(self.root / 'published'))
        self.shim_git(after=f"""
if [[ "$1" == ls-remote && ! -f {marker} ]]; then
  touch {marker}
  "$real_git" push -q --force origin {current}:refs/pull/7/merge
fi
""")
        result = self.run_preflight(PR_MERGE_TIMEOUT_SECONDS='5')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text(), f'merge={current}\nbase={self.other}\n')
        self.assertIn('awaiting publication', result.stdout)

    def test_base_moves_during_fetch(self):
        current = self.commit(self.other, self.head)
        newer_base = self.commit(self.other)
        newer = self.commit(newer_base, self.head)
        self.publish(self.other, self.head)
        self.git('push', '-q', 'origin', f'{current}:refs/pull/7/merge')
        marker = shlex.quote(str(self.root / 'moved'))
        self.shim_git(after=f"""
if [[ "$1" == fetch && ! -f {marker} ]]; then
  touch {marker}
  "$real_git" push -q --force origin {newer_base}:refs/heads/main {newer}:refs/pull/7/merge
fi
""")
        result = self.run_preflight(PR_MERGE_TIMEOUT_SECONDS='5')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text(), f'merge={newer}\nbase={newer_base}\n')

    def test_base_moves_before_fast_path_publication(self):
        current = self.commit(self.other, self.head)
        # The first lookup returns the original pair, then main advances.
        marker = shlex.quote(str(self.root / 'moved'))
        self.shim_git(after=f"""
if [[ "$1" == ls-remote && ! -f {marker} ]]; then
  touch {marker}
  "$real_git" push -q --force origin {self.other}:refs/heads/main {current}:refs/pull/7/merge
fi
""")
        result = self.run_preflight(PR_MERGE_TIMEOUT_SECONDS='5')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text(), f'merge={current}\nbase={self.other}\n')

    def test_api_head_changes_after_fetch(self):
        current = self.commit(self.other, self.head)
        self.publish(self.other, self.head)
        self.git('push', '-q', 'origin', f'{current}:refs/pull/7/merge')
        self.identity['head']['sha'] = self.other
        changed = self.root / 'changed.json'
        changed.write_text(json.dumps(self.identity))
        self.shim_git(after=f'if [[ "$1" == fetch ]]; then\n'
                      f'  cp {shlex.quote(str(changed))} "$PR_FIXTURE"\nfi')
        self.assert_refused()

    def test_fetch_failure_is_refused(self):
        current = self.commit(self.other, self.head)
        self.publish(self.other, self.head)
        self.git('push', '-q', 'origin', f'{current}:refs/pull/7/merge')
        self.shim_git(before='[[ "$1" != fetch ]] || exit 1')
        result = self.assert_refused()
        self.assertIn('candidate fetch failed', result.stderr)

    def test_head_moves_during_fetch(self):
        current = self.commit(self.other, self.head)
        self.publish(self.other, self.head)
        self.git('push', '-q', 'origin', f'{current}:refs/pull/7/merge')
        self.shim_git(after=f'if [[ "$1" == fetch ]]; then\n'
                      f'  "$real_git" push -q --force origin {self.other}:refs/pull/7/head\nfi')
        self.assert_refused()

    def test_target_changes_after_fetch(self):
        current = self.commit(self.other, self.head)
        self.publish(self.other, self.head)
        self.git('push', '-q', 'origin', f'{current}:refs/pull/7/merge')
        self.identity['base']['ref'] = 'release'
        changed = self.root / 'changed.json'
        changed.write_text(json.dumps(self.identity))
        self.shim_git(after=f'if [[ "$1" == fetch ]]; then\n'
                      f'  cp {shlex.quote(str(changed))} "$PR_FIXTURE"\nfi')
        self.assert_refused()

    def test_api_failure_with_valid_json(self):
        curl = self.root / 'api-bin' / 'curl'
        curl.write_text('#!/bin/sh\ncat "$PR_FIXTURE"\nexit 1\n')
        self.assert_refused()

    def test_malformed_api_response(self):
        self.api.write_text('{')
        self.assert_refused()

    def test_fetch_timeout_uses_total_deadline(self):
        current = self.commit(self.other, self.head)
        self.publish(self.other, self.head)
        self.git('push', '-q', 'origin', f'{current}:refs/pull/7/merge')
        self.shim_git(before='if [[ "$1" == fetch ]]; then exec sleep 120; fi')
        started = time.monotonic()
        result = self.assert_refused()
        self.assertLess(time.monotonic() - started, 3)
        self.assertIn('deadline expired', result.stderr)

    def test_unpublished_merge_expires_without_output(self):
        self.publish(self.other, self.head)
        started = time.monotonic()
        result = self.assert_refused()
        self.assertLess(time.monotonic() - started, 3)
        self.assertIn('deadline expired', result.stderr)

    def test_invalid_published_parent_order_expires(self):
        invalid = self.commit(self.head, self.other)
        self.publish(self.other, self.head)
        self.git('push', '-q', 'origin', f'{invalid}:refs/pull/7/merge')
        self.assert_refused()

    def test_identity_validation_survives_python_optimization(self):
        self.identity['head']['sha'] = self.other
        self.api.write_text(json.dumps(self.identity))
        result = self.run_preflight(PYTHONOPTIMIZE='1')
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.output.read_text(), '')

    def test_stale_base(self):
        self.publish(self.other, self.head)
        self.assert_refused()

    def test_stale_head(self):
        self.publish(self.base, self.other)
        self.assert_refused()

    def test_reversed_parents(self):
        self.git('checkout', '-q', '--detach', self.commit(self.head, self.base))
        self.assert_refused()

    def test_single_parent_is_not_a_merge(self):
        self.git('checkout', '-q', '--detach', self.head)
        self.assert_refused()

    def test_three_parents_are_refused(self):
        self.git('checkout', '-q', '--detach', self.commit(self.base, self.head, self.other))
        self.assert_refused()

    def test_missing_live_head(self):
        self.git('push', '-q', 'origin', ':refs/pull/7/head')
        self.assert_refused()

    def test_lookup_failure(self):
        self.git('remote', 'set-url', 'origin', str(self.root / 'missing.git'))
        self.assert_refused()

    def test_missing_live_base(self):
        self.git('push', '-q', 'origin', ':refs/heads/main')
        self.assert_refused()

    def test_depth_two_checkout_preserves_both_parents(self):
        self.git('push', '-q', 'origin', f'{self.merge}:refs/pull/7/merge')
        shallow = self.root / 'shallow'
        self.git('clone', '-q', '--depth=2', '--branch', 'main',
                 self.remote.as_uri(), str(shallow))
        self.repo = shallow
        self.git('fetch', '-q', '--depth=2', 'origin', 'refs/pull/7/merge')
        self.git('checkout', '-q', '--detach', 'FETCH_HEAD')
        self.assertEqual(self.git('rev-parse', '--is-shallow-repository'), 'true')
        result = self.run_preflight()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text(), f'merge={self.merge}\nbase={self.base}\n')

    def test_stale_depth_two_checkout_fetches_advertised_object(self):
        current = self.commit(self.other, self.head)
        self.publish(self.other, self.head)
        self.git('push', '-q', 'origin', f'{current}:refs/pull/7/merge',
                 f'{self.merge}:refs/pull/7/event')
        shallow = self.root / 'shallow'
        self.git('clone', '-q', '--depth=2', '--branch', 'main',
                 self.remote.as_uri(), str(shallow))
        self.repo = shallow
        self.git('fetch', '-q', '--depth=2', 'origin', 'refs/pull/7/event')
        self.git('checkout', '-q', '--detach', 'FETCH_HEAD')
        absent = subprocess.run(['git', 'cat-file', '-e', current], cwd=self.repo,
                                env=self.env, capture_output=True)
        self.assertNotEqual(absent.returncode, 0)
        result = self.run_preflight(PR_MERGE_TIMEOUT_SECONDS='5')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text(), f'merge={current}\nbase={self.other}\n')

    def test_hung_lookup_is_bounded(self):
        bindir = self.root / 'bin'
        bindir.mkdir()
        real_git = subprocess.check_output(['which', 'git'], text=True).strip()
        shim = bindir / 'git'
        shim.write_text('#!/bin/bash\nif [[ "$1" == ls-remote ]]; then\n'
                        f'exec sleep 120\nfi\nexec {real_git} "$@"\n')
        shim.chmod(0o700)
        self.env['PATH'] = str(bindir) + os.pathsep + self.env['PATH']
        started = time.monotonic()
        self.assert_refused()
        self.assertLess(time.monotonic() - started, 28)

    def test_lookup_failure_with_matching_stdout(self):
        # Model a transport failing after printing partial/matching refs.
        bindir = self.root / 'bin'
        bindir.mkdir()
        real_git = subprocess.check_output(['which', 'git'], text=True).strip()
        shim = bindir / 'git'
        shim.write_text('#!/bin/bash\nif [[ "$1" == ls-remote ]]; then\n'
                        f"printf '%s\\t%s\\n' '{self.base}' refs/heads/main '{self.head}' refs/pull/7/head\n"
                        f'exit 1\nfi\nexec {real_git} "$@"\n')
        shim.chmod(0o700)
        self.env['PATH'] = str(bindir) + os.pathsep + self.env['PATH']
        self.assert_refused()


class WorkflowConditions(unittest.TestCase):
    """Evaluate this workflow's boolean subset, including implicit success().

    These local controls are not a substitute for GitHub execution receipts.
    """
    def condition(self, job, event, preflight, changes='success', mode='all', merge='verified-merge', base='verified-base'):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        # Stop at the next top-level job, not a job property.
        body = re.split(r'\n  (?=\S)', workflow.split(f'\n  {job}:\n', 1)[1])[0]
        expression = re.search(r'^    if: (.*)$', body, re.M)[1]
        expression = expression.removeprefix('${{').removesuffix('}}').strip()
        dependencies = re.search(r'^    needs: (.*)$', body, re.M)
        results = dict(preflight=preflight, changes=changes)
        if not re.search(r'\b(always|cancelled|success|failure)\(', expression):
            # Actions adds success() unless a status function is present.
            names = dependencies[1].strip('[]').split(', ') if dependencies else []
            if any(results[name] != 'success' for name in names):
                return False
        scope = dict(
            github=SimpleNamespace(event_name=event, ref='refs/heads/main'),
            needs=SimpleNamespace(
                preflight=SimpleNamespace(result=preflight,
                                          outputs=SimpleNamespace(merge=merge, base=base)),
                changes=SimpleNamespace(result=changes,
                                        outputs=SimpleNamespace(mode=mode))),
            always=lambda: True, cancelled=lambda: False)
        expression = expression.replace('&&', ' and ').replace('||', ' or ')
        expression = re.sub(r'!(?!=)', ' not ', expression)
        return bool(eval(expression, {'__builtins__': {}}, scope))

    def admission(self, event, preflight, changes='success', mode='all',
                  merge='verified-merge', base='verified-base'):
        # Execute the actual first check step, with its actual Actions env wiring.
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        body = workflow.split('      - name: Require valid classification\n', 1)[1]
        body = body.split('      - uses:', 1)[0]
        values = {'github.event_name': event, 'github.ref': 'refs/heads/main',
                  'needs.preflight.result': preflight,
                  'needs.preflight.outputs.merge': merge,
                  'needs.preflight.outputs.base': base,
                  'needs.changes.result': changes, 'needs.changes.outputs.mode': mode}
        env = dict(os.environ)
        for name, expression in re.findall(r'^          (\w+): \$\{\{ (.*?) \}\}$', body, re.M):
            env[name] = values[expression]
        script = body.split('        run: |\n', 1)[1]
        script = '\n'.join(line[10:] for line in script.splitlines())
        return subprocess.run(['bash', '-c', script], env=env,
                              capture_output=True, text=True, timeout=5).returncode

    def test_stale_or_indeterminate_fails_required_check_before_costly_steps(self):
        for result in ['failure', 'cancelled', 'skipped', '', 'success']:
            for merge in ['', 'verified-merge']:
                if result == 'success' and merge:
                    continue
                for changes in ['skipped', 'success', 'failure', '']:
                    with self.subTest(result=result, merge=merge, changes=changes):
                        self.assertTrue(self.condition('check', 'pull_request', result,
                                                       changes=changes, merge=merge))
                        self.assertNotEqual(self.admission('pull_request', result,
                                                          changes=changes, merge=merge), 0)
                        for job in ['changes', 'test']:
                            self.assertFalse(self.condition(job, 'pull_request', result,
                                                            changes=changes, merge=merge))
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        check = workflow.split('\n  check:\n', 1)[1].split('\n  test:\n', 1)[0]
        self.assertEqual(check.split('    steps:\n', 1)[1].splitlines()[0],
                         '      - name: Require valid classification')
        for job in ['changes', 'check', 'test']:
            body = re.split(r'\n  (?=\S)', workflow.split(f'\n  {job}:\n', 1)[1])[0]
            self.assertIn('preflight', re.search(r'^    needs: (.*)$', body, re.M)[1])

    def test_missing_verified_base_blocks_downstream_and_fails_check(self):
        self.assertNotEqual(self.admission('pull_request', 'success', base=''), 0)
        self.assertTrue(self.condition('check', 'pull_request', 'success', base=''))
        for job in ['changes', 'test']:
            self.assertFalse(self.condition(job, 'pull_request', 'success', base=''))

    def test_checkout_and_selector_share_verified_pair(self):
        workflow = (ROOT / '.github/workflows/ci.yml').read_text()
        for job in ['changes', 'check', 'test']:
            body = re.split(r'\n  (?=\S)', workflow.split(f'\n  {job}:\n', 1)[1])[0]
            self.assertIn('ref: ${{ needs.preflight.outputs.merge || github.sha }}', body)
        changes = workflow.split('\n  changes:\n', 1)[1].split('\n  check:\n', 1)[0]
        self.assertIn('PR_BASE: ${{ needs.preflight.outputs.base }}', changes)
        self.assertNotIn('github.event.pull_request.base.sha', changes)

    def test_failed_or_missing_classification_fails_check(self):
        for changes, mode in [('failure', 'all'), ('skipped', ''), ('', ''),
                              ('success', ''), ('success', 'unknown')]:
            with self.subTest(changes=changes, mode=mode):
                self.assertTrue(self.condition('check', 'pull_request', 'success',
                                               changes=changes, mode=mode))
                self.assertNotEqual(self.admission('pull_request', 'success',
                                                  changes=changes, mode=mode), 0)
                self.assertFalse(self.condition('test', 'pull_request', 'success',
                                                changes=changes, mode=mode))

    def test_matching_pair_keeps_required_jobs(self):
        for mode in ['packages', 'all', 'docs-only']:
            self.assertEqual(self.admission('pull_request', 'success', mode=mode), 0)
            for job in ['changes', 'check', 'test']:
                with self.subTest(mode=mode, job=job):
                    self.assertEqual(self.condition(job, 'pull_request', 'success',
                                                    mode=mode),
                                     job != 'test' or mode != 'docs-only')

    def test_push_keeps_all_jobs_despite_skipped_preflight(self):
        self.assertEqual(self.admission('push', 'skipped', merge=''), 0)
        for job in ['changes', 'check', 'test', 'ambient']:
            with self.subTest(job=job):
                self.assertTrue(self.condition(job, 'push', 'skipped'))

    def test_schedule_keeps_only_check(self):
        self.assertEqual(self.admission('schedule', 'skipped', changes='skipped',
                                        mode='', merge=''), 0)
        for job in ['changes', 'check', 'test', 'ambient']:
            with self.subTest(job=job):
                self.assertEqual(self.condition(job, 'schedule', 'skipped',
                                                changes='skipped', mode=''),
                                 job == 'check')


if __name__ == '__main__':
    unittest.main(verbosity=2)
