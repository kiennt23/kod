use super::*;
use reqwest::Method;
#[derive(Deserialize)]
pub(super) struct Repository {
    pub id: u64,
    pub full_name: String,
}
#[derive(Deserialize)]
struct Identity {
    id: u64,
    full_name: String,
    default_branch: String,
}
#[derive(Deserialize)]
struct Repositories {
    total_count: u64,
    repositories: Vec<Repository>,
}
#[derive(Deserialize)]
struct Guard {
    id: u64,
    target: String,
    enforcement: String,
    bypass_actors: Vec<Bypass>,
    conditions: Conditions,
    rules: Vec<Rule>,
}
#[derive(Deserialize)]
struct Bypass {
    actor_id: u64,
    actor_type: String,
    bypass_mode: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Conditions {
    ref_name: Names,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Names {
    include: Vec<String>,
    exclude: Vec<String>,
}
#[derive(Deserialize)]
struct Rule {
    #[serde(rename = "type")]
    kind: String,
}
impl GitHub {
    pub(super) fn verify_scope(&self, token: &str, repos: Option<Vec<Repository>>) -> Result<()> {
        let repos = match repos {
            Some(repos) => repos,
            None => {
                let path = vec!["installation".into(), "repositories".into()];
                let mut repos = Vec::new();
                for page in 1..=100 {
                    let reply = self.http.request(
                        Method::GET,
                        token,
                        &path,
                        &[("per_page", "100".into()), ("page", page.to_string())],
                        None,
                    )?;
                    let data: Repositories = reply.json()?;
                    ensure!(data.total_count == 1, "overbroad GitHub repositories");
                    repos.extend(data.repositories);
                    if !self.http.next_page(reply.link.as_deref(), &path, page)? {
                        break;
                    }
                }
                repos
            }
        };
        ensure!(
            repos.len() == 1
                && repos[0].id == self.config.repository_id
                && repos[0].full_name == self.config.repo,
            "GitHub repository scope mismatch"
        );
        Ok(())
    }
    pub(super) fn protected(&self, token: &str) -> Result<()> {
        let repo: Identity = self
            .http
            .request(Method::GET, token, &self.repo_path(&[]), &[], None)?
            .json()?;
        ensure!(
            repo.id == self.config.repository_id
                && repo.full_name == self.config.repo
                && repo.default_branch == self.config.base_branch,
            "GitHub repository identity mismatch"
        );
        let guard: Guard = self
            .http
            .request(
                Method::GET,
                token,
                &self.repo_path(&["rulesets", &self.config.guard_ruleset_id.to_string()]),
                &[],
                None,
            )?
            .json()?;
        ensure!(
            guard.id == self.config.guard_ruleset_id
                && guard.target == "branch"
                && guard.enforcement == "active",
            "inactive GitHub branch guard"
        );
        ensure!(
            guard.conditions.ref_name.include == ["~ALL"]
                && guard.conditions.ref_name.exclude
                    == ["refs/heads/agent/*", "refs/heads/agent/**/*"],
            "unsupported GitHub guard scope"
        );
        ensure!(
            guard
                .bypass_actors
                .iter()
                .all(|b| b.actor_id > 0 && b.actor_type == "Team" && b.bypass_mode == "always"),
            "unsupported GitHub guard bypass"
        );
        for required in ["creation", "update", "deletion"] {
            ensure!(
                guard.rules.iter().filter(|r| r.kind == required).count() == 1,
                "missing GitHub branch restriction"
            );
        }
        Ok(())
    }
}
