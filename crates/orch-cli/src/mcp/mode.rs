//! The launch mode and the operation classes it admits (design D6).
//!
//! The mode is fixed when the process starts; nothing changes it afterwards.
//! A class is an access-policy category, not the meaning of the SDK's
//! `destructiveHint`: an overwrite can carry `destructiveHint: true` and still
//! be an ordinary [`Class::Mutate`] tool.

/// What a tool does to the project, for the mode gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    /// Reads stored state. Reads can still maintain storage (index rebuild,
    /// context expiry purge); read-only is not an immutable filesystem.
    Read,
    /// An ordinary authored mutation.
    #[allow(dead_code, reason = "seam consumed by the B2 tool groups")]
    Mutate,
    /// One of the expressly approved destructive operations.
    Destructive,
}

/// How the server was launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Reads and ordinary mutations.
    Default,
    /// `--read-only`: reads only.
    ReadOnly,
    /// `--allow-destructive`: everything.
    AllowDestructive,
}

impl Mode {
    /// The mode the two launch flags select. clap rejects both together.
    pub(crate) fn from_flags(read_only: bool, allow_destructive: bool) -> Mode {
        if read_only {
            Mode::ReadOnly
        } else if allow_destructive {
            Mode::AllowDestructive
        } else {
            Mode::Default
        }
    }

    /// Whether a tool of `class` is registered, and callable, in this mode.
    pub(crate) fn allows(self, class: Class) -> bool {
        match self {
            Mode::ReadOnly => class == Class::Read,
            Mode::Default => class != Class::Destructive,
            Mode::AllowDestructive => true,
        }
    }

    /// The label the initialize instructions use.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Mode::Default => "default",
            Mode::ReadOnly => "read-only",
            Mode::AllowDestructive => "allow-destructive",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_mode_admits_exactly_its_classes() {
        let table = [
            (Mode::ReadOnly, [true, false, false]),
            (Mode::Default, [true, true, false]),
            (Mode::AllowDestructive, [true, true, true]),
        ];
        for (mode, expected) in table {
            let actual = [Class::Read, Class::Mutate, Class::Destructive].map(|c| mode.allows(c));
            assert_eq!(actual, expected, "{mode:?}");
        }
    }

    #[test]
    fn the_flags_select_the_mode() {
        assert_eq!(Mode::from_flags(false, false), Mode::Default);
        assert_eq!(Mode::from_flags(true, false), Mode::ReadOnly);
        assert_eq!(Mode::from_flags(false, true), Mode::AllowDestructive);
        assert_eq!(Mode::Default.label(), "default");
        assert_eq!(Mode::ReadOnly.label(), "read-only");
        assert_eq!(Mode::AllowDestructive.label(), "allow-destructive");
    }
}
