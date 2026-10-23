use kod_tool_gateway::{
    broker::Broker,
    forge::Forge,
    github::{AppCredentials, Config, GitHub, RunPermit},
};
use std::{path::Path, sync::Arc};
fn production(
    c: Config,
    k: AppCredentials,
    leases: &Path,
    p: RunPermit,
) -> anyhow::Result<(Arc<dyn Broker>, Arc<dyn Forge>)> {
    let a = Arc::new(GitHub::new(c, k, leases)?);
    a.recover()?;
    a.verify_repository()?;
    a.register_run(p)?;
    Ok((a.clone(), a))
}
#[test]
fn public_api_compiles() {
    let _constructor = production;
}
#[cfg(feature = "test-support")]
#[test]
fn test_support_api_compiles() {
    fn fixture(
        c: Config,
        k: AppCredentials,
        leases: &Path,
        api: &str,
        clock: kod_tool_gateway::github::TestClock,
    ) -> anyhow::Result<GitHub> {
        GitHub::with_test_api(c, k, leases, api, clock)
    }
    let _constructor = fixture;
}
