//! Portable extra-descriptor inheritance snapshots, separate from production
//! reset/rebuild controls. Finite native observations do not cover all writers.
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn qualification(parent: &Path) {
    let root = parent.join(format!(
        "gate-recovery-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    eprintln!("qualification fixture: {}", root.display());
    for name in ["home", "tmp", "cargo", "rustup", "bin", "shims"] {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join(name))
            .unwrap();
    }
    fs::write(root.join("home/global.yml"), "{}\n").unwrap();
    let output = Command::new("rustup")
        .args(["which", "rustc"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "installed pinned compiler is required"
    );
    let compiler = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let tools = compiler.parent().unwrap();
    for name in [
        "cargo",
        "rustc",
        "rustdoc",
        "cargo-clippy",
        "clippy-driver",
        "cargo-fmt",
        "rustfmt",
    ] {
        symlink(tools.join(name), root.join("bin").join(name)).unwrap();
    }
    // Only installed resource trees are shared. No host Cargo settings, global
    // configuration, credential file or agent CLI is inherited by the fixture.
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        let registry = PathBuf::from(home).join("registry");
        if registry.is_dir() {
            symlink(registry, root.join("cargo/registry")).unwrap();
        }
    }
    let path = std::env::var_os("PATH").unwrap();
    if let Some(deny) = std::env::split_paths(&path)
        .map(|p| p.join("cargo-deny"))
        .find(|p| p.is_file())
    {
        symlink(deny, root.join("bin/cargo-deny")).unwrap();
    }
    for name in [
        "claude",
        "codex",
        "cursor",
        "cursor-agent",
        "agent",
        "opencode",
        "pi",
        "grok",
        "agy",
    ] {
        let file = root.join("shims").join(name);
        fs::write(&file, "#!/bin/sh\nexit 127\n").unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let output = Command::new("/usr/bin/python3")
        .args(["-c", PROBE])
        .arg(root.join("probe"))
        .env_clear()
        .env("HOME", root.join("home"))
        .env("CARGO_HOME", root.join("cargo"))
        .env("RUSTUP_HOME", root.join("rustup"))
        .env("RUSTUP_TOOLCHAIN", "1.99.1")
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_TERM_COLOR", "never")
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", root.join("home/global.yml"))
        .env("TMPDIR", root.join("tmp"))
        .env(
            "PATH",
            std::env::join_paths([
                root.join("shims"),
                root.join("bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
                PathBuf::from("/usr/sbin"),
                PathBuf::from("/sbin"),
            ])
            .unwrap(),
        )
        .current_dir(repo)
        .output()
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "fixture retained at {}: {}",
        root.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    // The Python fixture observed actual direct-process absence AND a granted
    // lock for every case before returning; pipe EOF is not cleanup authority.
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn actual_commands_retain_marker_on_worktree_filesystem_and_discriminators_detect_loss() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    qualification(&repo.join("target"));
}

#[test]
fn actual_commands_retain_marker_on_scratch_filesystem_and_discriminators_detect_loss() {
    qualification(&std::env::temp_dir());
}

const PROBE: &str = r###"
import os,sys,pathlib,subprocess,fcntl,json,time,select,shutil
root=pathlib.Path(sys.argv[1]);root.mkdir(mode=0o700)
fixture=root/'fixture';(fixture/'src').mkdir(parents=True)
(fixture/'Cargo.toml').write_text('[package]\nname="rc18-native"\nversion="0.0.0"\nedition="2024"\n[workspace]\n')
(fixture/'Cargo.lock').write_text('version = 4\n[[package]]\nname = "rc18-native"\nversion = "0.0.0"\n')
(fixture/'src/lib.rs').write_text('pub fn value( )->u8{1}\n#[test] fn native(){assert_eq!(value(),1);}\n')
subprocess.run(['git','init','-q',str(fixture)],check=True)
(fixture/'deleted').write_text('deleted');subprocess.run(['git','-C',str(fixture),'add','Cargo.toml','Cargo.lock','src/lib.rs','deleted'],check=True);(fixture/'deleted').unlink()
env={**os.environ,'CARGO_TARGET_DIR':str(root/'target')}
manifest=str(fixture/'Cargo.toml')
cases=[
('git-version',['git','--version']),
('git-inventory',['git','ls-files','--cached','--others','--exclude-standard','-z']),
('git-deleted',['git','ls-files','--deleted','-z']),
('git-hash',['git','hash-object','--no-filters','--','src/lib.rs']),
('rustc-identity',['rustc','-vV']),('rustc-version',['rustc','-Vv']),('cargo-version',['cargo','-V']),
('deny-version',['cargo-deny','--version']),
('ps',['ps','-axo','pid=,ppid=,comm=']),
('build',['cargo','--color','never','build','--locked','--manifest-path',manifest,'--workspace','--all-targets']),
('test-map',['cargo','--color','never','test','--quiet','--locked','--manifest-path',manifest,'--workspace','--no-run','--message-format=json']),
('test',['cargo','--color','never','test','--locked','--manifest-path',manifest,'--workspace','--no-fail-fast']),
('clippy',['cargo','--color','never','clippy','--locked','--manifest-path',manifest,'--workspace','--all-targets','--','-D','warnings']),
('fmt',['cargo','--color','never','fmt','--manifest-path',manifest,'--all','--check']),
('deny-check',['cargo-deny','--manifest-path',manifest,'check','advisories','licenses','bans','sources']),
('crate-graph',['cargo','--color','never','run','--locked','--quiet','--manifest-path',str(pathlib.Path.cwd()/'Cargo.toml'),'-p','xtask','--','crate-graph']),
]
if sys.platform=='darwin': cases += [('load',['sysctl','-n','vm.loadavg']),('boot',['/usr/sbin/sysctl','-n','kern.bootsessionuuid']),('thermal',['/usr/bin/notifyutil','-g','com.apple.system.thermalpressurelevel'])]
missing=[(label,argv) for label,argv in cases if not shutil.which(argv[0])]
cases=[(label,argv) for label,argv in cases if shutil.which(argv[0])]
print(json.dumps({'unmeasured_missing_tools':missing}),flush=True)
holder_code='''import os,sys,json,fcntl,subprocess,pathlib,time
marker,pidfile,writer,argv,mutant=sys.argv[1:];fd=os.open(marker,os.O_RDWR|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600);fcntl.flock(fd,fcntl.LOCK_EX)
os.set_inheritable(fd,True)
child=subprocess.Popen(json.loads(argv),stdin=subprocess.DEVNULL,pass_fds=() if mutant=='no-inherit' else (fd,),stdout=int(writer),stderr=int(writer))
if mutant=='unlock': fcntl.flock(fd,fcntl.LOCK_UN)
pathlib.Path(pidfile).write_text(str(child.pid));child.wait()
'''
def exists(pid):
 p=subprocess.run(['/bin/ps','-p',str(pid),'-o','stat='],capture_output=True,text=True)
 return p.returncode==0 and p.stdout.strip() and not p.stdout.strip().startswith('Z')
def grant(marker):
 fd=os.open(marker,os.O_RDONLY|os.O_NOFOLLOW)
 try:
  try: fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);return True
  except BlockingIOError:return False
 finally:os.close(fd)
def probe(label,argv,mutant='none'):
 marker=root/(label+'.use');pidfile=root/(label+'.pid')
 reader,writer=os.pipe();fcntl.fcntl(writer,fcntl.F_SETFL,os.O_NONBLOCK)
 filled=0
 try:
  while True: filled+=os.write(writer,b'x'*4096)
 except BlockingIOError: pass
 try:
  while True:filled+=os.write(writer,b'x')
 except BlockingIOError:pass
 fcntl.fcntl(writer,fcntl.F_SETFL,0);fcntl.fcntl(reader,fcntl.F_SETFL,os.O_NONBLOCK)
 holder=subprocess.Popen([sys.executable,'-c',holder_code,str(marker),str(pidfile),str(writer),json.dumps(argv),mutant],cwd=fixture,env=env,pass_fds=(writer,),stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
 os.close(writer)
 observations={};caught=None;output=bytearray()
 try:
  end=time.monotonic()+20
  while not pidfile.exists():
   assert holder.poll() is None, holder.stderr.read()
   assert time.monotonic()<end,'holder readiness timeout'
   time.sleep(.005)
  pid=int(pidfile.read_text());observations.update(pid=pid,marker_inode=marker.stat().st_ino,argv=argv)
  if sys.platform=='darwin':
   ls=subprocess.run(['/usr/sbin/lsof','-a','-p',str(pid),'-Ffin'],capture_output=True,text=True,timeout=10)
   observations['descriptor_observation']=ls.stdout
   marker_seen=('i'+str(marker.stat().st_ino)+'\n') in ls.stdout
   observations['extra_marker_seen']=marker_seen
   stdin=subprocess.run(['/usr/sbin/lsof','-a','-p',str(pid),'-d','0','-Ffin'],capture_output=True,text=True,timeout=10)
   observations['stdin']=stdin.stdout
   if argv[0]!='ps':
    assert 'i'+str(marker.stat().st_ino)+'\n' not in stdin.stdout,stdin
    if mutant!='no-inherit':assert marker_seen,ls
    else:assert not marker_seen,ls
  else:
   try:
    observations['stdin_inode']=os.stat('/proc/%s/fd/0'%pid).st_ino
    st=marker.stat(); found=[]
    for f in pathlib.Path('/proc/%s/fd'%pid).iterdir():
     try:m=f.stat()
     except FileNotFoundError:continue
     if (m.st_dev,m.st_ino)==(st.st_dev,st.st_ino):found.append(int(f.name))
    observations['marker_fds']=found
    assert observations['stdin_inode']!=st.st_ino
    assert bool(found)==(mutant!='no-inherit')
   except FileNotFoundError:raise AssertionError('native process disappeared before fd observation')
  holder.kill();assert holder.wait(timeout=5)==-9
  observations['direct_live_after_holder_kill']=bool(exists(pid))
  observations['lock_granted_while_live']=grant(marker)
  observations['same_marker']=marker.stat().st_ino==observations['marker_inode']
 finally:
  if holder.poll() is None:holder.kill();holder.wait(timeout=5)
  # Release only the fixture output backpressure. No signal to a diagnostic PID.
  end=time.monotonic()+60
  while True:
   try:
    data=os.read(reader,65536)
    discard=min(filled,len(data));filled-=discard;data=data[discard:]
    if len(output)<1024*1024:output.extend(data[:1024*1024-len(output)])
   except BlockingIOError:data=None
   if 'pid' in observations and not exists(observations['pid']) and grant(marker):
    # The process/lock observations precede cleanup; EOF only finishes capture.
    while True:
     try:
      rest=os.read(reader,65536)
      if not rest:break
      discard=min(filled,len(rest));filled-=discard;rest=rest[discard:]
      if len(output)<1024*1024:output.extend(rest[:1024*1024-len(output)])
     except BlockingIOError:break
    break
   assert time.monotonic()<end, 'retained fixture: direct process has not exited'
   select.select([reader],[],[],.01)
  os.close(reader)
 observations.update(last_close_granted=grant(marker),direct_absent=not bool(exists(observations['pid'])),output_tail=bytes(output).decode(errors='replace')[-500:])
 (root/(label+'.json')).write_text(json.dumps(observations,indent=2)+'\n')
 print(json.dumps(observations),flush=True)
 return observations
results=[]
for label,argv in cases:
 row=probe(label,argv);results.append((label,row))
 assert row['direct_live_after_holder_kill'] and not row['lock_granted_while_live'] and row['same_marker'] and row['last_close_granted'] and row['direct_absent'],(label,row)
for mutant in ['no-inherit','unlock']:
 row=probe(mutant,['rustc','-vV'],mutant);results.append((mutant,row))
 assert row['direct_live_after_holder_kill'] and row['lock_granted_while_live'] and row['same_marker'] and row['last_close_granted'] and row['direct_absent'],(mutant,row)
(root/'summary.json').write_text(json.dumps(dict(results),indent=2)+'\n')
print('Finite extra-descriptor snapshots only; production native reset/rebuild is verified separately; Linux is unmeasured until actually run.',flush=True)
"###;
