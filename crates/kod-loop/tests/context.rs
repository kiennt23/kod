use kod_loop::context::{ELIDED, elide, skills, system_prompt};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn put(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

#[test]
fn system_prompt_appends_agents_md_and_a_skill_index() {
    let d = tempfile::tempdir().unwrap();
    put(d.path(), "AGENTS.md", "Always run make lint.\n");
    put(
        d.path(),
        ".agents/skills/review/SKILL.md",
        "---\nname: review\ndescription: Review a diff\n---\nSKILL BODY\n",
    );
    put(
        d.path(),
        ".claude/skills/review/SKILL.md",
        "---\nname: review\ndescription: other\n---\n",
    );
    put(
        d.path(),
        ".claude/skills/deploy/SKILL.md",
        "no front matter\n",
    );
    let s = system_prompt("BASE\n", d.path());
    assert!(
        s.starts_with(
            "BASE\n\n# Repository instructions (AGENTS.md)\n\nAlways run make lint.\n\n# Skills\n"
        ),
        "{s}"
    );
    assert!(
        s.contains("- review: Review a diff (.agents/skills/review/SKILL.md)\n"),
        "{s}"
    );
    assert!(
        s.contains("- deploy (.claude/skills/deploy/SKILL.md)\n"),
        "{s}"
    );
    assert!(!s.contains("other"), "the first skill of a name wins: {s}");
    assert!(
        !s.contains("SKILL BODY"),
        "skill bodies are read on demand: {s}"
    );
}

#[test]
fn without_agents_md_or_skills_it_is_just_the_base() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(system_prompt("BASE\n", d.path()), "BASE");
}

#[test]
fn front_matter_values_may_be_quoted_and_must_be_closed() {
    let d = tempfile::tempdir().unwrap();
    put(
        d.path(),
        ".agents/skills/a/SKILL.md",
        "---\nname: \"x y\"\ndescription: 'quoted'\n---\n",
    );
    put(d.path(), ".agents/skills/b/SKILL.md", "---\nname: z\n");
    let s = skills(d.path());
    let got: Vec<(&str, &str)> = s
        .iter()
        .map(|k| (k.name.as_str(), k.description.as_str()))
        .collect();
    assert_eq!(got, [("x y", "quoted"), ("b", "")]);
}

fn result(id: &str, n: usize) -> Value {
    json!({"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": id, "content": "x".repeat(n)}
    ]})
}

fn conversation() -> Vec<Value> {
    vec![
        json!({"role": "user", "content": "ticket"}),
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": "a", "name": "run", "input": {}}]}),
        result("a", 10_000),
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": "b", "name": "run", "input": {}}]}),
        result("b", 10_000),
    ]
}

#[test]
fn elide_replaces_the_oldest_results_first_and_never_the_last_message() {
    let mut m = conversation();
    elide(&mut m, 0, 15_000);
    assert_eq!(m[2]["content"][0]["content"], ELIDED);
    assert_eq!(
        m[4]["content"][0]["content"].as_str().unwrap().len(),
        10_000
    );
    assert_eq!(m[2]["content"][0]["tool_use_id"], "a", "ids stay paired");

    let mut m = conversation();
    elide(&mut m, 0, 100);
    assert_eq!(m[2]["content"][0]["content"], ELIDED);
    assert_eq!(
        m[4]["content"][0]["content"].as_str().unwrap().len(),
        10_000,
        "the newest result stays even over the limit"
    );
}

#[test]
fn elide_goes_down_to_half_the_limit_so_the_prefix_stays_stable() {
    let mut m = conversation();
    m.push(json!({"role": "assistant", "content": [{"type": "tool_use", "id": "c", "name": "run", "input": {}}]}));
    m.push(result("c", 10_000));
    // About 30 000 bytes: over 25 000, so it elides down to 12 500, which
    // takes both older results, not just the first.
    elide(&mut m, 0, 25_000);
    assert_eq!(m[2]["content"][0]["content"], ELIDED);
    assert_eq!(m[4]["content"][0]["content"], ELIDED);
    assert_eq!(
        m[6]["content"][0]["content"].as_str().unwrap().len(),
        10_000
    );
}

#[test]
fn elide_counts_the_system_prompt() {
    // About 20 000 bytes of conversation fits in 30 000, but not with a
    // 15 000-byte system prompt.
    let mut m = conversation();
    elide(&mut m, 0, 30_000);
    assert_eq!(m, conversation());
    elide(&mut m, 15_000, 30_000);
    assert_eq!(m[2]["content"][0]["content"], ELIDED);
}

#[test]
fn elide_does_nothing_under_the_limit() {
    let mut m = conversation();
    elide(&mut m, 0, 1_000_000);
    assert_eq!(m, conversation());
}
