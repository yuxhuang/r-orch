#!/usr/bin/env python3
"""Real Git fixtures for the PR preflight; no GitHub or Rust build needed."""
import os
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

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.repo,
                                       env=self.env, text=True).strip()

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
        result = subprocess.run(['bash', str(SCRIPT)], cwd=self.repo,
                                env=dict(self.env, PR_BASE_REF='main', PR_NUMBER='7',
                                         GITHUB_OUTPUT=str(self.output), **env),
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
        self.assertEqual(self.output.read_text(), f'merge={self.merge}\n')
        for sha in [self.base, self.head, self.merge]:
            self.assertIn(sha, result.stdout)

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
        self.assertEqual(self.output.read_text(), f'merge={self.merge}\n')

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
    def condition(self, job, event, preflight, changes='success', mode='all'):
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
                preflight=SimpleNamespace(result=preflight),
                changes=SimpleNamespace(result=changes,
                                        outputs=SimpleNamespace(mode=mode))),
            always=lambda: True, cancelled=lambda: False)
        expression = expression.replace('&&', ' and ').replace('||', ' or ')
        expression = re.sub(r'!(?!=)', ' not ', expression)
        return bool(eval(expression, {'__builtins__': {}}, scope))

    def test_stale_or_indeterminate_blocks_all_expensive_jobs(self):
        for result in ['failure', 'cancelled', 'skipped', '']:
            for job in ['changes', 'check', 'test']:
                with self.subTest(result=result, job=job):
                    self.assertFalse(self.condition(job, 'pull_request', result))

    def test_matching_pair_keeps_required_jobs(self):
        for mode in ['packages', 'all', 'docs-only']:
            for job in ['changes', 'check', 'test']:
                with self.subTest(mode=mode, job=job):
                    self.assertEqual(self.condition(job, 'pull_request', 'success',
                                                    mode=mode),
                                     job != 'test' or mode != 'docs-only')

    def test_push_keeps_all_jobs_despite_skipped_preflight(self):
        for job in ['changes', 'check', 'test', 'ambient']:
            with self.subTest(job=job):
                self.assertTrue(self.condition(job, 'push', 'skipped'))

    def test_schedule_keeps_only_check(self):
        for job in ['changes', 'check', 'test', 'ambient']:
            with self.subTest(job=job):
                self.assertEqual(self.condition(job, 'schedule', 'skipped',
                                                changes='skipped', mode=''),
                                 job == 'check')


if __name__ == '__main__':
    unittest.main(verbosity=2)
