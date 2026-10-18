//! Directory-template command implementation seam.

/// No template operations are registered yet; keep unknown invocations guarded.
pub(crate) fn requires_initialized_project(_args: &[String]) -> bool {
    true
}
