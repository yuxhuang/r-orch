//! Skill library loader — ported from `src/infrastructure/skills/skill-loader.ts`.
//!
//! Library skills are Markdown files under `skills/library/`, injected into the
//! system prompt verbatim (frontmatter included). Names containing `:` are
//! Claude Code MCP skills, handled natively by the Claude CLI and skipped here.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use orch_core::skills::{format_skills_block, is_mcp_skill, is_valid_skill_name};
use orch_core::{Error, Result, skills::SkillLoader};
use tokio::sync::OnceCell;

type WarnSink = Arc<dyn Fn(&str) + Send + Sync>;

/// How many directories the library search probes upward, executable first
/// (the TS walk probes `thisDir` plus four parents).
const PROBE_DEPTH: usize = 5;

/// Find `skills/library` for the Rust binary.
///
/// Probes the executable's directory and its ancestors (installed layout:
/// `<prefix>/bin/orch` with `<prefix>/skills/library`; `target/debug/orch`
/// inside the repo), then the repository this crate was built from. If none
/// exists the result is the unchecked executable-relative path, so the
/// missing-skill warning names a meaningful location. There is deliberately no
/// environment variable (the TS loader has none); embedders override with
/// [`FileSkillLoader::with_library_dir`].
pub fn resolve_library_dir() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let build_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    let mut starts = Vec::new();
    starts.extend(exe_dir.clone());
    starts.push(build_root);
    for start in &starts {
        for dir in start.ancestors().take(PROBE_DEPTH) {
            let candidate = dir.join("skills").join("library");
            if candidate.is_dir() {
                return candidate;
            }
        }
    }
    exe_dir.unwrap_or_default().join("skills").join("library")
}

pub struct FileSkillLoader {
    library_dir: OnceCell<PathBuf>,
    configured_dir: Option<PathBuf>,
    cache: Mutex<HashMap<String, String>>,
    available: Mutex<Option<Vec<String>>>,
    warn: WarnSink,
}

impl FileSkillLoader {
    /// Resolve the library directory with [`resolve_library_dir`], lazily.
    pub fn new() -> Self {
        Self::build(None)
    }

    pub fn with_library_dir(dir: impl Into<PathBuf>) -> Self {
        Self::build(Some(dir.into()))
    }

    #[allow(
        clippy::print_stderr,
        reason = "the default sink for the user-facing missing-skill warning; callers can redirect it"
    )]
    fn build(configured_dir: Option<PathBuf>) -> Self {
        Self {
            library_dir: OnceCell::new(),
            configured_dir,
            cache: Mutex::new(HashMap::new()),
            available: Mutex::new(None),
            warn: Arc::new(|line| eprint!("{line}")),
        }
    }

    /// Redirect the missing-skill warning (stderr by default).
    pub fn with_warning_sink(mut self, sink: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.warn = Arc::new(sink);
        self
    }

    async fn dir(&self) -> &Path {
        self.library_dir
            .get_or_init(|| async {
                self.configured_dir
                    .clone()
                    .unwrap_or_else(resolve_library_dir)
            })
            .await
    }

    async fn load_one(&self, name: &str) -> Option<String> {
        if let Some(cached) = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
        {
            return (!cached.is_empty()).then(|| cached.clone());
        }
        // Path-traversal guard: nothing touches the filesystem before this.
        if !is_valid_skill_name(name) {
            return None;
        }
        let dir = self.dir().await;
        match tokio::fs::read(dir.join(format!("{name}.md"))).await {
            Ok(bytes) => {
                let content = String::from_utf8_lossy(&bytes).into_owned();
                self.cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(name.to_owned(), content.clone());
                Some(content)
            }
            Err(err) => {
                tracing::warn!(skill = name, dir = %dir.display(), error = %err, "skill not readable");
                (self.warn)(&format!(
                    "[orch] skill library: \"{name}\" not found in {}\n",
                    dir.display()
                ));
                self.cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(name.to_owned(), String::new());
                None
            }
        }
    }
}

impl Default for FileSkillLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SkillLoader for FileSkillLoader {
    async fn load_skills(&self, skill_names: &[String]) -> Result<String> {
        let library: Vec<&String> = skill_names.iter().filter(|s| !is_mcp_skill(s)).collect();
        if library.is_empty() {
            return Ok(String::new());
        }
        let loaded = futures::future::join_all(library.iter().map(|n| self.load_one(n))).await;
        let resolved: Vec<(String, String)> = library
            .into_iter()
            .zip(loaded)
            .filter_map(|(name, content)| {
                content.filter(|c| !c.is_empty()).map(|c| (name.clone(), c))
            })
            .collect();
        Ok(format_skills_block(&resolved))
    }

    async fn list_available(&self) -> Result<Vec<String>> {
        if let Some(cached) = self
            .available
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            return Ok(cached);
        }
        let dir = self.dir().await;
        let mut names = match tokio::fs::read_dir(dir).await {
            Ok(mut entries) => {
                let mut names = Vec::new();
                while let Some(entry) = entries
                    .next_entry()
                    .await
                    .map_err(|e| Error::io("reading the skill library", dir, e))?
                {
                    if let Some(stem) = entry.file_name().to_string_lossy().strip_suffix(".md") {
                        names.push(stem.to_owned());
                    }
                }
                names
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return Err(Error::io("reading the skill library", dir, e));
            }
        };
        names.sort();
        *self
            .available
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(names.clone());
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn lib(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            fs::write(dir.path().join(name), content).unwrap();
        }
        dir
    }

    fn loader(dir: &Path) -> (FileSkillLoader, Arc<Mutex<Vec<String>>>) {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&warnings);
        let loader = FileSkillLoader::with_library_dir(dir)
            .with_warning_sink(move |line| sink.lock().unwrap().push(line.to_owned()));
        (loader, warnings)
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[tokio::test]
    async fn loads_library_skills_into_a_markdown_block() {
        let dir = lib(&[
            ("review.md", "# Review\nDo a review."),
            ("investigate.md", "# Investigate\nDebug stuff."),
        ]);
        let (l, _) = loader(dir.path());
        let out = l
            .load_skills(&names(&["review", "investigate"]))
            .await
            .unwrap();
        assert_eq!(
            out,
            "## Skills\n\n### review\n\n# Review\nDo a review.\n\n### investigate\n\n# Investigate\nDebug stuff."
        );
    }

    #[tokio::test]
    async fn skips_mcp_skills() {
        let dir = lib(&[("review.md", "Review content")]);
        let (l, warnings) = loader(dir.path());
        let out = l
            .load_skills(&names(&["feature-dev:code-explorer", "review"]))
            .await
            .unwrap();
        assert!(out.contains("### review"));
        assert!(!out.contains("feature-dev"));
        assert!(warnings.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn empty_results_are_the_empty_string() {
        let dir = lib(&[]);
        let (l, _) = loader(dir.path());
        assert_eq!(
            l.load_skills(&names(&["a:b", "testing-suite:generate-tests"]))
                .await
                .unwrap(),
            ""
        );
        assert_eq!(l.load_skills(&[]).await.unwrap(), "");
    }

    #[tokio::test]
    async fn unknown_skill_warns_once_with_the_exact_line() {
        let dir = lib(&[("review.md", "Review content")]);
        let (l, warnings) = loader(dir.path());
        let out = l
            .load_skills(&names(&["review", "nonexistent"]))
            .await
            .unwrap();
        assert!(out.contains("### review") && !out.contains("nonexistent"));
        l.load_skills(&names(&["nonexistent"])).await.unwrap();
        assert_eq!(
            *warnings.lock().unwrap(),
            [format!(
                "[orch] skill library: \"nonexistent\" not found in {}\n",
                dir.path().display()
            )]
        );
    }

    #[tokio::test]
    async fn rejects_traversal_and_invalid_names_silently() {
        let dir = lib(&[("review.md", "Review content")]);
        // The decoy goes in a sibling *inside* the tempdir, not in
        // `dir/..` — which is the tempdir's own parent, i.e. `/tmp` (code
        // m5). A test that writes outside its tempdir leaves litter on the
        // machine and, worse, would be writing somewhere it has no business
        // writing if `..` ever resolved somewhere sensitive.
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.md"), "x").unwrap();
        let (l, warnings) = loader(dir.path());
        let out = l
            .load_skills(&names(&["../etc/passwd", "../outside/secret", "review"]))
            .await
            .unwrap();
        assert!(out.contains("### review") && !out.contains("passwd") && !out.contains("secret"));
        let out = l
            .load_skills(&names(&["Review", "my_skill", "some.skill", ""]))
            .await
            .unwrap();
        assert_eq!(out, "");
        assert!(warnings.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn caches_content() {
        let dir = lib(&[("review.md", "Review content")]);
        let (l, _) = loader(dir.path());
        l.load_skills(&names(&["review"])).await.unwrap();
        std::fs::write(dir.path().join("review.md"), "CHANGED").unwrap();
        let out = l.load_skills(&names(&["review"])).await.unwrap();
        assert!(out.contains("Review content") && !out.contains("CHANGED"));
    }

    #[tokio::test]
    async fn lists_md_files_sorted_without_extension() {
        let dir = lib(&[
            ("review.md", "x"),
            ("qa.md", "x"),
            ("careful.md", "x"),
            ("not-a-skill.txt", "x"),
        ]);
        let (l, _) = loader(dir.path());
        assert_eq!(
            l.list_available().await.unwrap(),
            ["careful", "qa", "review"]
        );
    }

    #[tokio::test]
    async fn missing_directory_lists_nothing() {
        let (l, _) = loader(Path::new("/nonexistent/path"));
        assert!(l.list_available().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn caches_the_list() {
        let dir = lib(&[("review.md", "x")]);
        let (l, _) = loader(dir.path());
        let first = l.list_available().await.unwrap();
        std::fs::write(dir.path().join("new-skill.md"), "x").unwrap();
        assert_eq!(l.list_available().await.unwrap(), first);
        assert_eq!(first, ["review"]);
    }

    #[tokio::test]
    async fn resolves_the_bundled_library_by_default() {
        let l = FileSkillLoader::new();
        let list = l.list_available().await.unwrap();
        for expected in ["review", "qa", "careful", "investigate", "ship"] {
            assert!(list.iter().any(|s| s == expected), "{expected} missing");
        }
        let out = l.load_skills(&names(&["careful"])).await.unwrap();
        assert!(out.starts_with("## Skills\n\n### careful\n\n---\nname: careful"));
        assert!(out.len() > 100);
    }
}
