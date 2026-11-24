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
import time
import unittest

WATCHDOG = Path(__file__).with_name("orphan_watchdog.py")


def exited(fd, timeout=8):
    return bool(select.select([fd], [], [], timeout)[0])


def wait_ready(pid, fd, executable, timeout=8):
    expected = os.stat(executable)
    deadline = time.monotonic() + timeout
    while not exited(fd, 0):
        try:
            actual = os.stat(f"/proc/{pid}/exe")
            # Popen's exec handshake alone does not prove that a multicall
            # executable accepted argv[0]. Wait for the intended child to sleep.
            state = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]
            if (actual.st_dev, actual.st_ino) == (expected.st_dev, expected.st_ino) and state == "S":
                if not exited(fd, 0):
                    return
        except (FileNotFoundError, ProcessLookupError):
            pass
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise AssertionError("synthetic child never reached the expected sleeping executable")
        select.select([fd], [], [], min(0.01, remaining))
    raise AssertionError("synthetic child exited before readiness; check executable invocation")


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
        # Multicall implementations dispatch on the basename; keep "sleep"
        # while copying into a private directory for a unique executable inode.
        self.exe = self.base / "bin" / "sleep"
        self.exe.parent.mkdir()
        shutil.copyfile("/bin/sleep", self.exe)
        self.exe.chmod(0o700)
        self.assertFalse(os.path.samefile("/bin/sleep", self.exe))
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
        wait_ready(pid, fd, self.exe)
        self.assertFalse(exited(watch_fd, 0), "watchdog exited before owner readiness")
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

    def ready_child(self, child, executable):
        self.children.append(child)
        fd = os.pidfd_open(child.pid)
        self.fds.append(fd)
        wait_ready(child.pid, fd, executable)

    def test_readiness_rejects_a_child_that_exited_without_being_signalled(self):
        child = subprocess.Popen([sys.executable, "-I", "-S", "-c", "pass"], env={})
        self.children.append(child)
        fd = os.pidfd_open(child.pid)
        self.fds.append(fd)
        self.assertEqual(child.wait(timeout=8), 0)
        with self.assertRaisesRegex(AssertionError, "exited before readiness"):
            wait_ready(child.pid, fd, sys.executable)

    def test_readiness_rejects_a_live_child_with_the_wrong_executable(self):
        child = subprocess.Popen([str(self.exe), "600"], env={})
        self.ready_child(child, self.exe)
        with self.assertRaisesRegex(AssertionError, "never reached"):
            wait_ready(child.pid, self.fds[-1], sys.executable, timeout=0.05)

    def test_owner_sigkill_removes_owned_process_and_paths(self):
        self.kill_owner(self.fixture("victim"))

    def test_live_sibling_with_same_executable_survives(self):
        sibling = self.fixture("sibling")
        self.kill_owner(self.fixture("victim"))
        self.assertFalse(exited(sibling[1], 0), "live sibling was signalled")
        self.assertTrue(sibling[3].exists() and sibling[4].exists())

    def test_foreign_same_executable_without_marker_survives(self):
        child = subprocess.Popen([str(self.exe), "600"], env={})
        self.ready_child(child, self.exe)
        self.kill_owner(self.fixture("victim"))
        self.assertIsNone(child.poll(), "unmarked fixture was signalled")

    def test_matching_marker_with_wrong_executable_survives(self):
        victim = self.fixture("victim")
        child = subprocess.Popen(
            [sys.executable, "-I", "-S", "-c", "import time; time.sleep(600)"],
            env={"ORCH_TEST_SOCKET_ROOT": str(victim[4])},
        )
        self.ready_child(child, sys.executable)
        self.kill_owner(victim)
        self.assertIsNone(child.poll(), "marker alone authorised signalling")

    def test_marker_prefix_is_not_ownership(self):
        victim = self.fixture("victim")
        child = subprocess.Popen(
            [str(self.exe), "600"],
            env={"ORCH_TEST_SOCKET_ROOT": str(victim[4]) + "-sibling"},
        )
        self.ready_child(child, self.exe)
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
