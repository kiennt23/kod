//! The token broker. The gateway asks it for a credential inside the one
//! operation that needs it and drops it when the operation ends, so no
//! credential is ever in a tool result, an audit record or the sandbox.
use anyhow::Result;
use std::fmt;

/// A credential. It can't be printed or serialized; `expose` is for the
/// adapter call that sends it.
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Secret {
        Secret(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// Mints short-lived repository credentials belonging to one run, so a run's
/// end or the kill switch can revoke them. Concrete Forge adapters enforce
/// exact agent branches; GitHub installation tokens cannot encode that scope.
pub trait Broker: Send + Sync {
    fn mint(&self, run: &str, repo: &str) -> Result<Secret>;
    /// Revokes every credential minted for the run. Safe to repeat.
    fn revoke(&self, run: &str) -> Result<()>;
    /// Revokes every credential this broker minted.
    fn revoke_all(&self) -> Result<()>;
}
