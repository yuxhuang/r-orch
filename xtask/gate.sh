#!/usr/bin/env bash
# full (default) is the goal/release gate; pr is the merge condition after the
# policy transition. fast is PR2. Incident protections I1-I24 live in Rust:
# content freshness, isolated targets, sentinel HOME/shims outside the workspace,
# named nonzero inventory, actual suite status, metadata-only real-home check,
# timed retained logs and ENV-LIMITED advisories. --warm is developer-only.
# No EXIT or signal trap deletes a target. Bootstrap owns/reaps its direct cargo
# child; its kernel lock is inherited through stdin and never explicitly unlocked.
# D45 uses checked paths then by-name operations under the cooperative lease.
# A same-account racing replacement can redirect a mutation (accepted interim
# risk). FS1 escaped descendants and manual cargo-clean remain separate risks.
set -euo pipefail
cd "$(dirname "$0")/.."
exec /usr/bin/python3 - "$@" <<'PYTHON'
import fcntl, os, pathlib, re, signal, stat, subprocess, sys, time
repo = pathlib.Path.cwd()
target = repo / 'target'
boot = target / 'gate-xtask'
stamp = boot / 'judge-id'
uid = os.geteuid()
child = None
interrupted = None
locks = []
def receive(number, frame):
    global interrupted
    if interrupted is None: interrupted = number
    if child is not None and child.poll() is None:
        child.send_signal(number)
for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP): signal.signal(number, receive)
def check(path, kind):
    m = path.lstat()
    if m.st_uid != uid or (kind == 'dir' and not stat.S_ISDIR(m.st_mode)) or (kind == 'file' and (not stat.S_ISREG(m.st_mode) or m.st_nlink != 1)):
        raise RuntimeError('path link/owner/type refused: ' + str(path))
    return m
def directory(path):
    if not os.path.lexists(path): path.mkdir(mode=0o700)
    check(path, 'dir')
def read(path, limit=16*1024*1024):
    before = check(path, 'file')
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as f:
        now = os.fstat(f.fileno())
        if (before.st_dev,before.st_ino) != (now.st_dev,now.st_ino): raise RuntimeError('file changed')
        b = f.read(limit+1)
    if len(b)>limit: raise RuntimeError('source budget')
    return b
def git(*args, data=None):
    r = subprocess.run(['git', '-C', str(repo), *args], input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if r.returncode: raise RuntimeError('git input failed: ' + r.stderr.decode(errors='replace'))
    return r.stdout
def encode(p):
    if all(0x21<=b<=0x7e and b not in b'"\\' for b in p): return p
    out=b'"'
    for b in p:
        out += {34:b'\\"',92:b'\\\\',9:b'\\t',10:b'\\n',13:b'\\r'}.get(b,bytes([b]) if 0x20<=b<=0x7e else ('\\%03o'%b).encode())
    return out+b'"'
def identity():
    paths=git('ls-files','-z','--cached','--others','--exclude-standard','--','xtask','Cargo.toml','Cargo.lock','rust-toolchain.toml','.cargo/config.toml').split(b'\0')
    if len(paths)>4096: raise RuntimeError('judge source count budget')
    records=[]
    for p in sorted(set(filter(None,paths))):
        name=os.fsdecode(p)
        if any(x in ('','..','.') for x in name.split('/')): raise RuntimeError('source path')
        path=repo/name
        for parent in reversed(path.parents):
            if parent == repo or repo in parent.parents: check(parent,'dir')
        blob=git('hash-object','--stdin',data=read(path)).strip()
        records.append(blob+b' '+encode(p)+b'\n')
    if not records: raise RuntimeError('empty judge source set')
    return git('hash-object','--stdin',data=b''.join(records)).strip()
def lock(path):
    if os.path.lexists(path):
        m=check(path,'file')
        if m.st_mode & 0o77: raise RuntimeError('lock mode')
    fd=os.open(path,os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW|os.O_NONBLOCK,0o600)
    locks.append((fd,path))
    held=os.fstat(fd); now=check(path,'file')
    if (held.st_dev,held.st_ino)!=(now.st_dev,now.st_ino): raise RuntimeError('lock replaced')
    end=time.monotonic()+1800
    while True:
        if interrupted: raise RuntimeError('INTERRUPTED waiting for bootstrap lock')
        try: fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB); break
        except BlockingIOError:
            if time.monotonic()>=end: raise RuntimeError('bootstrap lease busy')
            time.sleep(.01)
    return fd
def validate_locks():
    for fd,path in locks:
        held=os.fstat(fd);now=check(path,'file')
        if now.st_mode & 0o77 or (held.st_dev,held.st_ino)!=(now.st_dev,now.st_ino): raise RuntimeError('bootstrap lease/marker lost: '+str(path))
def remove(path, budget):
    validate_locks()
    m=path.lstat();budget[0]+=1
    if budget[0]>1000000: raise RuntimeError('bootstrap removal budget')
    if m.st_uid!=uid: raise RuntimeError('bootstrap entry owner')
    if stat.S_ISDIR(m.st_mode):
        for p in path.iterdir(): remove(p,budget)
        check(path,'dir');path.rmdir()
    elif stat.S_ISREG(m.st_mode) or stat.S_ISLNK(m.st_mode): path.unlink()
    else: raise RuntimeError('bootstrap special entry')
try:
    lease_dir=pathlib.Path(os.environ.get('XTASK_GATE_LEASE_DIR','/tmp')).resolve(strict=True)
    lock(lease_dir/'r-orch-gate.turnstile')
    lock(lease_dir/'r-orch-gate.lease')
    directory(target)
    marker=lock(target/'.gate-xtask.lock')
    validate_locks()
    if os.path.lexists(boot): check(boot,'dir')
    if os.path.lexists(stamp):
        previous=read(stamp,128).strip();stamp.unlink()
    else: previous=None
    before=identity()
    if previous!=before or not (boot/'debug/xtask').is_file():
        if os.path.lexists(boot): remove(boot,[0])
        directory(boot)
    os.environ['CARGO_TARGET_DIR']=str(boot)
    os.environ['LC_ALL']='C'
    pin=re.findall(r'^\s*channel\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"\s*$',read(repo/'rust-toolchain.toml').decode(),re.M)
    if len(pin)!=1: raise RuntimeError('numeric pinned toolchain required')
    os.environ['RUSTUP_TOOLCHAIN']=pin[0]
    subprocess.run(['rustup','which','cargo'],check=True,stdout=subprocess.DEVNULL)
    child=subprocess.Popen(['cargo','build','--locked','--quiet','-p','xtask'],stdin=marker)
    grace=None
    failure=None
    kill_end=None
    while child.poll() is None:
        if failure is None:
            try: validate_locks()
            except Exception as e:
                failure=str(e);child.terminate();grace=time.monotonic()+2
        if interrupted and grace is None: grace=time.monotonic()+2
        if grace is not None and time.monotonic()>=grace:
            if kill_end is None: child.kill();kill_end=time.monotonic()+3
            elif time.monotonic()>=kill_end: raise RuntimeError('direct bootstrap child unreaped; target kept; stamp absent')
        time.sleep(.01)
    status=child.wait();child=None
    if failure: raise RuntimeError(failure)
    validate_locks()
    if interrupted: sys.exit(128+interrupted)
    if status: sys.exit(status if status>0 else 128-status)
    if identity()!=before: raise RuntimeError('judge-source-id changed during bootstrap')
    if interrupted: sys.exit(128+interrupted)
    tmp=boot/'judge-id.tmp'
    fd=os.open(tmp,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
    with os.fdopen(fd,'wb') as f: f.write(before+b'\n');f.flush();os.fsync(f.fileno())
    if interrupted:
        tmp.unlink();sys.exit(128+interrupted)
    validate_locks()
    os.rename(tmp,stamp)
    if interrupted:
        stamp.unlink();sys.exit(128+interrupted)
    os.environ['XTASK_GATE_JUDGE_ID']=before.decode('ascii')
    for fd,path in reversed(locks): os.close(fd)
    locks=[]
    os.execv(str(boot/'debug/xtask'),[str(boot/'debug/xtask'),'gate',*sys.argv[1:]])
except Exception as e:
    print('REFUSED: bootstrap: '+str(e),file=sys.stderr)
    sys.exit(1)
PYTHON
