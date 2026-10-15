//! What the model sees besides the conversation: the base prompt, the repo's
//! AGENTS.md and an index of its skills, all read at runtime. Also keeps the
//! conversation under a size limit.
use serde_json::{Value, json};
use std::{fs, path::Path};

/// Skill directories, in order; the first skill of a name wins.
const SKILL_DIRS: [&str; 2] = [".agents/skills", ".claude/skills"];

pub const ELIDED: &str = "[elided to save context; run the tool again if you need this]";

pub struct Skill {
    pub name: String,
    pub description: String,
    /// Relative to the workspace.
    pub path: String,
}

pub fn system_prompt(base: &str, workspace: &Path) -> String {
    let mut s = base.trim_end().to_owned();
    if let Ok(a) = fs::read_to_string(workspace.join("AGENTS.md")) {
        s += "\n\n# Repository instructions (AGENTS.md)\n\n";
        s += a.trim_end();
    }
    let skills = skills(workspace);
    if !skills.is_empty() {
        s += "\n\n# Skills\n\nRead a skill's SKILL.md with read_file before you use it.\n\n";
        for k in skills {
            if k.description.is_empty() {
                s += &format!("- {} ({})\n", k.name, k.path);
            } else {
                s += &format!("- {}: {} ({})\n", k.name, k.description, k.path);
            }
        }
    }
    s
}

pub fn skills(workspace: &Path) -> Vec<Skill> {
    let mut out: Vec<Skill> = vec![];
    for root in SKILL_DIRS {
        let Ok(entries) = fs::read_dir(workspace.join(root)) else {
            continue;
        };
        let mut dirs: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        dirs.sort();
        for d in dirs {
            let path = format!("{root}/{d}/SKILL.md");
            let Ok(text) = fs::read_to_string(workspace.join(&path)) else {
                continue;
            };
            let name = front(&text, "name").unwrap_or(d);
            if out.iter().any(|k| k.name == name) {
                continue;
            }
            out.push(Skill {
                name,
                description: front(&text, "description").unwrap_or_default(),
                path,
            });
        }
    }
    out
}

/// ponytail: single-line `key: value` front matter only; use a YAML parser
/// if skills ever need more.
fn front(text: &str, key: &str) -> Option<String> {
    let body = text.strip_prefix("---\n")?;
    let end = body.find("\n---")?;
    body[..end]
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
        .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_owned())
        .filter(|v| !v.is_empty())
}

/// Once the system prompt plus the serialized conversation is over
/// `max_bytes`, replaces tool results, oldest first, until the total is at
/// most half of `max_bytes`. Going down to the low-water mark means elision
/// happens rarely, so the cached prefix stays the same between elisions. The
/// last message is never touched, so the model always sees its newest results.
/// ponytail: tool inputs (such as write_file content) are kept; the gateway's
/// body limit refuses a request that is still too large.
pub fn elide(messages: &mut [Value], system_bytes: usize, max_bytes: usize) {
    let size = |m: &[Value]| system_bytes + serde_json::to_string(m).map_or(0, |s| s.len());
    if size(messages) <= max_bytes {
        return;
    }
    let last = messages.len().saturating_sub(1);
    for i in 0..last {
        if size(messages) <= max_bytes / 2 {
            return;
        }
        let Some(blocks) = messages[i]["content"].as_array_mut() else {
            continue;
        };
        for b in blocks.iter_mut().filter(|b| b["type"] == "tool_result") {
            b["content"] = json!(ELIDED);
        }
    }
}
