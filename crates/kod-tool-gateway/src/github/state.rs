use super::*;
use std::collections::BTreeSet;
#[derive(Default)]
pub(super) struct State {
    pub records: BTreeMap<String, store::Record>,
    pub last_time: i64,
    pub tokens: BTreeMap<String, String>,
    pub branches: BTreeMap<String, String>,
    pub cleanup: BTreeSet<String>,
    pub inflight: BTreeSet<String>,
    pub operations: BTreeMap<String, Arc<Mutex<()>>>,
}
impl State {
    pub fn new(records: BTreeMap<String, store::Record>, last_time: i64) -> Result<Self> {
        let mut state = Self {
            last_time,
            ..Default::default()
        };
        for record in records.into_values() {
            state.insert(record)?;
        }
        Ok(state)
    }
    pub fn insert(&mut self, record: store::Record) -> Result<()> {
        let run = &record.permit.run;
        ensure!(
            self.branches
                .get(&record.permit.branch())
                .is_none_or(|old| old == run),
            "GitHub run branch already bound"
        );
        for t in &record.tokens {
            ensure!(
                self.tokens.get(&t.secret).is_none_or(|old| old == run),
                "GitHub credential already bound"
            );
        }
        if let Some(old) = self.records.get(run) {
            for t in &old.tokens {
                self.tokens.remove(&t.secret);
            }
        }
        for t in &record.tokens {
            self.tokens.insert(t.secret.clone(), run.clone());
        }
        if record.tokens.is_empty() && record.pending.is_none() {
            self.cleanup.remove(run);
        } else {
            self.cleanup.insert(run.clone());
        }
        self.branches.insert(record.permit.branch(), run.clone());
        self.operations.entry(run.clone()).or_default();
        self.records.insert(run.clone(), record);
        Ok(())
    }
}
