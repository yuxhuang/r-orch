//! Persistent peer registry and listening preferences. Transport belongs to the owner.
use super::rpc::Rpc;
use orch_core::error::{Error, Result};
use orch_domain::RemoteStatus;
use orch_proto::ops::args;

use crate::{
    context::CliContext,
    output::{Color, Icon, Styles, print_success, print_table},
    tree::RemoteSub,
};

/// Where `remote listen on` and a bare `--listen` bind: this host only. The
/// peer protocol has no authentication, so any address reachable from other
/// hosts must be typed explicitly (security review F1).
pub const DEFAULT_LISTEN: &str = "127.0.0.1:7690";

pub async fn run(
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    command: &RemoteSub,
) -> Result<()> {
    match command {
        RemoteSub::Add { alias, addr } => {
            let addr = normalize_addr(addr)?;
            engine
                .call::<args::RemoteAddArgs>(serde_json::json!({"alias":alias,"addr":addr}))
                .await?;
            print_success(styles, &format!("Added remote {alias}"));
        }
        RemoteSub::Remove { alias } => {
            engine
                .call::<args::RemoteRemoveArgs>(serde_json::json!({"alias":alias}))
                .await?;
            print_success(styles, &format!("Removed remote {alias}"));
        }
        RemoteSub::List => {
            let entries = engine.request(args::RemoteListArgs {}).await?;
            if let Some(format) = ctx.output {
                return format.print(&entries);
            }
            let now = now_ms();
            let report = engine.request(args::RemoteStatusArgs {}).await?;
            let mut rows = Vec::new();
            for entry in entries {
                let status = report
                    .remotes
                    .iter()
                    .find(|status| status.alias == entry.alias);
                rows.push(vec![
                    entry.alias,
                    entry.addr,
                    status
                        .as_ref()
                        .map(|status| describe_link(status, now))
                        .unwrap_or_else(|| "offline".into()),
                ]);
            }
            print_table(styles, &["ALIAS", "ADDRESS", "STATUS"], &rows, 2);
        }
        RemoteSub::Status => {
            let report = engine.request(args::RemoteStatusArgs {}).await?;
            let configured = report.listen_configured.as_ref();
            let inbound = report.inbound.as_ref();
            let remotes = &report.remotes;
            if let Some(format) = ctx.output {
                return format.print(&report);
            }
            println!(
                "  Listen: {}{}",
                describe_listen(report.listen.as_deref(), configured.map(String::as_str)),
                report
                    .last_session_error
                    .as_ref()
                    .map(|error| format!(" (last session ended: {error})"))
                    .unwrap_or_default()
            );
            println!(
                "  Controller: {}{}",
                report.controller.as_deref().unwrap_or("none"),
                inbound
                    .as_ref()
                    .filter(|_| report.controller.is_some())
                    .and_then(|inbound| inbound.controller_version.as_deref())
                    .map(|version| format!(" (v{version})"))
                    .unwrap_or_default()
            );
            let now = now_ms();
            for status in remotes {
                println!(
                    "  {}: {}{}",
                    status.alias,
                    describe_link(status, now),
                    status
                        .peer_version
                        .as_ref()
                        .map(|version| format!(" v{version}"))
                        .unwrap_or_else(|| if status.online {
                            // A peer that predates the field, so there is no
                            // version to show and nothing to infer from it.
                            " (version unknown)".into()
                        } else {
                            String::new()
                        })
                );
            }
        }
        RemoteSub::Listen { state, addr } => {
            if state == "off" && addr.is_some() {
                return Err(Error::invalid_arguments(
                    "remote listen off does not accept an address",
                ));
            }
            let listen = if state == "on" {
                Some(normalize_listen(addr.as_deref().unwrap_or(DEFAULT_LISTEN))?)
            } else {
                None
            };
            engine
                .call::<args::RemoteListenArgs>(
                    serde_json::json!({"enabled":state == "on","addr":listen}),
                )
                .await?;
            print_success(
                styles,
                &match &listen {
                    Some(listen) => {
                        format!("Peer listening on {listen}; takes effect when the engine starts")
                    }
                    None => "Peer listening off; takes effect when the engine starts".into(),
                },
            );
            if let Some(listen) = &listen {
                warn_if_exposed(styles, listen);
            }
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(i64::MAX as u128) as i64
        })
}

/// One wording for the CLI and the TUI: what this link is doing, and when it
/// will try again. `offline` alone covered a worker that is down, one serving
/// another controller, and one being redialled every second — which is how
/// the live redial storm stayed invisible.
pub(crate) fn describe_link(status: &RemoteStatus, now_ms: i64) -> String {
    if status.online {
        return "online".into();
    }
    let cause = status.last_error.clone().or_else(|| {
        (status.link_state == orch_domain::LinkState::Connecting).then(|| "connecting".into())
    });
    let mut retry = Vec::new();
    if let Some(due) = status
        .next_retry_at
        .as_deref()
        .and_then(orch_core::clock::epoch_ms_from_iso)
    {
        retry.push(format!(
            "retry in {}s",
            due.saturating_sub(now_ms).max(0) / 1000
        ));
    }
    if status.attempts > 1 {
        retry.push(format!("attempt {}", status.attempts));
    }
    match (cause, retry.join(", ")) {
        (None, retry) if retry.is_empty() => "offline".into(),
        (None, retry) => format!("offline ({retry})"),
        (Some(cause), retry) if retry.is_empty() => format!("offline ({cause})"),
        (Some(cause), retry) => format!("offline ({cause}; {retry})"),
    }
}

/// One wording for the CLI and the TUI header: only a bound socket is shown
/// as an address; a configured preference is labelled as such.
pub(crate) fn describe_listen(active: Option<&str>, configured: Option<&str>) -> String {
    match (active, configured) {
        (Some(addr), _) if exposure_warning(addr).is_some() => {
            format!("{addr} (EXPOSED, no auth)")
        }
        (Some(addr), _) => addr.to_owned(),
        (None, Some(configured)) => format!("off (configured {configured}, not active)"),
        (None, None) => "off".to_owned(),
    }
}

/// The listener's own wording, shared with the daemon's start-up warning.
pub use orch_domain::remote::exposure_warning;

/// Prints [`exposure_warning`] on stderr, so it survives a redirected or
/// machine-read stdout (`serve --log-format json`).
pub(crate) fn warn_if_exposed(styles: &Styles, addr: &str) {
    if let Some(warning) = exposure_warning(addr) {
        let glyph = styles.paint(Color::Yellow, styles.icon(Icon::Warning));
        eprintln!("  {glyph} {warning}");
    }
}

pub fn normalize_listen(addr: &str) -> Result<String> {
    addr.parse::<std::net::SocketAddr>()
        .map(|a| a.to_string())
        .map_err(|_| Error::invalid_arguments("Listen address must be an IP address and port"))
}

fn normalize_addr(addr: &str) -> Result<String> {
    if addr.is_empty() || addr.chars().any(char::is_whitespace) || addr.contains('/') {
        return Err(Error::invalid_arguments(
            "Remote address must be host[:port]",
        ));
    }
    if addr.starts_with('[') {
        if addr.parse::<std::net::SocketAddr>().is_ok() {
            return Ok(addr.into());
        }
        return Err(Error::invalid_arguments(
            "IPv6 remote addresses require [address]:port",
        ));
    }
    if let Some((host, port)) = addr.split_once(':') {
        if host.is_empty() || port.parse::<u16>().is_err() {
            return Err(Error::invalid_arguments(
                "Remote address must be host[:port]",
            ));
        }
        Ok(addr.into())
    } else {
        Ok(format!("{addr}:7690"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_links_description_says_what_it_is_doing_and_when_it_retries() {
        let base = RemoteStatus {
            alias: "w1".into(),
            addr: "127.0.0.1:7690".into(),
            online: false,
            last_seen: None,
            last_error: None,
            peer_name: None,
            instance_id: None,
            os: None,
            agents: Vec::new(),
            peer_version: None,
            link_state: orch_domain::LinkState::Connecting,
            next_retry_at: None,
            attempts: 0,
        };
        let now = 1_000_000;
        assert_eq!(describe_link(&base, now), "offline (connecting)");
        assert_eq!(
            describe_link(
                &RemoteStatus {
                    online: true,
                    link_state: orch_domain::LinkState::Online,
                    attempts: 5,
                    ..base.clone()
                },
                now
            ),
            "online"
        );
        // The live failure: a worker that closed every session right after
        // the handshake looked exactly like one that was simply down.
        assert_eq!(
            describe_link(
                &RemoteStatus {
                    last_error: Some("Peer closed connection".into()),
                    link_state: orch_domain::LinkState::Backoff,
                    next_retry_at: Some(orch_core::clock::iso_from_epoch_ms(now + 12_400)),
                    attempts: 5,
                    ..base.clone()
                },
                now
            ),
            "offline (Peer closed connection; retry in 12s, attempt 5)"
        );
        // A retry already due, and a first attempt, carry no counters.
        assert_eq!(
            describe_link(
                &RemoteStatus {
                    last_error: Some("Worker busy: controlled by mac".into()),
                    link_state: orch_domain::LinkState::Busy,
                    next_retry_at: Some(orch_core::clock::iso_from_epoch_ms(now - 5_000)),
                    attempts: 1,
                    ..base.clone()
                },
                now
            ),
            "offline (Worker busy: controlled by mac; retry in 0s)"
        );
        // A record written before these fields still reads sensibly.
        assert_eq!(
            describe_link(
                &RemoteStatus {
                    last_error: Some("Peer heartbeat timed out".into()),
                    ..base
                },
                now
            ),
            "offline (Peer heartbeat timed out)"
        );
    }

    #[test]
    fn describe_listen_never_presents_a_preference_as_a_listener() {
        assert_eq!(
            describe_listen(Some("127.0.0.1:50162"), Some("127.0.0.1:0")),
            "127.0.0.1:50162"
        );
        assert_eq!(
            describe_listen(None, Some("127.0.0.1:0")),
            "off (configured 127.0.0.1:0, not active)"
        );
        assert_eq!(describe_listen(None, None), "off");
    }

    #[test]
    fn describe_listen_marks_an_active_network_listener_as_exposed() {
        for addr in ["0.0.0.0:7690", "[::]:7690", "192.168.1.5:7690"] {
            assert_eq!(
                describe_listen(Some(addr), None),
                format!("{addr} (EXPOSED, no auth)")
            );
        }
        assert_eq!(describe_listen(Some("[::1]:7690"), None), "[::1]:7690");
    }

    #[test]
    fn loopback_listen_addresses_need_no_exposure_warning() {
        for addr in [
            DEFAULT_LISTEN,
            "127.0.0.1:0",
            "127.8.9.10:7690",
            "[::1]:7690",
        ] {
            assert_eq!(exposure_warning(addr), None, "{addr}");
        }
    }

    #[test]
    fn wildcard_listen_addresses_warn_about_all_interfaces() {
        for addr in ["0.0.0.0:7690", "[::]:7690"] {
            let warning = exposure_warning(addr).expect(addr);
            for needle in [
                addr,
                "ALL network interfaces",
                "no authentication",
                "127.0.0.1",
            ] {
                assert!(warning.contains(needle), "{needle} missing: {warning}");
            }
        }
    }

    #[test]
    fn specific_network_and_unparsed_listen_addresses_warn() {
        let lan = exposure_warning("192.168.1.5:7690").expect("LAN address");
        assert!(lan.contains("192.168.1.5:7690") && lan.contains("no authentication"));
        assert!(!lan.contains("ALL network interfaces"), "{lan}");
        // `peer.listen` set by hand is bound as given; never stay silent about
        // an address this check cannot prove is loopback.
        assert!(exposure_warning("localhost:7690").is_some());
    }

    #[test]
    fn address_defaults_and_validation() {
        assert_eq!(normalize_addr("worker").unwrap(), "worker:7690");
        assert_eq!(normalize_addr("worker:1234").unwrap(), "worker:1234");
        assert!(normalize_addr("worker:not-a-port").is_err());
        assert!(normalize_addr("../worker").is_err());
        assert!(normalize_listen("127.0.0.1:0").is_ok());
        assert!(normalize_listen("worker:1234").is_err());
    }
}
