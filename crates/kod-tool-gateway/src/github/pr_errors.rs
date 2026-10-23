use serde::Deserialize;

#[derive(Deserialize)]
struct Rejection {
    message: String,
    #[serde(default)]
    errors: Vec<ValidationError>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum ValidationError {
    Message(String),
    Detail {
        resource: Option<String>,
        field: Option<String>,
        code: Option<String>,
        message: Option<String>,
    },
}
fn message_cause(message: &str) -> Option<&'static str> {
    let message = message.to_ascii_lowercase();
    if message.starts_with("no commits between ") {
        Some("head has no commits beyond base; publish a commit before retrying")
    } else if matches!(
        message.trim_end_matches('.'),
        "draft pull requests are not supported in this repository"
            | "draft pull requests are not supported"
            | "drafts are not supported"
            | "drafts not supported"
    ) {
        Some("draft pull requests are not supported; enable draft support before retrying")
    } else {
        None
    }
}
impl ValidationError {
    fn cause(&self) -> Option<&'static str> {
        match self {
            Self::Message(message) => message_cause(message),
            Self::Detail {
                resource,
                field,
                code,
                message,
            } => {
                if resource.as_deref() != Some("PullRequest") {
                    return None;
                }
                match code.as_deref() {
                    Some("missing" | "missing_field" | "invalid") => match field.as_deref() {
                        Some("head") => {
                            Some("head is missing or invalid; check the published head branch")
                        }
                        Some("base") => {
                            Some("base is missing or invalid; check the configured base branch")
                        }
                        Some("title") => {
                            Some("title is missing or invalid; supply a valid pull request title")
                        }
                        Some("body") => Some("body is invalid; correct the pull request body"),
                        Some("draft") => Some("draft is invalid; check repository draft support"),
                        _ => None,
                    },
                    Some("custom") => message.as_deref().and_then(message_cause),
                    _ => None,
                }
            }
        }
    }
}
pub(super) fn definite_rejection(bytes: &[u8]) -> Option<&'static str> {
    let rejection: Rejection = serde_json::from_slice(bytes).ok()?;
    if !rejection.message.eq_ignore_ascii_case("validation failed")
        && message_cause(&rejection.message).is_none()
    {
        return None;
    }
    if rejection.errors.is_empty() {
        return message_cause(&rejection.message);
    }
    let mut cause = None;
    for error in rejection.errors {
        cause = Some(error.cause()?);
    }
    cause
}
pub(super) fn created_number(bytes: &[u8]) -> Option<u64> {
    #[derive(Deserialize)]
    struct Created {
        number: u64,
    }
    let created: Created = serde_json::from_slice(bytes).ok()?;
    (created.number > 0).then_some(created.number)
}
pub(super) fn review_error(number: u64) -> anyhow::Error {
    anyhow::anyhow!(
        "GitHub PR #{number} requires human review: it failed identity validation and closing it was not confirmed; close it on GitHub or retry"
    )
}
