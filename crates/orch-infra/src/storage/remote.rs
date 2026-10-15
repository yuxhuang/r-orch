//! Persistent peer registry and read-only remote agent directory.

use orch_core::error::{Error, Result};
use orch_domain::{Agent, AgentStatus, InboundStatus, RemoteConfig, RemoteRef, RemoteStatus};

use super::fs_utils::{atomic_write, ensure_dir, read_json, read_yaml, write_json, write_yaml};
use super::lock::{check_lock, is_stale_by_age};
use super::paths::{Paths, sanitize_id};

/// Display name fallback; persisted configuration and CLI overrides take precedence.
pub fn default_peer_name() -> String {
    for key in ["HOSTNAME", "HOST"] {
        if let Ok(name) = std::env::var(key)
            && let Some(name) = short_hostname(&name)
        {
            return name;
        }
    }
    if let Ok(output) = std::process::Command::new("/bin/hostname").output()
        && output.status.success()
        && let Some(name) = short_hostname(&String::from_utf8_lossy(&output.stdout))
    {
        return name;
    }
    "localhost".into()
}

fn short_hostname(name: &str) -> Option<String> {
    let name = name.trim().split('.').next()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

/// Registry aliases are also safe, unambiguous filename components.
pub fn validate_alias(alias: &str) -> Result<()> {
    if alias
        .as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && alias
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        Ok(())
    } else {
        Err(Error::invalid_arguments(format!(
            "Invalid remote alias: {alias:?}; expected [a-z0-9][a-z0-9-]*"
        )))
    }
}

/// Reject nested overlay identifiers before storing a peer snapshot.
pub fn validate_snapshot(agents: &[Agent]) -> Result<()> {
    for agent in agents {
        sanitize_id(&agent.id)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct FileRemoteStore {
    paths: Paths,
}

// Like the other file stores, serialize read-modify-write within this process.
static REGISTRY_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl FileRemoteStore {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    pub async fn list(&self) -> Result<Vec<RemoteConfig>> {
        let entries: Vec<RemoteConfig> = read_yaml(&self.paths.remotes_path())
            .await?
            .unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        for entry in &entries {
            validate_alias(&entry.alias)?;
            if !seen.insert(&entry.alias) {
                return Err(Error::other(format!(
                    "Duplicate remote alias: {}",
                    entry.alias
                )));
            }
        }
        Ok(entries)
    }

    pub async fn add(&self, config: RemoteConfig) -> Result<()> {
        validate_alias(&config.alias)?;
        if config.addr.trim().is_empty() {
            return Err(Error::invalid_arguments("Remote address must not be empty"));
        }
        let _guard = REGISTRY_MUTEX.lock().await;
        let mut entries = self.list().await?;
        if entries.iter().any(|entry| entry.alias == config.alias) {
            return Err(Error::other(format!(
                "Remote {} already exists",
                config.alias
            )));
        }
        entries.push(config);
        write_yaml(&self.paths.remotes_path(), &entries).await
    }

    pub async fn remove(&self, alias: &str) -> Result<()> {
        validate_alias(alias)?;
        let _guard = REGISTRY_MUTEX.lock().await;
        let mut entries = self.list().await?;
        if !entries.iter().any(|entry| entry.alias == alias) {
            return Err(Error::other(format!("Remote {alias} not found")));
        }
        entries.retain(|entry| entry.alias != alias);
        write_yaml(&self.paths.remotes_path(), &entries).await?;
        let path = self.paths.remotes_dir().join(format!("{alias}.json"));
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(Error::io("remove remote status", path, err)),
        }
    }

    async fn owner_is_live(&self) -> bool {
        check_lock(&self.paths.lock_path()).await.acquired
            && !is_stale_by_age(&self.paths.lock_path()).await
    }

    pub async fn read_status(&self, alias: &str) -> Result<Option<RemoteStatus>> {
        validate_alias(alias)?;
        let path = self.paths.remotes_dir().join(format!("{alias}.json"));
        let mut status: Option<RemoteStatus> = read_json(&path).await?;
        if let Some(status) = &mut status {
            if status.alias != alias {
                return Err(Error::other("Remote status alias does not match filename"));
            }
            validate_snapshot(&status.agents)?;
            if !self.owner_is_live().await {
                status.online = false;
            }
        }
        Ok(status)
    }

    pub async fn write_status(&self, status: &RemoteStatus) -> Result<()> {
        validate_alias(&status.alias)?;
        validate_snapshot(&status.agents)?;
        write_json(
            &self
                .paths
                .remotes_dir()
                .join(format!("{}.json", status.alias)),
            status,
        )
        .await
    }

    pub async fn read_inbound(&self) -> Result<Option<InboundStatus>> {
        let inbound: Option<InboundStatus> =
            read_json(&self.paths.remotes_dir().join("_inbound.json")).await?;
        if let Some(inbound) = &inbound {
            validate_snapshot(&inbound.agents)?;
        }
        Ok(inbound)
    }

    pub async fn write_inbound(&self, status: &InboundStatus) -> Result<()> {
        validate_snapshot(&status.agents)?;
        write_json(&self.paths.remotes_dir().join("_inbound.json"), status).await
    }

    /// Stable UUID v4. Publish by hard link so concurrent first readers cannot
    /// observe a partially written identity or replace an established one.
    pub async fn peer_id(&self) -> Result<String> {
        let path = self.paths.peer_id_path();
        match tokio::fs::read_to_string(&path).await {
            Ok(id) => return validated_peer_id(id),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(Error::io("read peer identity", &path, err)),
        }
        ensure_dir(&self.paths.root()).await?;
        // nanoid already provides OS-backed randomness. Restrict the alphabet
        // to hex, then set the version and RFC 4122 variant bits.
        const HEX: [char; 16] = [
            '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
        ];
        let mut hex = nanoid::nanoid!(32, &HEX).into_bytes();
        hex[12] = b'4';
        let variant = (hex[16] as char).to_digit(16).expect("hex alphabet");
        hex[16] = b"89ab"[(variant & 3) as usize];
        let hex = String::from_utf8(hex).expect("hex alphabet");
        let id = format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        );
        let temp = self
            .paths
            .root()
            .join(format!("peer-id-{}.tmp", nanoid::nanoid!()));
        atomic_write(&temp, &id).await?;
        let published = tokio::fs::hard_link(&temp, &path).await;
        let _ = tokio::fs::remove_file(&temp).await;
        match published {
            Ok(()) => Ok(id),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let id = tokio::fs::read_to_string(&path)
                    .await
                    .map_err(|err| Error::io("read peer identity", &path, err))?;
                validated_peer_id(id)
            }
            Err(err) => Err(Error::io("publish peer identity", &path, err)),
        }
    }
}

fn validated_peer_id(id: String) -> Result<String> {
    let id = id.trim();
    if id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
    {
        Ok(id.to_owned())
    } else {
        Err(Error::other("Invalid persisted peer identity"))
    }
}

#[derive(Debug, Clone)]
pub struct RemoteAgentDirectory {
    store: FileRemoteStore,
}

impl RemoteAgentDirectory {
    pub fn new(paths: Paths) -> Self {
        Self {
            store: FileRemoteStore::new(paths),
        }
    }

    pub async fn list(&self, mut local: Vec<Agent>) -> Result<Vec<Agent>> {
        for config in self.store.list().await? {
            let Some(status) = self.store.read_status(&config.alias).await? else {
                continue;
            };
            for mut agent in status.agents {
                agent.id = format!("{}@{}", agent.id, config.alias);
                agent.name = format!("{}@{}", agent.name, config.alias);
                agent.adapter = "remote".into();
                agent.autonomous = Some(false);
                agent.remote = Some(RemoteRef {
                    alias: config.alias.clone(),
                    online: status.online,
                });
                if !status.online {
                    agent.status = AgentStatus::Offline;
                }
                local.push(agent);
            }
        }
        Ok(local)
    }

    pub async fn resolve(&self, id: &str, local: Vec<Agent>) -> Result<Option<Agent>> {
        let agents = self.list(local).await?;
        Ok(agents
            .iter()
            .find(|agent| agent.id == id)
            .or_else(|| agents.iter().find(|agent| agent.name == id))
            .cloned())
    }

    pub async fn accepts_recipient(&self, recipient: &str) -> Result<bool> {
        let Some((id, alias)) = recipient.split_once('@') else {
            return Ok(false);
        };
        if sanitize_id(id).is_err() || alias.is_empty() || alias.contains('@') {
            return Ok(false);
        }
        if self
            .store
            .list()
            .await?
            .iter()
            .any(|config| config.alias == alias)
        {
            return Ok(true);
        }
        Ok(self
            .store
            .read_inbound()
            .await?
            .is_some_and(|inbound| inbound.controller_name == alias))
    }
}

#[cfg(test)]
mod tests {
    use super::super::lock::{LOCK_STALE, acquire_lock};
    use super::*;
    use orch_core::test_support::make_agent;

    fn config(alias: &str) -> RemoteConfig {
        RemoteConfig {
            alias: alias.into(),
            addr: "127.0.0.1:7690".into(),
            added_at: "2026-10-15T00:00:00Z".into(),
        }
    }

    fn status(alias: &str) -> RemoteStatus {
        let mut agent = make_agent();
        agent.capabilities = vec!["linux".into()];
        RemoteStatus {
            alias: alias.into(),
            addr: "127.0.0.1:7690".into(),
            online: true,
            last_seen: None,
            last_error: None,
            peer_name: Some("worker".into()),
            instance_id: None,
            os: Some("linux".into()),
            agents: vec![agent],
        }
    }

    #[test]
    fn peer_name_uses_short_hostname() {
        assert_eq!(short_hostname("worker.lan\n"), Some("worker".into()));
        assert_eq!(short_hostname("my-mac"), Some("my-mac".into()));
        assert_eq!(short_hostname(""), None);
        assert_eq!(short_hostname(".lan"), None);
        assert!(!default_peer_name().is_empty());
    }

    #[test]
    fn aliases_reject_paths_uppercase_and_overlay_delimiters() {
        for valid in ["w1", "2com", "worker-2", "a-"] {
            validate_alias(valid).unwrap();
        }
        for invalid in ["", "W1", "-w1", "w_1", "a@b", "../a", "a/b", "é"] {
            assert!(validate_alias(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[tokio::test]
    async fn registry_survives_new_store_and_removal_hides_cached_agents() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        let store = FileRemoteStore::new(paths.clone());
        assert!(store.list().await.unwrap().is_empty());
        store.add(config("w1")).await.unwrap();
        store.write_status(&status("w1")).await.unwrap();
        let reopened = FileRemoteStore::new(paths.clone());
        assert_eq!(reopened.list().await.unwrap(), vec![config("w1")]);
        assert!(store.add(config("w1")).await.is_err());
        store.remove("w1").await.unwrap();
        assert!(
            RemoteAgentDirectory::new(paths)
                .list(vec![])
                .await
                .unwrap()
                .is_empty()
        );
        assert!(store.read_status("w1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn directory_merges_local_and_remote_without_reexporting_or_mutating_local() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        let store = FileRemoteStore::new(paths.clone());
        store.add(config("w1")).await.unwrap();
        store.write_status(&status("w1")).await.unwrap();
        assert!(acquire_lock(&paths.lock_path()).await.unwrap().acquired);
        let directory = RemoteAgentDirectory::new(paths);
        let local = make_agent();
        let agents = directory.list(vec![local.clone()]).await.unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0], local);
        let remote = &agents[1];
        assert_eq!(remote.id, "agt_test1@w1");
        assert_eq!(remote.name, "TestAgent@w1");
        assert_eq!(remote.adapter, "remote");
        assert_eq!(remote.autonomous, Some(false));
        assert_eq!(remote.capabilities, vec!["linux"]);
        assert_eq!(remote.status, AgentStatus::Idle);
        assert_eq!(
            remote.remote,
            Some(RemoteRef {
                alias: "w1".into(),
                online: true
            })
        );
        assert_eq!(
            directory.resolve("agt_test1@w1", vec![]).await.unwrap(),
            Some(remote.clone())
        );
    }

    #[tokio::test]
    async fn missing_dead_and_stale_owner_lock_make_retained_agents_offline() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        let store = FileRemoteStore::new(paths.clone());
        store.add(config("w1")).await.unwrap();
        store.write_status(&status("w1")).await.unwrap();
        let directory = RemoteAgentDirectory::new(paths.clone());
        assert_eq!(
            directory.list(vec![]).await.unwrap()[0].status,
            AgentStatus::Offline
        );
        tokio::fs::write(paths.lock_path(), "0").await.unwrap();
        assert!(!store.read_status("w1").await.unwrap().unwrap().online);
        tokio::fs::write(paths.lock_path(), std::process::id().to_string())
            .await
            .unwrap();
        assert!(store.read_status("w1").await.unwrap().unwrap().online);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(paths.lock_path())
            .unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(
            std::time::SystemTime::now() - LOCK_STALE - std::time::Duration::from_secs(1),
        ))
        .unwrap();
        let agents = directory.list(vec![]).await.unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, AgentStatus::Offline);
        assert!(!agents[0].remote.as_ref().unwrap().online);
        // Read-time guard must not destroy the reported snapshot on disk.
        let persisted: RemoteStatus = read_json(&paths.remotes_dir().join("w1.json"))
            .await
            .unwrap()
            .unwrap();
        assert!(persisted.online);
    }

    #[tokio::test]
    async fn nested_remote_ids_are_rejected_on_snapshot_write_and_read() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        let store = FileRemoteStore::new(paths.clone());
        let mut bad = status("w1");
        bad.agents[0].id = "agt_test1@other".into();
        assert!(store.write_status(&bad).await.is_err());
        let inbound = InboundStatus {
            controller_name: "controller".into(),
            instance_id: "id".into(),
            connected_since: "now".into(),
            agents: bad.agents.clone(),
        };
        assert!(store.write_inbound(&inbound).await.is_err());
        write_json(&paths.remotes_dir().join("w1.json"), &bad)
            .await
            .unwrap();
        assert!(store.read_status("w1").await.is_err());
        write_json(&paths.remotes_dir().join("_inbound.json"), &inbound)
            .await
            .unwrap();
        assert!(store.read_inbound().await.is_err());
    }

    #[tokio::test]
    async fn recipients_accept_configured_offline_alias_or_last_inbound_controller() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        let store = FileRemoteStore::new(paths.clone());
        store.add(config("w1")).await.unwrap();
        store
            .write_inbound(&InboundStatus {
                controller_name: "my-mac".into(),
                instance_id: "id".into(),
                connected_since: "now".into(),
                agents: vec![make_agent()],
            })
            .await
            .unwrap();
        let directory = RemoteAgentDirectory::new(paths);
        assert!(directory.accepts_recipient("agt_missing@w1").await.unwrap());
        assert!(
            directory
                .accepts_recipient("agt_test1@my-mac")
                .await
                .unwrap()
        );
        for recipient in [
            "agt_test1",
            "@w1",
            "../x@w1",
            "agt_test1@unknown",
            "agt_test1@w1@other",
        ] {
            assert!(
                !directory.accepts_recipient(recipient).await.unwrap(),
                "accepted {recipient}"
            );
        }
        assert!(
            directory.list(vec![]).await.unwrap().is_empty(),
            "inbound agents must never become dispatch candidates"
        );
    }

    #[tokio::test]
    async fn peer_identity_is_uuid_v4_stable_and_race_safe() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        let a = FileRemoteStore::new(paths.clone());
        let b = FileRemoteStore::new(paths.clone());
        let (one, two) = tokio::join!(a.peer_id(), b.peer_id());
        let one = one.unwrap();
        assert_eq!(one, two.unwrap());
        assert_eq!(one.len(), 36);
        assert_eq!(&one[14..15], "4");
        assert!(matches!(one.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
        assert_eq!(
            one,
            FileRemoteStore::new(paths.clone()).peer_id().await.unwrap()
        );
        tokio::fs::write(paths.peer_id_path(), "broken")
            .await
            .unwrap();
        assert!(
            a.peer_id().await.is_err(),
            "corrupt identity must not silently change"
        );
    }
}
