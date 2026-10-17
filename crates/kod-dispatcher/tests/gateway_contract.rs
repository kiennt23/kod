mod common;

use common::FakeGateway;
use kod_dispatcher::{Gateway, RunStart, check_absent_run};

#[test]
fn the_fake_accepts_revoke_and_stop_for_a_never_started_run() {
    let gateway = FakeGateway::default();
    check_absent_run(&gateway, "never-started").unwrap();
    assert_eq!(gateway.revoked(), ["never-started", "never-started"]);
    assert_eq!(
        gateway.calls(),
        ["stop never-started", "stop never-started"]
    );
}

struct RejectsMissing(bool);

impl Gateway for RejectsMissing {
    fn start(&self, _: &RunStart) -> anyhow::Result<()> {
        unreachable!()
    }
    fn revoke(&self, _: &str) -> anyhow::Result<()> {
        if self.0 {
            anyhow::bail!("unknown run");
        }
        Ok(())
    }
    fn stop(&self, _: &str) -> anyhow::Result<()> {
        if !self.0 {
            anyhow::bail!("unknown run");
        }
        Ok(())
    }
    fn stop_all(&self) -> anyhow::Result<()> {
        unreachable!()
    }
    fn publish(&self, _: &str) -> anyhow::Result<String> {
        unreachable!()
    }
    fn find_published(&self, _: &RunStart) -> anyhow::Result<Option<String>> {
        unreachable!()
    }
}

#[test]
fn the_contract_refuses_an_adapter_that_errors_on_either_absent_operation() {
    for rejects_revoke in [true, false] {
        let result = check_absent_run(&RejectsMissing(rejects_revoke), "never-started");
        assert!(
            result.is_err(),
            "accepted an adapter that rejects an absent run"
        );
    }
}
