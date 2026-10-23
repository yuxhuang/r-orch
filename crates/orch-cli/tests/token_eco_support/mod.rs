//! Shared harness for the token-eco external CLI tests (T1).
//!
//! Everything here is hermetic: a throwaway project, a throwaway `HOME`, a
//! private `ORCHESTRY_GLOBAL_CONFIG_PATH`, and a `PATH` that holds only
//! recording wrappers around the repository's fake agent CLIs plus `/usr/bin`
//! and `/bin`. No real AI CLI can be reached.
#![allow(dead_code, reason = "each test binary uses a subset")]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One `orch` binary under test.
#[derive(Clone, Debug)]
pub struct Reader {
    pub name: &'static str,
    pub bin: PathBuf,
}

pub fn new_reader() -> Reader {
    Reader {
        name: "new",
        bin: PathBuf::from(env!("CARGO_BIN_EXE_orch")),
    }
}

/// The baseline (`d8c2319`) binary, when the harness supplies one.
///
/// `ORCH_T1_REQUIRE_OLD=1` turns "absent" into a failure so the matrix
/// harness cannot silently skip the old-reader half.
pub fn old_reader() -> Option<Reader> {
    match std::env::var_os("ORCH_T1_OLD_BIN") {
        Some(bin) => {
            let bin = PathBuf::from(bin);
            assert!(bin.is_file(), "ORCH_T1_OLD_BIN {bin:?} is not a file");
            Some(Reader { name: "old", bin })
        }
        None => {
            assert!(
                std::env::var_os("ORCH_T1_REQUIRE_OLD").is_none(),
                "ORCH_T1_REQUIRE_OLD is set but ORCH_T1_OLD_BIN is not"
            );
            eprintln!("SKIPPED: ORCH_T1_OLD_BIN not set; old-reader cells did not run");
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
    pub fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// One disposable project plus its private home, fake-agent `PATH` and
/// recording directory.
pub struct Sandbox {
    _parent: tempfile::TempDir,
    /// Always `<canonical tmp>/proj`, so the project name in a prompt is stable.
    pub project: PathBuf,
    pub home: tempfile::TempDir,
    pub bin: tempfile::TempDir,
    pub rec: tempfile::TempDir,
}

/// CLI name -> (fixture script, capture stdin?). Pi keeps stdin open for its
/// RPC session, so capturing to EOF would deadlock.
const FAKES: [(&str, &str, bool); 6] = [
    ("claude", "claude", true),
    ("codex", "codex", true),
    ("opencode", "opencode", true),
    ("grok", "grok", true),
    ("agy", "agy", true),
    ("cursor-agent", "cursor-agent", true),
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-agents")
}

fn write_script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

impl Sandbox {
    /// A fresh initialised project. `init` always runs with `init_reader`.
    pub fn new(init_reader: &Reader) -> Sandbox {
        let parent = tempfile::tempdir().unwrap();
        let project = parent.path().canonicalize().unwrap().join("proj");
        fs::create_dir(&project).unwrap();
        let sb = Sandbox {
            _parent: parent,
            project,
            home: tempfile::tempdir().unwrap(),
            bin: tempfile::tempdir().unwrap(),
            rec: tempfile::tempdir().unwrap(),
        };
        sb.install_fakes();
        let out = sb.run(init_reader, &["init"], None);
        assert!(out.ok(), "init failed: {}", out.all());
        sb
    }

    fn install_fakes(&self) {
        let fx = fixtures();
        for (cli, script, capture) in FAKES {
            let real = fx.join(script);
            let body = format!(
                "#!/bin/sh\n\
                 rec=\"${{ORCH_T1_REC:?}}\"\n\
                 real='{real}'\n\
                 for a in \"$@\"; do case \"$a\" in --version) exec \"$real\" \"$@\";; esac; done\n\
                 if [ '{cli}' = opencode ] && [ \"$1\" = run ] && [ \"$2\" = --help ]; then\n\
                   printf '%s\\n' '  --model, -m string   Model in the format provider/model#variant'; exit 0; fi\n\
                 n=$(ls \"$rec\" | grep -c '^{cli}\\.[0-9]*\\.argv$')\n\
                 printf '%s\\0' \"$@\" > \"$rec/{cli}.$n.argv\"\n\
                 {capture}\n\
                 exec \"$real\" \"$@\" < \"$rec/{cli}.$n.stdin\"\n",
                real = real.display(),
                cli = cli,
                capture = if capture {
                    format!("cat > \"$rec/{cli}.$n.stdin\"")
                } else {
                    format!(": > \"$rec/{cli}.$n.stdin\"")
                },
            );
            write_script(&self.bin.path().join(cli), &body);
        }
        // pi: record argv only, then become the fake (it owns its stdin).
        let real = fx.join("pi");
        write_script(
            &self.bin.path().join("pi"),
            &format!(
                "#!/bin/sh\nrec=\"${{ORCH_T1_REC:?}}\"\n\
                 for a in \"$@\"; do case \"$a\" in --version) exec '{r}' \"$@\";; esac; done\n\
                 n=$(ls \"$rec\" | grep -c '^pi\\.[0-9]*\\.argv$')\n\
                 printf '%s\\0' \"$@\" > \"$rec/pi.$n.argv\"\nexec '{r}' \"$@\"\n",
                r = real.display()
            ),
        );
    }

    pub fn root(&self) -> PathBuf {
        self.project.join(".orchestry")
    }

    pub fn agents_dir(&self) -> PathBuf {
        self.root().join("agents")
    }

    pub fn command(&self, reader: &Reader) -> Command {
        let mut c = Command::new(&reader.bin);
        c.current_dir(&self.project)
            .env_clear()
            .env("HOME", self.home.path())
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                self.home.path().join("global.yml"),
            )
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.bin.path().display()),
            )
            .env("ORCH_T1_REC", self.rec.path())
            .env("NO_COLOR", "1")
            .env("EDITOR", "false")
            .env("ORCH_FAKE_VERSION", "2.1.2");
        c
    }

    pub fn run(&self, reader: &Reader, args: &[&str], stdin: Option<&str>) -> Out {
        self.run_env(reader, args, stdin, &[])
    }

    /// Like [`Sandbox::run`] with extra environment variables (for example a
    /// different `ORCH_FAKE_VERSION` for the fake CLIs).
    pub fn run_env(
        &self,
        reader: &Reader,
        args: &[&str],
        stdin: Option<&str>,
        env: &[(&str, &str)],
    ) -> Out {
        let mut cmd = self.command(reader);
        cmd.envs(env.iter().copied());
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = cmd.spawn().expect("spawn orch");
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        let out = child.wait_with_output().expect("wait orch");
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Every file under `agents/`, sorted, as `(name, bytes)`.
    pub fn agent_bytes(&self) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<_> = fs::read_dir(self.agents_dir())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.is_file())
            .map(|p| {
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    fs::read(&p).unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    }

    /// Create an agent with `reader`, then inject `effort` structurally into
    /// both the definition file and the index cache. This is how a legacy or
    /// hand-edited store looks to a reader; it never goes through a validator.
    pub fn seed_agent(
        &self,
        reader: &Reader,
        name: &str,
        adapter: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> String {
        let mut args = vec![
            "agent",
            "add",
            name,
            "--adapter",
            adapter,
            "--workspace-mode",
            "shared",
            "--quiet",
        ];
        if adapter == "shell" {
            args.extend(["--command", "printf shell-ok"]);
        }
        if let Some(m) = model {
            args.extend(["--model", m]);
        }
        let out = self.run(reader, &args, None);
        assert!(out.ok(), "agent add failed: {}", out.all());
        let id = out.stdout.trim().to_owned();
        if let Some(effort) = effort {
            self.inject_effort(&id, effort);
        }
        id
    }

    pub fn inject_effort(&self, id: &str, effort: &str) {
        let path = self.agents_dir().join(format!("{id}.yml"));
        let mut doc: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        doc["config"]["effort"] = serde_yaml_ng::Value::String(effort.to_owned());
        fs::write(&path, serde_yaml_ng::to_string(&doc).unwrap()).unwrap();

        let index = self.agents_dir().join("_index.json");
        let mut entries: serde_json::Value =
            serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
        for entry in entries.as_array_mut().unwrap() {
            if entry["id"] == id {
                entry["config"]["effort"] = serde_json::Value::String(effort.to_owned());
            }
        }
        fs::write(&index, serde_json::to_vec_pretty(&entries).unwrap()).unwrap();
    }

    /// Argv of every recorded fake-CLI invocation named `cli`, in order,
    /// excluding `--version`/`--help` probes (which are never recorded).
    pub fn recorded(&self, cli: &str) -> Vec<Vec<String>> {
        let mut found: Vec<(usize, Vec<String>)> = fs::read_dir(self.rec.path())
            .unwrap()
            .filter_map(|e| {
                let name = e.unwrap().file_name().to_string_lossy().into_owned();
                let rest = name.strip_prefix(&format!("{cli}."))?;
                let n: usize = rest.strip_suffix(".argv")?.parse().ok()?;
                let raw = fs::read(self.rec.path().join(&name)).unwrap();
                let args = raw
                    .split(|b| *b == 0)
                    .filter(|part| !part.is_empty())
                    .map(|part| String::from_utf8_lossy(part).into_owned())
                    .collect();
                Some((n, args))
            })
            .collect();
        found.sort();
        found.into_iter().map(|(_, args)| args).collect()
    }

    pub fn recorded_stdin(&self, cli: &str, n: usize) -> Option<String> {
        fs::read_to_string(self.rec.path().join(format!("{cli}.{n}.stdin"))).ok()
    }
}

/// The CLI executable name an adapter spawns.
pub fn cli_of(adapter: &str) -> &'static str {
    match adapter {
        "claude" => "claude",
        "codex" => "codex",
        "opencode" => "opencode",
        "antigravity" => "agy",
        "pi" => "pi",
        "grok" => "grok",
        "cursor" => "cursor-agent",
        _ => "",
    }
}
