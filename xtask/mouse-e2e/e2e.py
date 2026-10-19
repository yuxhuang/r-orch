#!/usr/bin/env python3
"""TUI mouse end-to-end harness. See README.md.

Three environments drive the real `orch tui` and read the rendered screen:
  tmux-sgr   raw SGR mouse bytes injected into the app's pane input
  tmux-fwd   a tmux client in a Python PTY; tmux translates and forwards (mouse on;
             the same checks with mouse off are reported as INFO)
  xterm-web  `orch web` + headless Chromium; real mouse events on xterm.js
"""
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time

ORCH = os.environ["ORCH_BIN"]
SOCK = "orch-e2e"  # dedicated tmux socket
COLS, ROWS = 120, 40

# ---- Targets: adjust here when the design changes ---------------------------------
# (header label, active-tab text once switched, keyboard equivalent)
TAB_TARGETS = [("agents", "AGENTS", "a"), ("goals", "GOALS", "Left")]
FIRST, SECOND = "First task", "Second task"  # titles of the two named seeded tasks
# Plus fillers, so the list is long enough for the wheel cap to be observable
# without the selection clamping at the bottom first. Kept under
# TASK_LIST_LIMIT (10) so the collapsed list shows every one of them and no
# `Show all` pseudo-row appears.
FILLERS = 6
OVERLAY_TITLE = "KEYBOARD SHORTCUTS"          # opened with `?`
OVERLAY_INSIDE_LABEL = "NAVIGATION"            # a cell known to lie inside the overlay
# Wheel-down over the task list is expected to move the selection down one row,
# wheel-up back up (see wheel_check).
# The per-frame wheel cap (orch_tui::mouse::WHEEL_MAX_STEPS_PER_FRAME). A flick
# delivered in one write must move the selection by exactly this much — which
# is the point of the check: alternate-scroll arrow translation gives one step
# per notch, so it cannot produce this number.
WHEEL_CAP = 3
WHEEL_BURST = 10
# ------------------------------------------------------------------------------------

ROOT = None


def log(*a):
    print(*a, file=sys.stderr, flush=True)


# ---- hermetic project ---------------------------------------------------------------
def orch_env(home):
    env = dict(os.environ)
    env.update(HOME=home, XDG_CONFIG_HOME=home + "/.config", XDG_DATA_HOME=home + "/.local/share",
               XDG_CACHE_HOME=home + "/.cache", XDG_STATE_HOME=home + "/.local/state", NO_COLOR="1")
    return env


def make_project(name):
    base = os.path.join(ROOT, name)
    proj, home = base + "/proj", base + "/home"
    os.makedirs(proj)
    os.makedirs(home)
    env = orch_env(home)

    def run(*a):
        subprocess.run([ORCH, *a], cwd=proj, env=env, check=True, stdout=subprocess.DEVNULL,
                       stderr=subprocess.PIPE)
    run("init", "--name", "e2e", "--adapter", "shell")
    run("agent", "add", "Alpha", "--adapter", "shell", "--command", "true")
    run("agent", "add", "Beta", "--adapter", "shell", "--command", "true")
    run("goal", "add", "Ship goal")
    run("task", "add", FIRST)
    run("task", "add", SECOND)
    for i in range(FILLERS):
        run("task", "add", "Filler task %d" % i)
    return proj, home, env


# ---- screen parsing -----------------------------------------------------------------
def find(lines, text):
    for r, l in enumerate(lines):
        if text in l:
            return r, l.index(text)
    return None


def header_row(lines):
    for r, l in enumerate(lines):
        if "ORCH" in l and re.search(r"\b[GTAL] [A-Za-z]+", l):
            return r
    return None


def active_tab(lines):
    r = header_row(lines)
    if r is None:
        return None
    up = [x for x in re.findall(r"\b[GTAL] ([A-Za-z]+)", lines[r]) if x.isupper()]
    return up[0] if up else None


def selected(lines):
    sel = [l for l in lines if l.lstrip().startswith("▸")]
    return sel[0] if sel else ""


def overlay_open(lines):
    return any(OVERLAY_TITLE in l for l in lines)


def detail_title(lines):
    for l in lines:
        m = re.search(r"DETAIL ━ (.*?)(\s{2,}|$)", l)
        if m:
            return m.group(1).strip()
    return None


def wait_for(env, pred, timeout=1.5):
    end = time.time() + timeout
    while True:
        lines = env.screen()
        if pred(lines):
            return True, lines
        if time.time() > end:
            return False, lines
        time.sleep(0.1)


# ---- environments -------------------------------------------------------------------
def sgr(btn, col, row, release=False):
    """col,row are 0-based cells; SGR reports are 1-based."""
    return "\x1b[<%d;%d;%d%s" % (btn, col + 1, row + 1, "m" if release else "M")


class TmuxBase:
    count = 0
    mouse = False
    needs_reporting = True
    # A burst arrives as one write, so the cap is exactly observable.
    atomic_burst = True

    def tmux(self, *a, check=True):
        r = subprocess.run(["tmux", "-L", SOCK, "-f", "/dev/null", *a], capture_output=True, text=True)
        if check and r.returncode:
            raise RuntimeError("tmux %s: %s" % (a, r.stderr))
        return r.stdout

    def start(self):
        TmuxBase.count += 1
        self.sess = "s%d" % TmuxBase.count
        proj, home, _ = make_project("%s-%d" % (self.name, TmuxBase.count))
        self.tmux("new-session", "-d", "-s", self.sess, "-x", str(COLS), "-y", str(ROWS), "-c", proj,
                  "exec env HOME={h} XDG_CONFIG_HOME={h}/.config XDG_DATA_HOME={h}/.local/share "
                  "XDG_CACHE_HOME={h}/.cache NO_COLOR=1 {o} tui --observe".format(h=home, o=ORCH))
        self.tmux("set-option", "-g", "status", "off")
        self.tmux("set-option", "-g", "mouse", "on" if self.mouse else "off")
        self.after_session()
        ok, _ = wait_for(self, lambda L: find(L, "ORCH"), 15)
        if not ok:
            raise RuntimeError("orch tui did not render")

    def after_session(self):
        pass

    def screen(self):
        return self.tmux("capture-pane", "-p", "-t", self.sess).split("\n")

    def key(self, k):
        if len(k) == 1:
            self.tmux("send-keys", "-t", self.sess, "-l", k)
        else:
            self.tmux("send-keys", "-t", self.sess, k)

    def click(self, col, row):
        self.inject(sgr(0, col, row) + sgr(0, col, row, True))

    def wheel(self, col, row, down):
        self.inject(sgr(65 if down else 64, col, row))

    def wheel_burst(self, col, row, down, notches):
        # One write, so every notch reaches the app inside a single event
        # drain and the per-frame cap is what decides the result.
        self.inject(sgr(65 if down else 64, col, row) * notches)

    def mouse_reporting(self):
        out = self.tmux("display-message", "-p", "-t", self.sess,
                        "#{mouse_any_flag} #{mouse_sgr_flag}").split()
        return out == ["1", "1"], "tmux pane mouse_any/mouse_sgr flags = %s" % " ".join(out)

    def stop(self):
        # `orch tui` now exits on its own when the pty hangs up (a defect
        # this harness found), so `kill-session` is enough. The explicit
        # kill stays as a belt: a check that wedged the app mid-wizard should
        # not leave a pane behind for the next one.
        pids = self.tmux("list-panes", "-s", "-t", self.sess, "-F", "#{pane_pid}", check=False).split()
        self.tmux("kill-session", "-t", self.sess, check=False)
        for pid in pids:
            try:
                os.kill(int(pid), signal.SIGKILL)
            except (OSError, ValueError):
                pass


class TmuxSgr(TmuxBase):
    """Bytes go straight into the app's input (tmux send-keys -l): independent of tmux mouse mode."""
    name = "tmux-sgr"
    needs_reporting = False

    def inject(self, data):
        self.tmux("send-keys", "-t", self.sess, "-l", data)


class TmuxFwd(TmuxBase):
    """tmux client attached inside a Python-driven PTY; we write what an outer terminal would."""

    def __init__(self, mouse):
        self.mouse = mouse
        self.name = "tmux-fwd" if mouse else "tmux-fwd-off"

    def after_session(self):
        import fcntl
        import pty
        import struct
        import termios
        pid, fd = pty.fork()
        if pid == 0:
            os.environ["TERM"] = "xterm-256color"
            os.execvp("tmux", ["tmux", "-L", SOCK, "-f", "/dev/null", "attach", "-t", self.sess])
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        self.pid, self.fd = pid, fd
        self.stop_reader = False

        def drain():
            while not self.stop_reader:
                try:
                    if not os.read(fd, 65536):
                        return
                except OSError:
                    return
        threading.Thread(target=drain, daemon=True).start()
        time.sleep(0.5)

    def inject(self, data):
        os.write(self.fd, data.encode())

    def stop(self):
        self.stop_reader = True
        try:
            os.kill(self.pid, signal.SIGTERM)
            os.waitpid(self.pid, 0)
            os.close(self.fd)
        except OSError:
            pass
        super().stop()


class WebEnv:
    name = "xterm-web"
    needs_reporting = True
    # Each notch is a separate CDP round trip, so a burst can straddle a frame
    # boundary and spend more than one frame's budget.
    atomic_burst = False
    server = pw = browser = url = None

    @classmethod
    def boot(cls):
        from playwright.sync_api import sync_playwright
        proj, home, env = make_project("web")
        cls.server = subprocess.Popen([ORCH, "web", "--bind", "127.0.0.1:0", "--observe"], cwd=proj,
                                      env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        deadline = time.time() + 15
        while time.time() < deadline:
            m = re.search(r"listening on (http://\S+)", cls.server.stdout.readline())
            if m:
                cls.url = m.group(1)
                break
        if not cls.url:
            raise RuntimeError("orch web did not report a listening URL")
        # keep draining so the pipe never blocks the server
        threading.Thread(target=lambda: [None for _ in cls.server.stdout], daemon=True).start()
        cls.pw = sync_playwright().start()
        cls.browser = cls.pw.chromium.launch(headless=True)

    @classmethod
    def shutdown(cls):
        for f in (lambda: cls.browser.close(), lambda: cls.pw.stop(), lambda: cls.server.terminate()):
            try:
                f()
            except Exception:
                pass
        if cls.server:
            try:
                cls.server.wait(5)
            except subprocess.TimeoutExpired:
                cls.server.kill()

    def start(self):
        self.page = self.browser.new_page(viewport={"width": 1100, "height": 760})
        self.page.goto(self.url)
        self.page.wait_for_function("typeof term !== 'undefined' && term.cols > 0")
        ok, _ = wait_for(self, lambda L: find(L, "ORCH"), 15)
        if not ok:
            raise RuntimeError("orch tui did not render in xterm.js")
        self.page.mouse.move(5, 5)

    def screen(self):
        return self.page.evaluate("""() => { const b = term.buffer.active, o = [];
            for (let i = 0; i < term.rows; i++) { const l = b.getLine(b.viewportY + i); o.push(l ? l.translateToString(true) : ''); }
            return o; }""")

    def xy(self, col, row):
        g = self.page.evaluate("""() => { const r = document.querySelector('.xterm-screen').getBoundingClientRect();
            return {x: r.x, y: r.y, w: r.width, h: r.height, c: term.cols, r: term.rows}; }""")
        return g["x"] + (col + 0.5) * g["w"] / g["c"], g["y"] + (row + 0.5) * g["h"] / g["r"]

    def key(self, k):
        names = {"Down": "ArrowDown", "Up": "ArrowUp", "Left": "ArrowLeft"}
        if len(k) == 1:
            self.page.keyboard.type(k)
        else:
            self.page.keyboard.press(names.get(k, k))

    def click(self, col, row):
        x, y = self.xy(col, row)
        self.page.mouse.click(x, y)

    def wheel(self, col, row, down):
        x, y = self.xy(col, row)
        self.page.mouse.move(x, y)
        self.page.mouse.wheel(0, 100 if down else -100)

    def wheel_burst(self, col, row, down, notches):
        x, y = self.xy(col, row)
        self.page.mouse.move(x, y)
        for _ in range(notches):
            self.page.mouse.wheel(0, 100 if down else -100)

    def drag(self, c0, r0, c1, r1, modifier=None):
        x0, y0 = self.xy(c0, r0)
        x1, y1 = self.xy(c1, r1)
        if modifier:
            self.page.keyboard.down(modifier)
        self.page.mouse.move(x0, y0)
        self.page.mouse.down()
        self.page.mouse.move((x0 + x1) / 2, y1, steps=4)
        self.page.mouse.move(x1, y1, steps=4)
        self.page.mouse.up()
        if modifier:
            self.page.keyboard.up(modifier)
        return self.page.evaluate("term.getSelection()")

    def clear_selection(self):
        self.page.evaluate("term.clearSelection()")

    def mouse_reporting(self):
        mode = self.page.evaluate("term.modes.mouseTrackingMode")
        return mode != "none", "xterm.js mouseTrackingMode = %s" % mode

    def stop(self):
        try:
            self.page.close()
        except Exception:
            pass


# ---- checks: (name, kind, fn(env) -> (ok, detail)) ----------------------------------
# kind "control" = keyboard equivalent (proves the harness reads the screen);
#      "mouse"   = the behaviour under test.
def other_task(lines):
    return FIRST if SECOND in selected(lines) else SECOND


def tab_check(label, active, key, mouse):
    def fn(env):
        L = env.screen()
        if mouse:
            hr = header_row(L)
            if hr is None or label not in L[hr]:
                return False, "label %r not found in header" % label
            env.click(L[hr].index(label) + 1, hr)
        else:
            env.key(key)
        ok, L = wait_for(env, lambda L: active_tab(L) == active)
        return ok, "active tab = %r, want %r" % (active_tab(L), active)
    return fn


def select_check(mouse):
    def fn(env):
        L = env.screen()
        want = other_task(L)
        r, c = find(L, want)
        if mouse:
            env.click(c + 2, r)
        else:
            # Walk to the same row with the arrows, however far away the
            # fixture puts it, so the control check does not depend on the
            # target happening to sit one row below the selection.
            for _ in range(abs(r - sel_index(L))):
                env.key("Down" if r > sel_index(env.screen()) else "Up")
        ok, L = wait_for(env, lambda L: want in selected(L))
        if not ok:
            return False, "selected row = %r, want one containing %r" % (selected(L).strip()[:40], want)
        env.key("Enter")
        ok, L = wait_for(env, lambda L: detail_title(L) == want)
        return ok, "detail title = %r, want %r" % (detail_title(L), want)
    return fn


def sel_index(lines):
    for i, l in enumerate(lines):
        if l.lstrip().startswith("▸"):
            return i
    return -1


def wheel_check(mouse):
    def fn(env):
        L = env.screen()
        start = sel_index(L)
        r, c = find(L, other_task(L))
        if mouse:
            env.wheel(c + 2, r, True)
        else:
            env.key("Down")
        ok, L = wait_for(env, lambda L: sel_index(L) > start)
        if not ok:
            return False, "after wheel-down selection row %d, want > %d" % (sel_index(L), start)
        mid = sel_index(L)
        if mouse:
            env.wheel(c + 2, r, False)
        else:
            env.key("Up")
        ok, L = wait_for(env, lambda L: 0 <= sel_index(L) < mid)
        if not ok:
            return False, "after wheel-up selection row %d, want < %d" % (sel_index(L), mid)
        # xterm.js / tmux turn wheel into Up/Down keys when the app has not enabled mouse
        # reporting; that must not count as mouse support.
        if mouse and env.needs_reporting:
            rep, why = env.mouse_reporting()
            if not rep:
                return False, "selection moved but app never enabled mouse reporting (%s): arrow-key fallback" % why
        return True, "selection moved down then up"
    return fn


def wheel_cap(env):
    """A flick must spend the per-frame budget and no more.

    This is the check that cannot false-green on alternate scroll: with mouse
    reporting off, tmux and xterm.js translate the wheel into Up/Down arrows,
    which move the selection one row per notch. WHEEL_BURST notches would then
    move WHEEL_BURST rows, not WHEEL_CAP.
    """
    L = env.screen()
    start = sel_index(L)
    r, c = find(L, other_task(L))
    if not env.atomic_burst:
        # Each notch is its own round trip here, so the burst straddles frame
        # boundaries and spends several budgets — this environment cannot
        # answer "exactly one frame's worth". It does not need to: the
        # false-green this check exists to rule out is alternate-scroll arrow
        # translation, and `app-enables-mouse-reporting` rules that out
        # directly here by reading xterm.js's own tracking mode.
        env.wheel_burst(c + 2, r, True, WHEEL_BURST)
        time.sleep(0.8)
        moved = sel_index(env.screen()) - start
        return None, ("%d notches moved %d row(s); a browser cannot deliver a burst "
                      "atomically, so the cap is asserted in the tmux environments"
                      % (WHEEL_BURST, moved))
    env.wheel_burst(c + 2, r, True, WHEEL_BURST)
    time.sleep(0.8)
    moved = sel_index(env.screen()) - start
    ok = moved == WHEEL_CAP
    return ok, "%d notches in one write moved the selection %d row(s), want exactly %d" % (
        WHEEL_BURST, moved, WHEEL_CAP)


def selection_shift_drag(env):
    """Shift+drag must still select text while the app holds mouse reporting.

    A hard gate: if it fails, the answer is documentation plus the Option path
    plus F2 — never a relaxed assertion here.
    """
    env.clear_selection()
    plain = env.drag(4, 6, 30, 6)
    if plain.strip():
        return False, "a plain drag selected %r, so the app is not holding reporting" % plain[:40]
    env.clear_selection()
    shifted = env.drag(4, 6, 30, 6, modifier="Shift")
    if not shifted.strip():
        return False, "Shift+drag selected nothing while reporting is on"
    return True, "plain drag selects nothing; Shift+drag selected %r" % shifted.strip()[:40]


def selection_option_drag(env):
    """`macOptionClickForcesSelection: true` — the macOS half of the gate.

    xterm.js gates this on the platform: in `xterm.js` the force-selection
    modifier is `isMac ? e.altKey && options.macOptionClickForcesSelection
    : e.shiftKey`. So Option+drag can only be observed on macOS, and on every
    other platform the *same* option is what makes `selection.shift-drag`
    work — which is the half this harness can prove. Reported as INFO off
    macOS rather than asserted, because a FAIL here would mean "this is not a
    Mac", not "the feature is broken".
    """
    if sys.platform != "darwin":
        return None, ("not macOS: xterm.js routes force-selection to Shift here, "
                      "covered by selection.shift-drag")
    env.clear_selection()
    alt = env.drag(4, 6, 30, 6, modifier="Alt")
    ok = bool(alt.strip())
    detail = ("Option+drag selected %r" % alt.strip()[:40]) if ok else \
        "Option+drag selected nothing despite macOptionClickForcesSelection"
    return ok, detail


def overlay_close_key(env):
    env.key("?")
    ok, L = wait_for(env, overlay_open)
    if not ok:
        return False, "overlay did not open on '?'"
    env.key("Escape")
    ok, L = wait_for(env, lambda L: not overlay_open(L))
    return ok, "overlay closed by Escape" if ok else "overlay still open after Escape"


def overlay_inside(env):
    """A click inside the box must not dismiss — and must be a *click*.

    On its own this cannot fail: the overlay also stays open when the app
    ignores the mouse entirely, so it would pass against a revert. It is a
    real gate only when paired with evidence that clicks are reaching the
    app, so it asserts that too: the overlay is still open AND a click
    outside it does dismiss, in the same session. Reverting mouse handling
    fails the second half.
    """
    # The outside target has to be located *before* the overlay covers it.
    L0 = env.screen()
    row, col = find(L0, other_task(L0))
    env.key("?")
    ok, L = wait_for(env, overlay_open)
    if not ok:
        return False, "overlay did not open on '?'"
    r, c = find(L, OVERLAY_INSIDE_LABEL)
    env.click(c + 1, r)
    time.sleep(0.6)
    if not overlay_open(env.screen()):
        return False, "click inside the overlay dismissed it"
    # The control half: the same session, a click outside the box.
    env.click(col + 2, row)
    ok, _ = wait_for(env, lambda L: not overlay_open(L))
    if not ok:
        return False, ("overlay stayed open after a click inside, but a click outside did not "
                       "dismiss it either — clicks are not reaching the app, so the first half "
                       "proves nothing")
    return True, "click inside kept it open; click outside dismissed it (so clicks do arrive)"


def overlay_outside(env):
    L0 = env.screen()
    r, c = find(L0, other_task(L0))  # a list row, which the overlay does not cover
    env.key("?")
    ok, L = wait_for(env, overlay_open)
    if not ok:
        return False, "overlay did not open on '?'"
    env.click(c + 2, r)
    ok, L = wait_for(env, lambda L: not overlay_open(L))
    return ok, "overlay closed by click outside" if ok else "overlay still open after click outside"


def mouse_reporting(env):
    return env.mouse_reporting()


CHECKS = []
for _label, _active, _key in TAB_TARGETS:
    CHECKS += [("tab-%s.keyboard" % _label, "control", tab_check(_label, _active, _key, False)),
               ("tab-%s.click" % _label, "mouse", tab_check(_label, _active, _key, True))]
CHECKS += [
    ("row-select.keyboard", "control", select_check(False)),
    ("row-select.click", "mouse", select_check(True)),
    ("wheel.keyboard", "control", wheel_check(False)),
    ("wheel.scroll", "mouse", wheel_check(True)),
    ("wheel.cap", "mouse", wheel_cap),
    ("overlay-close.keyboard", "control", overlay_close_key),
    ("overlay.click-inside-stays-open", "mouse", overlay_inside),
    ("overlay.click-outside-closes", "mouse", overlay_outside),
    ("app-enables-mouse-reporting", "mouse", mouse_reporting),
    ("selection.shift-drag", "mouse", selection_shift_drag),
    ("selection.option-drag", "mouse", selection_option_drag),
]

# Checks only one environment can answer.
WEB_ONLY = {"selection.shift-drag", "selection.option-drag"}


def run_env(factory, info_only=False):
    results = []
    for name, kind, fn in CHECKS:
        env = factory()
        if name == "app-enables-mouse-reporting" and isinstance(env, TmuxSgr):
            continue  # the flag is only meaningful where tmux mediates (tmux-fwd) or xterm.js does
        if name in WEB_ONLY and not isinstance(env, WebEnv):
            continue  # only a real browser can report a terminal text selection
        try:
            env.start()
            ok, detail = fn(env)
        except Exception as e:  # harness/environment fault: always a FAIL, tagged
            ok, detail = False, "HARNESS ERROR: %s: %s" % (type(e).__name__, e)
        finally:
            try:
                env.stop()
            except Exception:
                pass
        if ok is None:
            # The check cannot be answered on this platform; it says why.
            tag = "INFO"
        elif info_only and kind == "mouse":
            tag, detail = "INFO", ("reacted" if ok else "no reaction") + " | " + detail
        else:
            tag = "PASS" if ok else "FAIL"
        print("%s %s/%s [%s] %s" % (tag, env.name, name, kind, detail), flush=True)
        results.append(tag)
    return results


def main():
    global ROOT
    want = None
    if "--only" in sys.argv:
        want = sys.argv[sys.argv.index("--only") + 1].split(",")
    base = os.environ.get("E2E_ROOT") or tempfile.gettempdir()
    os.makedirs(base, exist_ok=True)
    ROOT = tempfile.mkdtemp(prefix="run-", dir=base)
    log("hermetic root: %s (real HOME %s is never passed to orch)" % (ROOT, os.path.expanduser("~")))
    allres = []
    try:
        if want is None or "tmux-sgr" in want:
            allres += run_env(TmuxSgr)
        if want is None or "tmux-fwd" in want:
            allres += run_env(lambda: TmuxFwd(True))
            allres += run_env(lambda: TmuxFwd(False), info_only=True)
        if want is None or "xterm-web" in want:
            try:
                WebEnv.boot()
                allres += run_env(WebEnv)
            except Exception as e:
                print("FAIL xterm-web/boot [env] HARNESS ERROR: %s: %s" % (type(e).__name__, e), flush=True)
                allres.append("FAIL")
            finally:
                WebEnv.shutdown()
    finally:
        subprocess.run(["tmux", "-L", SOCK, "kill-server"], capture_output=True)
        if not os.environ.get("KEEP"):
            shutil.rmtree(ROOT, ignore_errors=True)
    f = allres.count("FAIL")
    print("\n%d passed, %d failed, %d informational" % (allres.count("PASS"), f, allres.count("INFO")), flush=True)
    sys.exit(1 if f else 0)


main()
