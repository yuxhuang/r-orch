//! The server's default agent identity (design D7).
//!
//! Identity is **attribution, not authorization**: it fills in a message
//! sender or an inbox owner the caller left out, and restricts nothing. It is
//! resolved once at startup — `--agent` over `ORCH_AGENT_ID` — and never
//! checked against the agent store, because a check at initialize would
//! neither authenticate the label nor stay true after a deletion.

use rmcp::ErrorData;

/// The default agent id, if the launch supplied one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Identity {
    default_agent: Option<String>,
}

impl Identity {
    /// Resolve the startup identity from the flag and the environment value
    /// (read once by the caller). A nonblank flag wins; a blank flag is an
    /// error rather than a silent fallback; a blank environment value counts
    /// as unset. The winning text is kept as given, never trimmed or mapped
    /// from a name to an id.
    pub(crate) fn resolve(flag: Option<&str>, env: Option<&str>) -> orch_core::Result<Identity> {
        if let Some(flag) = flag {
            if flag.trim().is_empty() {
                return Err(orch_core::Error::invalid_arguments(
                    "--agent must not be blank",
                ));
            }
            return Ok(Identity::of(Some(flag)));
        }
        Ok(Identity::of(env.filter(|value| !value.trim().is_empty())))
    }

    pub(crate) fn of(default_agent: Option<&str>) -> Identity {
        Identity {
            default_agent: default_agent.map(str::to_owned),
        }
    }

    /// The default agent id, if any.
    pub(crate) fn default_agent(&self) -> Option<&str> {
        self.default_agent.as_deref()
    }

    /// The sender of `msg_send`/`msg_broadcast`: the explicit `from`, else the
    /// default agent. Neither is an invalid-params error naming `field`; the
    /// CLI's `cli` fallback is not applied silently, though an explicit
    /// `"cli"` is accepted like any other label.
    #[allow(dead_code, reason = "seam consumed by the B2 tool groups")]
    pub(crate) fn sender(&self, field: &str, explicit: Option<&str>) -> Result<String, ErrorData> {
        self.explicit_or_default(field, explicit)
    }

    /// The owner of `msg_inbox`: the explicit agent id, else the default.
    #[allow(dead_code, reason = "seam consumed by the B2 tool groups")]
    pub(crate) fn inbox_agent(
        &self,
        field: &str,
        explicit: Option<&str>,
    ) -> Result<String, ErrorData> {
        self.explicit_or_default(field, explicit)
    }

    #[allow(dead_code, reason = "seam consumed by the B2 tool groups")]
    fn explicit_or_default(
        &self,
        field: &str,
        explicit: Option<&str>,
    ) -> Result<String, ErrorData> {
        match explicit {
            Some(value) if value.trim().is_empty() => Err(ErrorData::invalid_params(
                format!("{field} must not be blank"),
                None,
            )),
            Some(value) => Ok(value.to_owned()),
            None => self.default_agent.clone().ok_or_else(|| {
                ErrorData::invalid_params(
                    format!(
                        "{field} is required: this server has no default agent \
                         (start it with --agent or ORCH_AGENT_ID)"
                    ),
                    None,
                )
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_precedence_table_holds() {
        // (flag, env) -> default agent, or an error for a blank flag.
        type Row<'a> = (
            Option<&'a str>,
            Option<&'a str>,
            Result<Option<&'a str>, ()>,
        );
        let table: [Row; 9] = [
            (None, None, Ok(None)),
            (None, Some("agt_env"), Ok(Some("agt_env"))),
            (None, Some(""), Ok(None)),
            (None, Some("  \t"), Ok(None)),
            (Some("agt_flag"), None, Ok(Some("agt_flag"))),
            (Some("agt_flag"), Some("agt_env"), Ok(Some("agt_flag"))),
            (Some(""), Some("agt_env"), Err(())),
            (Some("   "), None, Err(())),
            // Nonblank text is kept exactly, padding included.
            (Some(" agt_x "), None, Ok(Some(" agt_x "))),
        ];
        for (flag, env, expected) in table {
            let actual = Identity::resolve(flag, env);
            match expected {
                Ok(agent) => assert_eq!(
                    actual.expect("resolves").default_agent(),
                    agent,
                    "{flag:?}/{env:?}"
                ),
                Err(()) => {
                    let err = actual.expect_err("blank flag");
                    assert_eq!(err.to_string(), "--agent must not be blank");
                    assert_eq!(err.exit_code(), 2);
                }
            }
        }
    }

    #[test]
    fn an_explicit_sender_beats_the_default_and_absence_without_one_is_invalid() {
        let with = Identity::of(Some("agt_default"));
        let without = Identity::default();

        assert_eq!(with.sender("from", Some("agt_c")).unwrap(), "agt_c");
        assert_eq!(with.sender("from", None).unwrap(), "agt_default");
        assert_eq!(without.sender("from", Some("cli")).unwrap(), "cli");

        let err = without.sender("from", None).expect_err("no sender");
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(
            err.message.starts_with("from is required"),
            "{}",
            err.message
        );

        let err = with.inbox_agent("agent_id", Some(" ")).expect_err("blank");
        assert_eq!(err.message, "agent_id must not be blank");
        assert_eq!(with.inbox_agent("agent_id", None).unwrap(), "agt_default");
        assert!(without.inbox_agent("agent_id", None).is_err());
    }
}
