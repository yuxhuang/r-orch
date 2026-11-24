"""Linux test-fixture backstop; invoked with python -I -S, never installed.

The parent alone holds stdin's write end (CLOEXEC). EOF covers SIGKILL without
polling a reusable parent PID. No process is selected by name or argv: require
the exact private socket-root environment entry AND an allowed executable inode.
pidfds pin that process across the final identity check and signal.
"""
import os
import select
import shutil
import signal
import stat
import sys
import time


def identity(path):
    info = os.stat(path)
    return info.st_dev, info.st_ino


root, short, *executables = sys.argv[1:]
directories = [(path, identity(path)) for path in (root, short)]
allowed = {identity(path) for path in executables}
marker = os.fsencode("ORCH_TEST_SOCKET_ROOT=" + short)


def owned(pid):
    try:
        if identity(f"/proc/{pid}/exe") not in allowed:
            return False
        with open(f"/proc/{pid}/environ", "rb") as env:
            # A fixture has a tiny cleared environment. Fail closed on overflow.
            data = env.read(65537)
        return len(data) <= 65536 and marker in data.split(b"\0")
    except (OSError, ValueError):
        return False


# Refuse the fixture before launching anything if this kernel lacks pidfds.
probe = os.pidfd_open(os.getpid())
signal.pidfd_send_signal(probe, 0)
os.close(probe)
print("ready", flush=True)
while os.read(0, 1):
    pass

deadline = time.monotonic() + 5
while time.monotonic() < deadline:
    victims = []
    # /proc is local kernel metadata; bound both entries and per-entry bytes.
    with os.scandir("/proc") as entries:
        for index, entry in enumerate(entries):
            if index >= 100000 or time.monotonic() >= deadline:
                break
            if not entry.name.isdecimal() or not owned(entry.name):
                continue
            fd = None
            try:
                fd = os.pidfd_open(int(entry.name))
                # PID reuse before open must not admit another executable/marker.
                if owned(entry.name):
                    signal.pidfd_send_signal(fd, signal.SIGKILL)
                    victims.append(fd)
                else:
                    os.close(fd)
            except (OSError, ValueError):
                if fd is not None and fd not in victims:
                    os.close(fd)
    if not victims:
        break
    for fd in victims:
        select.select([fd], [], [], max(0, deadline - time.monotonic()))
        os.close(fd)

# Do not remove a replacement path or follow a symlink at an owned root.
# shutil.rmtree uses its descriptor-relative symlink-safe implementation here.
assert shutil.rmtree.avoids_symlink_attacks
for path, expected in directories:
    try:
        info = os.lstat(path)
        if stat.S_ISDIR(info.st_mode) and (info.st_dev, info.st_ino) == expected:
            shutil.rmtree(path)
    except FileNotFoundError:
        pass
