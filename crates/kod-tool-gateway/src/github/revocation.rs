use super::*;
use reqwest::{Method, StatusCode};
use std::collections::BTreeSet;
impl GitHub {
    pub(super) fn revoke_token(&self, secret: &str) -> bool {
        self.http
            .request(
                Method::DELETE,
                secret,
                &["installation".into(), "token".into()],
                &[],
                None,
            )
            .is_ok_and(|r| matches!(r.status, StatusCode::NO_CONTENT | StatusCode::UNAUTHORIZED))
    }
    pub(super) fn revoke_runs(&self, ids: Vec<String>) -> Result<()> {
        let (snapshots, safe_time, mut failed) = {
            let mut s = self.lock()?;
            let wall = (self.clock)();
            let safe_time = (wall > 0 && wall >= s.last_time).then_some(wall);
            let updated = wall.max(s.last_time).max(1);
            s.last_time = updated;
            let mut snapshots = Vec::new();
            let mut failed = false;
            for run in ids {
                let Some(mut r) = s.records.get(&run).cloned() else {
                    continue;
                };
                let active = !r.tokens.is_empty() || r.pending.is_some();
                r.generation = r
                    .generation
                    .checked_add(1)
                    .context("GitHub revocation generation overflow")?;
                for token in &mut r.tokens {
                    token.usable = false;
                }
                if r.pending
                    .is_some_and(|until| safe_time.is_some_and(|now| until <= now))
                {
                    r.pending = None;
                }
                if !active {
                    s.insert(r)?;
                    continue;
                }
                r.updated_at = updated;
                if self.store.save(&r).is_err() {
                    failed = true;
                }
                snapshots.push(r.clone());
                s.insert(r)?;
            }
            (snapshots, safe_time, failed)
        };
        for snapshot in snapshots {
            let removed: BTreeSet<_> = snapshot
                .tokens
                .iter()
                .filter(|t| {
                    safe_time.is_some_and(|now| t.expires.saturating_add(60) <= now)
                        || self.revoke_token(&t.secret)
                })
                .map(|t| t.secret.clone())
                .collect();
            let mut s = self.lock()?;
            let mut current = s
                .records
                .get(&snapshot.permit.run)
                .context("missing GitHub cleanup record")?
                .clone();
            current.tokens.retain(|t| !removed.contains(&t.secret));
            if current.pending == snapshot.pending
                && current
                    .pending
                    .is_some_and(|until| safe_time.is_some_and(|now| until <= now))
            {
                current.pending = None;
            }
            let pending = !current.tokens.is_empty() || current.pending.is_some();
            if self.store.save(&current).is_err() {
                failed = true;
            } else {
                s.insert(current)?;
            }
            failed |= pending;
        }
        ensure!(!failed, "GitHub credential cleanup pending");
        Ok(())
    }
}
