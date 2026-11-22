#!/usr/bin/env bash
# full (default) is the goal/release gate; pr is the merge condition after the
# policy transition. fast is local iteration, never merge evidence. Incident protections I1-I24 live in Rust:
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
import fcntl, json, os, pathlib, re, signal, socket, stat, subprocess, sys, time
repo = pathlib.Path.cwd()
target = repo / 'target'
boot = target / 'gate-xtask'
stamp = boot / 'judge-id'
uid = os.geteuid()
child = None
interrupted = None
locks = []
inherited = None
class BootstrapBusy(RuntimeError):
    def __init__(self, message, code): super().__init__(message); self.code=code
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
def publish_executable(expected):
    # Cargo legitimately hardlinks debug/xtask to debug/deps on Linux. Only
    # this post-success build output may have aliases; published judge/stamp
    # and every source input still require single-link regular owned files.
    executable = boot/'debug/xtask'
    temporary = boot/'debug/.xtask.publish.tmp'
    check(executable.parent, 'dir')
    def candidate():
        m = executable.lstat()
        if not stat.S_ISREG(m.st_mode) or m.st_uid != uid or not m.st_mode & stat.S_IXUSR:
            raise RuntimeError('bootstrap executable owner/type/mode refused')
        return m
    def signature(m):
        return (m.st_dev,m.st_ino,m.st_mode,m.st_uid,m.st_nlink,m.st_size,m.st_mtime_ns,m.st_ctime_ns)
    before = candidate()
    fd = os.open(executable,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    with os.fdopen(fd,'rb') as source:
        if signature(os.fstat(source.fileno())) != signature(before): raise RuntimeError('bootstrap executable changed')
        output_fd = os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o700)
        with os.fdopen(output_fd,'wb') as output:
            remaining = before.st_size
            while remaining:
                chunk = source.read(min(1024*1024,remaining))
                if not chunk: raise RuntimeError('bootstrap executable truncated')
                output.write(chunk);remaining -= len(chunk)
            if source.read(1) or signature(os.fstat(source.fileno())) != signature(before): raise RuntimeError('bootstrap executable changed')
            output.flush();os.fsync(output.fileno())
    validate_locks()
    if signature(candidate()) != signature(before): raise RuntimeError('bootstrap executable replaced')
    if identity()!=expected: raise RuntimeError('judge-source-id changed during publication')
    if interrupted: raise RuntimeError('INTERRUPTED before executable publication')
    check(temporary,'file')
    os.rename(temporary,executable) # D45 checked-path/by-name interim risk.
    check(executable,'file')
    fd=os.open(executable.parent,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    try: os.fsync(fd)
    finally: os.close(fd)

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
    # Keep this literal workspace closure in sync with gate/judge.rs. Git's
    # ls-tree (commit verification) does not expand ls-files wildcard syntax.
    inputs = ('xtask', 'crates/orch-core', 'crates/orch-domain', 'crates/orch-infra',
              'crates/orch-cli/Cargo.toml', 'crates/orch-client/Cargo.toml',
              'crates/orch-daemon/Cargo.toml', 'crates/orch-engine/Cargo.toml',
              'crates/orch-peer/Cargo.toml', 'crates/orch-proto/Cargo.toml',
              'crates/orch-tui/Cargo.toml', 'Cargo.toml', 'Cargo.lock',
              'rust-toolchain.toml', '.cargo/config.toml')
    paths=git('ls-files','-z','--cached','--others','--exclude-standard','--',*inputs).split(b'\0')
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
    if not {b'xtask/gate.sh',b'Cargo.lock'}.issubset(set(paths)): raise RuntimeError('incomplete judge source set')
    return git('hash-object','--stdin',data=b''.join(records)).strip()
def lock(path, deadline, code=75, shared=False):
    if os.path.lexists(path):
        m=check(path,'file')
        if m.st_mode & 0o7077: raise RuntimeError('lock mode')
    fd=os.open(path,os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW|os.O_NONBLOCK,0o600)
    locks.append((fd,path))
    held=os.fstat(fd); now=check(path,'file')
    if (held.st_dev,held.st_ino)!=(now.st_dev,now.st_ino): raise RuntimeError('lock replaced')
    announced=False
    while True:
        if interrupted: raise RuntimeError('INTERRUPTED waiting for bootstrap lock')
        try: fcntl.flock(fd,(fcntl.LOCK_SH if shared else fcntl.LOCK_EX)|fcntl.LOCK_NB); break
        except BlockingIOError:
            if time.monotonic()>=deadline: raise BootstrapBusy('bootstrap cooperative lease busy' if code==75 else 'bootstrap target in use',code)
            if not announced:
                stage='admission-turnstile' if path.name=='r-orch-gate.turnstile' else 'lease-shared' if shared else 'lease-exclusive'
                try: print('bootstrap: waiting stage='+stage,file=sys.stderr,flush=True)
                except OSError: pass # A closed terminal cannot change lock/cleanup authority.
                announced=True
            time.sleep(.01)
    return fd
def holder_record(cap):
    path=cap['lease']
    before=check(path,'file')
    if before.st_mode & 0o7077: raise RuntimeError('inherited lease mode')
    fd=os.open(path,os.O_RDWR|os.O_NOFOLLOW|os.O_NONBLOCK)
    try:
        held=os.fstat(fd);now=check(path,'file')
        identity=(held.st_dev,held.st_ino)
        if identity!=(before.st_dev,before.st_ino) or identity!=(now.st_dev,now.st_ino): raise RuntimeError('inherited lease identity changed')
        if cap.get('identity') is not None and identity!=cap['identity']: raise RuntimeError('inherited lease path replaced')
        try: fcntl.flock(fd,fcntl.LOCK_SH|fcntl.LOCK_NB)
        except BlockingIOError: pass
        else: raise RuntimeError('inherited holder is not kernel-exclusive')
        data=os.read(fd,4097)
        if len(data)>4096: raise RuntimeError('inherited holder record budget')
        record=json.loads(data)
        if not isinstance(record,dict): raise RuntimeError('inherited holder record is not an object')
        if record.get('socket')!=cap['socket'] or record.get('token')!=cap['token'].hex(): raise RuntimeError('inherited capability mismatch')
        return identity
    finally: os.close(fd) # close only, never unlock another description

def validate_inherited(cap):
    before=holder_record(cap)
    with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as stream:
        stream.settimeout(1)
        stream.connect(cap['socket']);stream.sendall(cap['token'])
        if stream.recv(1)!=b'\x01': raise RuntimeError('inherited holder challenge refused')
    if holder_record(cap)!=before: raise RuntimeError('inherited holder changed during challenge')
    return before

def inherited_capability(directory):
    name=os.environ.get('XTASK_GATE_LEASE_SOCKET');token=os.environ.get('XTASK_GATE_LEASE_TOKEN')
    if name is None or token is None or len(token)!=64 or any(c not in '0123456789abcdefABCDEF' for c in token): return None
    try:
        name.encode('utf-8')
        cap=dict(lease=directory/'r-orch-gate.lease',socket=name,token=bytes.fromhex(token))
        cap['identity']=validate_inherited(cap)
        return cap
    except (OSError,ValueError,RuntimeError,TypeError): return None

def validate_locks():
    if inherited is not None: validate_inherited(inherited)
    for fd,path in locks:
        held=os.fstat(fd);now=check(path,'file')
        if now.st_mode & 0o7077 or (held.st_dev,held.st_ino)!=(now.st_dev,now.st_ino): raise RuntimeError('bootstrap lease/marker lost: '+str(path))
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
    wait=30
    args=sys.argv[1:]
    for index,arg in enumerate(args):
        if arg=='--': break
        raw=args[index+1] if arg=='--lease-wait' and index+1<len(args) else arg.split('=',1)[1] if arg.startswith('--lease-wait=') else None
        if raw is not None:
            if not raw.isascii() or not raw.isdigit() or int(raw)>2**64-1: raise RuntimeError('invalid --lease-wait')
            wait=int(raw)
    if wait>(2**64-1)//60: raise RuntimeError('lease wait overflow')
    deadline=time.monotonic()+wait*60 # one admission deadline, CLI minutes
    inherited=inherited_capability(lease_dir)
    if inherited is None:
        # Admission classification only, not another CLI parser. Skip known
        # global option values so a label/base named "fast" is not a tier, and
        # honor clap's options-before-tier spelling. Unknown shapes stay
        # exclusive; Rust remains responsible for complete usage validation.
        def requests_fast(args):
            values={'--part','--base','--evidence','--host-label','--lease-wait','--no-lease'}
            flags={'--plan','--keep','--warm','--help','-h','--version','-V'}
            index=0
            while index<len(args):
                arg=args[index]
                if arg=='--': return index+1<len(args) and args[index+1]=='fast'
                if arg in values: index+=2;continue
                if any(arg.startswith(option+'=') for option in values) or arg in flags:
                    index+=1;continue
                return arg=='fast'
            return False
        turnstile=lock(lease_dir/'r-orch-gate.turnstile',deadline)
        lock(lease_dir/'r-orch-gate.lease',deadline,shared=requests_fast(args))
        os.close(turnstile);locks.remove((turnstile,lease_dir/'r-orch-gate.turnstile'))
    if interrupted: sys.exit(128+interrupted)
    directory(target)
    marker=lock(target/'.gate-xtask.lock',time.monotonic(),1)
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
    publish_executable(before)
    validate_locks()
    if identity()!=before: raise RuntimeError('judge-source-id changed after executable publication')
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
    print('bootstrap-lease: '+('inherited' if inherited is not None else 'owned'),flush=True)
    for fd,path in reversed(locks): os.close(fd)
    locks=[]
    os.execv(str(boot/'debug/xtask'),[str(boot/'debug/xtask'),'gate',*sys.argv[1:]])
except BootstrapBusy as e:
    detail='unavailable'
    if e.code==75:
        try: detail=ascii(read(lease_dir/'r-orch-gate.holder',4096).decode('utf-8',errors='replace'))
        except Exception: pass
    print('REFUSED: '+str(e)+'; purpose/holder diagnostics (unvalidated, never signal authority): '+detail,file=sys.stderr)
    sys.exit(e.code)
except Exception as e:
    print('REFUSED: bootstrap: '+str(e),file=sys.stderr)
    sys.exit(128+interrupted if interrupted else 1)
PYTHON
