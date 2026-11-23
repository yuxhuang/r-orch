"""Synthetic SIGKILL and ownership controls; no product or real tmux is needed."""
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest

WATCHDOG = Path(__file__).with_name("orphan_watchdog.py")


def exited(fd, timeout=8):
    return bool(select.select([fd], [], [], timeout)[0])


def owner(root, short, executable):
    watch = subprocess.Popen(
        [sys.executable, "-I", "-S", str(WATCHDOG), root, short, executable],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, env={}, cwd="/",
    )
    assert watch.stdout.readline() == b"ready\n"
    child = subprocess.Popen(
        [executable, "600"], env={"ORCH_TEST_SOCKET_ROOT": short},
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    print(json.dumps([child.pid, watch.pid]), flush=True)
    # Only this process retains watch.stdin. Other children cannot keep it open.
    select.select([sys.stdin], [], [])


class WatchdogTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.exe = self.base / "fixture"
        shutil.copyfile("/bin/sleep", self.exe)
        self.exe.chmod(0o700)
        self.children = []
        self.fds = []
        self.addCleanup(self.cleanup_children)

    def cleanup_children(self):
        for child in self.children:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=8)
            if child.stdin:
                child.stdin.close()
            if child.stdout:
                child.stdout.close()
        for fd in self.fds:
            os.close(fd)

    def fixture(self, name):
        root, short = self.base / (name + "-root"), self.base / (name + "-short")
        root.mkdir()
        short.mkdir()
        child = subprocess.Popen(
            [sys.executable, "-I", "-S", __file__, "owner", str(root), str(short), str(self.exe)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, env={}, cwd="/",
        )
        self.children.append(child)
        self.assertTrue(select.select([child.stdout], [], [], 8)[0], "owner never became ready")
        pid, watchdog = json.loads(child.stdout.readline())
        fd, watch_fd = os.pidfd_open(pid), os.pidfd_open(watchdog)
        self.fds.extend([fd, watch_fd])
        # Independent emergency cleanup even if the cleanup under test is broken.
        self.addCleanup(self.kill_fixture, child, fd, watch_fd)
        return child, fd, watch_fd, root, short

    def kill_fixture(self, child, fd, watch_fd):
        if child.poll() is None:
            child.kill()
        child.wait(timeout=8)
        if not exited(watch_fd):
            signal.pidfd_send_signal(watch_fd, signal.SIGKILL)
        if not exited(fd, 0):
            signal.pidfd_send_signal(fd, signal.SIGKILL)

    def kill_owner(self, fixture):
        child, fd, watch_fd, root, short = fixture
        child.kill()
        child.wait(timeout=8)
        self.assertTrue(exited(watch_fd), "watchdog exceeded cleanup bound")
        self.assertTrue(exited(fd, 0), "owned child survived owner SIGKILL")
        self.assertFalse(root.exists() or short.exists(), "owned paths survived")

    def test_owner_sigkill_removes_owned_process_and_paths(self):
        self.kill_owner(self.fixture("victim"))

    def test_live_sibling_with_same_executable_survives(self):
        sibling = self.fixture("sibling")
        self.kill_owner(self.fixture("victim"))
        self.assertFalse(exited(sibling[1], 0), "live sibling was signalled")
        self.assertTrue(sibling[3].exists() and sibling[4].exists())

    def test_foreign_same_executable_without_marker_survives(self):
        child = subprocess.Popen([str(self.exe), "600"], env={})
        self.children.append(child)
        self.kill_owner(self.fixture("victim"))
        self.assertIsNone(child.poll(), "unmarked fixture was signalled")

    def test_matching_marker_with_wrong_executable_survives(self):
        victim = self.fixture("victim")
        child = subprocess.Popen(
            [sys.executable, "-I", "-S", "-c", "import time; time.sleep(600)"],
            env={"ORCH_TEST_SOCKET_ROOT": str(victim[4])},
        )
        self.children.append(child)
        self.kill_owner(victim)
        self.assertIsNone(child.poll(), "marker alone authorised signalling")

    def test_marker_prefix_is_not_ownership(self):
        victim = self.fixture("victim")
        child = subprocess.Popen(
            [str(self.exe), "600"],
            env={"ORCH_TEST_SOCKET_ROOT": str(victim[4]) + "-sibling"},
        )
        self.children.append(child)
        self.kill_owner(victim)
        self.assertIsNone(child.poll(), "a marker prefix authorised signalling")

    def test_replacement_directory_is_preserved(self):
        victim = self.fixture("victim")
        child, fd, watch_fd, root, short = victim
        root.rename(self.base / "original-root")
        root.mkdir()
        canary = root / "canary"
        canary.write_text("preserve")
        child.kill()
        child.wait(timeout=8)
        self.assertTrue(exited(watch_fd))
        self.assertEqual(canary.read_text(), "preserve")


if __name__ == "__main__":
    if sys.argv[1:2] == ["owner"]:
        owner(*sys.argv[2:])
    else:
        unittest.main()
