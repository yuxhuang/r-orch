//! Production-holder RC18 controls. Unsupported is recorded, never reclassified
//! as delivered recovery. The commands run through NativeTarget and Runner.
use super::*;
use std::os::fd::AsFd;
use std::process::Command;

#[test]
fn production_holder_fixture() {
    let Some(root) = std::env::var_os("RC18_ROOT").map(PathBuf::from) else {
        return;
    };
    let mode = std::env::var("RC18_MODE").unwrap();
    let lease = super::super::lease::Lease::Owned(
        super::super::lease::Owned::acquire(
            &root.join("lease"),
            nix::unistd::geteuid().as_raw(),
            super::super::lease::Mode::Exclusive,
            std::time::Duration::ZERO,
        )
        .unwrap(),
    );
    let target = native_admission(&root.join("repo"), false, &lease).unwrap();
    let started = target.begin(&lease);
    if mode == "admit" {
        let text = match started {
            Ok(use_) => format!("ADMITTED {}", use_.decision()),
            Err(e) => format!("REFUSED {e:#}"),
        };
        fs::write(root.join("admission"), text).unwrap();
        return;
    }
    let use_ = started.unwrap();
    fs::write(use_.target().join("old-artifact"), b"must stay while live").unwrap();
    if mode == "unlock" || mode == "record-lock" {
        // Deliberate production-holder mutations: same NativeTarget marker,
        // different release semantics. Never compiled into the CLI.
        use_.marker.file.unlock().unwrap();
        if mode == "record-lock" {
            let lock = nix::libc::flock {
                l_type: nix::libc::F_WRLCK as _,
                l_whence: nix::libc::SEEK_SET as _,
                l_start: 0,
                l_len: 0,
                l_pid: 0,
            };
            nix::fcntl::fcntl(&use_.marker.file, nix::fcntl::FcntlArg::F_SETLK(&lock)).unwrap();
        }
    }
    let argv: Vec<String> =
        serde_json::from_slice(&fs::read(root.join("argv.json")).unwrap()).unwrap();
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).current_dir(root.join("repo"));
    // The observer supplies an already-full output pipe as this helper's fd0.
    // Runner still supplies the actual locked marker as the direct child's fd0.
    let output = File::from(std::io::stdin().as_fd().try_clone_to_owned().unwrap());
    command.stdout(output.try_clone().unwrap()).stderr(output);
    let outcome = use_
        .run(&lease, true, |runner, _| {
            runner.qualification_run(
                &mut command,
                &root.join("ready"),
                mode == "no-inherit",
                mode == "unlock" || mode == "prior-tool-close",
            )
        })
        .unwrap();
    assert!(outcome.result.is_ok());
}

fn qualification(parent: &Path) {
    let root = parent.join(format!(
        "rc18-production-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let output = Command::new("/usr/bin/python3")
        .args(["-c", PROBE])
        .env(
            "RC18_WORKTREE",
            Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap(),
        )
        .arg(&root)
        .arg(std::env::current_exe().unwrap())
        .output()
        .unwrap();
    eprintln!("production qualification retained: {}", root.display());
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Retain raw command/lifetime/admission records, including explicit gaps.
}

#[test]
fn production_holder_commands_and_discriminators_on_worktree_filesystem() {
    qualification(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("target"),
    );
}

#[test]
fn production_holder_commands_and_discriminators_on_scratch_filesystem() {
    qualification(&std::env::temp_dir());
}

const PROBE: &str = r###"
import os,sys,pathlib,subprocess,fcntl,json,time,select,shutil,hashlib
root=pathlib.Path(sys.argv[1]);exe=sys.argv[2]
fixture='gate::target::qualification::production_holder_fixture'
# Resource-only views; no host settings, credentials, Git configuration or home
# are passed into any qualification command, even outside the task recorder.
outer=dict(os.environ)
for name in ['home','cargo','rustup','bin','shims','tmp']:(root/name).mkdir(mode=0o700)
(root/'home/global.yml').write_text('{}\n')
resources=pathlib.Path(outer.get('RUSTUP_HOME',str(pathlib.Path.home()/'.rustup')))/'toolchains'
assert resources.is_dir(),'installed pinned toolchains required'
(root/'rustup/toolchains').symlink_to(resources.resolve())
registry=pathlib.Path(outer.get('CARGO_HOME',str(pathlib.Path.home()/'.cargo')))/'registry'
if registry.is_dir():(root/'cargo/registry').symlink_to(registry.resolve())
rustup=shutil.which('rustup');assert rustup,'installed rustup required'
for name in ['rustup','cargo','rustc','rustdoc','rustfmt','cargo-fmt','cargo-clippy','clippy-driver']:
 (root/'bin'/name).symlink_to(pathlib.Path(rustup).resolve())
deny=shutil.which('cargo-deny')
if deny:(root/'bin/cargo-deny').symlink_to(pathlib.Path(deny).resolve())
for name in ['claude','codex','cursor','cursor-agent','agent','opencode','pi','grok','agy']:
 p=root/'shims'/name;p.write_text('#!/bin/sh\nexit 127\n');p.chmod(0o700)
base_env=dict(HOME=str(root/'home'),CARGO_HOME=str(root/'cargo'),RUSTUP_HOME=str(root/'rustup'),RUSTUP_TOOLCHAIN='1.99.1',RUSTUP_AUTO_INSTALL='0',RUSTUP_NO_UPDATE_CHECK='1',CARGO_NET_OFFLINE='true',CARGO_TERM_COLOR='never',ORCHESTRY_GLOBAL_CONFIG_PATH=str(root/'home/global.yml'),GIT_CONFIG_GLOBAL='/dev/null',GIT_CONFIG_NOSYSTEM='1',TMPDIR=str(root/'tmp'),PATH=os.pathsep.join([str(root/'shims'),str(root/'bin'),'/usr/bin','/bin','/usr/sbin','/sbin']))
assert '.orchestry' not in root.parts
identity={}
for tool in ['cargo','rustc','git','cargo-deny','ps','sysctl','notifyutil']:
 path=shutil.which(tool,path=base_env['PATH'])
 if path:
  p=pathlib.Path(path);identity[tool]={'path':str(p),'resolved':str(p.resolve()),'sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'mode':p.stat().st_mode}
for tool in ['cargo','rustc','rustdoc','rustfmt','cargo-fmt','cargo-clippy','clippy-driver']:
 resolved=subprocess.run(['rustup','which',tool],env=base_env,capture_output=True,text=True,check=True).stdout.strip()
 p=pathlib.Path(resolved)
 identity.setdefault(tool,{})['backend']={'path':str(p),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
(root/'tool-identities.json').write_text(json.dumps(identity,indent=2)+'\n')

def exists(pid):
 p=subprocess.run(['/bin/ps','-p',str(pid),'-o','stat='],capture_output=True,text=True)
 return p.returncode==0 and bool(p.stdout.strip()) and not p.stdout.strip().startswith('Z')
def granted(marker,record=False):
 with marker.open('r+b',buffering=0) as f:
  try:
   (fcntl.lockf if record else fcntl.flock)(f,fcntl.LOCK_EX|fcntl.LOCK_NB)
   return True
  except BlockingIOError:return False

def admission(case,env):
 env={**env,'RC18_MODE':'admit'}
 p=subprocess.run([exe,'--exact',fixture,'--nocapture'],env=env,capture_output=True,timeout=20)
 assert p.returncode==0,p.stderr
 return (case/'admission').read_text()

def probe(label,argv,mode='normal',source=None):
 case=root/label;repo=case/'repo'
 (repo/'src').mkdir(parents=True);(case/'lease').mkdir(mode=0o700)
 (repo/'.gitignore').write_text('target/\n')
 (repo/'Cargo.toml').write_text('[package]\nname="rc18-production"\nversion="0.0.0"\nedition="2024"\n[workspace]\n')
 (repo/'Cargo.lock').write_text('version = 4\n[[package]]\nname = "rc18-production"\nversion = "0.0.0"\n')
 (repo/'src/lib.rs').write_text(source or 'pub fn value( )->u8{1}\n#[test] fn native(){assert_eq!(value(),1);}\n')
 subprocess.run(['/usr/bin/git','init','-q',str(repo)],env=base_env,check=True)
 (repo/'deleted').write_text('deleted')
 subprocess.run(['/usr/bin/git','-C',str(repo),'add','.'],env=base_env,check=True);(repo/'deleted').unlink()
 argv=[a.replace('@manifest',str(repo/'Cargo.toml')).replace('@repo',str(repo)).replace('@case',str(case)) for a in argv]
 (case/'argv.json').write_text(json.dumps(argv))
 env={**base_env,'RC18_ROOT':str(case),'RC18_MODE':mode,'CARGO_TARGET_DIR':str(repo/'target/gate-warm')}
 if mode=='prior-tool-close':
  (case/'bin').mkdir()
  previous=case/'bin/cargo'
  previous.write_text('#!/usr/bin/python3\nimport os,signal,time,pathlib\nsignal.signal(signal.SIGTERM,signal.SIG_IGN)\nos.close(0)\nroot=pathlib.Path('+repr(str(case))+')\n(root/"resistant.ready").touch()\nprint("ready",flush=True)\nwhile not (root/"release").exists():time.sleep(.005)\n')
  previous.chmod(0o700)
  env['PATH']=str(case/'bin')+os.pathsep+env['PATH']

 reader,writer=os.pipe();fcntl.fcntl(writer,fcntl.F_SETFL,os.O_NONBLOCK);filled=0
 try:
  while True:filled+=os.write(writer,b'x'*4096)
 except BlockingIOError:pass
 try:
  while True:filled+=os.write(writer,b'x')
 except BlockingIOError:pass
 fcntl.fcntl(writer,fcntl.F_SETFL,0);fcntl.fcntl(reader,fcntl.F_SETFL,os.O_NONBLOCK)
 log=(case/'holder.stderr').open('wb')
 holder=subprocess.Popen([exe,'--exact',fixture,'--nocapture'],env=env,stdin=writer,stdout=subprocess.DEVNULL,stderr=log);os.close(writer)
 out=bytearray();samples=[];row={'label':label,'argv':argv,'mode':mode,'device':repo.stat().st_dev};pid=None
 try:
  end=time.monotonic()+30
  while not (case/'ready').exists():
   assert holder.poll() is None,(label,(case/'holder.stderr').read_text())
   assert time.monotonic()<end,(label,'holder not ready')
   time.sleep(.002)
  pid=int((case/'ready').read_text());marker=repo/'target/.gate-state/warm.use';inode=marker.stat().st_ino
  row.update(pid=pid,marker_inode=inode)
  # Observation starts immediately after production spawn, before releasing any
  # command output; it does not claim visibility into spawn/exit kernel internals.
  assert exists(pid),(label,'command already exited')
  row['before_kill_granted']=granted(marker,mode=='record-lock')
  if sys.platform=='darwin':
   ls=subprocess.run(['/usr/sbin/lsof','-a','-p',str(pid),'-d','0','-Ffin'],capture_output=True,text=True,timeout=10)
   row['stdin_observation']={'exit':ls.returncode,'stdout':ls.stdout,'stderr':ls.stderr}
  holder.kill();row['holder_wait']=holder.wait(timeout=5)
  assert row['holder_wait']==-9
  row['live_after_kill']=exists(pid);row['grant_live']=granted(marker,mode=='record-lock')
  if mode=='prior-tool-close':
   row['prior_tool_sha256']=hashlib.sha256(previous.read_bytes()).hexdigest()
   previous.unlink();previous.symlink_to(shutil.which('cargo',path=base_env['PATH']))
   row['current_cargo']=str(previous.resolve())
   row['current_cargo_sha256']=hashlib.sha256(previous.read_bytes()).hexdigest()
   row['marker_bytes_after_replacement']=marker.read_text()

  row['admission_live']=admission(case,env)
  row['live_after_admission']=exists(pid)
  assert row['live_after_admission'],row
  assert row['live_after_kill']
  assert (repo/'target/gate-warm/old-artifact').read_bytes()==b'must stay while live'
  if mode=='normal':
   assert not row['grant_live'],row
   assert 'marker lock not granted' in row['admission_live'],row
  else:
   assert row['grant_live'],row
   # Record replacement is probed with lockf too; flock and record locks are
   # independent, so a flock grant alone would not discriminate this mutation.
   assert 'recovery unsupported' in row['admission_live'],row
 finally:
  if holder.poll() is None:holder.kill();holder.wait(timeout=5)
  # Only release this test's output backpressure. Never signal an orphan PID.
  end=time.monotonic()+120
  if mode in ['unlock','prior-tool-close']:(case/'release').write_text('release')
  if pid:
   while True:
    live=exists(pid);grant=granted(marker,mode=='record-lock');live_after=exists(pid)
    samples.append({'live_before':live,'live_after':live_after,'granted':grant,'ns':time.monotonic_ns()})
    try:
     data=os.read(reader,65536);discard=min(filled,len(data));filled-=discard
     out.extend(data[discard:])
    except BlockingIOError:pass
    if not live and grant:break
    assert time.monotonic()<end,(label,'retained: direct exit/last-close not observed')
    select.select([reader],[],[],.001)
  os.close(reader);log.close()
 row['samples']=samples;row['actual_exit_observed']=not exists(pid);row['last_close_granted']=granted(marker,mode=='record-lock')
 row['same_inode']=marker.stat().st_ino==inode
 row['admission_after_exit']=admission(case,env)
 row['old_artifact_retained']=(repo/'target/gate-warm/old-artifact').read_bytes()==b'must stay while live'
 (case/'command-output').write_bytes(out)
 (case/'result.json').write_text(json.dumps(row,indent=2)+'\n')
 print(json.dumps({k:v for k,v in row.items() if k not in ['samples','stdin_observation']}),flush=True)
 assert row['actual_exit_observed'] and row['last_close_granted'] and row['same_inode'] and row['old_artifact_retained']
 assert 'recovery unsupported' in row['admission_after_exit']
 return row
cases=[
 ('git-version',['git','--version']),
 ('git-inventory',['git','ls-files','--cached','--others','--exclude-standard','-z']),
 ('git-deleted',['git','ls-files','--deleted','-z']),
 ('git-hash',['git','hash-object','--no-filters','--','src/lib.rs']),
 ('rustc-identity',['rustc','-vV']),('rustc-version',['rustc','-Vv']),('cargo-version',['cargo','-V']),
 ('deny-version',['cargo-deny','--version']),('ps',['ps','-axo','pid=,ppid=,comm=']),
 ('build',['cargo','--color','never','build','--locked','--manifest-path','@manifest','--workspace','--all-targets']),
 ('test-map',['cargo','--color','never','test','--quiet','--locked','--manifest-path','@manifest','--workspace','--no-run','--message-format=json']),
 ('test',['cargo','--color','never','test','--locked','--manifest-path','@manifest','--workspace','--no-fail-fast']),
 ('clippy',['cargo','--color','never','clippy','--locked','--manifest-path','@manifest','--workspace','--all-targets','--','-D','warnings']),
 ('fmt',['cargo','--color','never','fmt','--manifest-path','@manifest','--all','--check']),
 ('deny-check',['cargo-deny','--manifest-path','@manifest','check','advisories','licenses','bans','sources']),
 ('crate-graph',['cargo','--color','never','run','--locked','--quiet','--manifest-path',str(pathlib.Path(os.environ['RC18_WORKTREE'])/'Cargo.toml'),'-p','xtask','--','crate-graph'])]
if sys.platform=='darwin':cases += [('load',['sysctl','-n','vm.loadavg']),('boot',['/usr/sbin/sysctl','-n','kern.bootsessionuuid']),('thermal',['/usr/bin/notifyutil','-g','com.apple.system.thermalpressurelevel'])]
missing=[(label,argv) for label,argv in cases if not shutil.which(argv[0],path=base_env['PATH'])]
print(json.dumps({'missing_unqualified':missing}),flush=True)
results=[]
for label,argv in cases:
 if shutil.which(argv[0],path=base_env['PATH']):results.append(probe(label,argv))
# A resistant direct child, controlled by explicit release (no self-expiry), is
# used for all three production-holder mutations. TERM outlives the 2s grace.
resistant='import signal,time,pathlib;signal.signal(signal.SIGTERM,signal.SIG_IGN);pathlib.Path("@case/resistant.ready").touch();print("ready",flush=True);p=pathlib.Path("@case/release");\nwhile not p.exists():time.sleep(.005)'
# These discriminators use a real rustc blocked in write for dropped inheritance
# and record-lock semantics. Explicit unlock's resistant process is below.
for mode in ['no-inherit','record-lock']:
 results.append(probe(mode,['rustc','-vV'],mode))
results.append(probe('unlock',['/usr/bin/python3','-c',resistant],'unlock'))
results.append(probe('prior-tool-close',['cargo','--color','never','build','--locked','--manifest-path','@manifest','--workspace','--all-targets'],'prior-tool-close'))
(root/'summary.json').write_text(json.dumps(results,indent=2)+'\n')
print('PRODUCTION HOLDER RC18: native last-close reset remains Unsupported; no capability enabled',flush=True)
"###;
