//! Secret redaction, applied to every string and object key in a payload
//! before it is hashed and stored. Pattern-based: it catches the shapes listed
//! here, not every secret. Unknown formats pass through.
use serde_json::{Map, Value};

pub const REDACTED: &str = "[REDACTED]";

/// A token is a known prefix, not preceded by an ASCII letter or digit (so
/// `KEY=`, `--x=`, `/` and `?a=` all count as a start), followed by at least
/// MIN_BODY token characters. `bearer ` matches in any case.
const PREFIXES: [&str; 21] = [
    "sk-ant-",
    "sk-",
    "sk_live_",
    "rk_live_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxr-",
    "xoxs-",
    "AKIA",
    "ASIA",
    "AIza",
    "eyJ",
    "bearer ",
];
const MIN_BODY: usize = 16;
/// A name containing one of these, ignoring case, `_` and `-`, marks its
/// value as secret: `name=value` and `name: value` in env dumps, config files,
/// headers and query strings, `--name value` flags, and `"name": "value"` in
/// JSON payloads. It errs towards redacting (`bypass: x` goes too).
const SECRET_NAMES: [&str; 8] = [
    "SECRET",
    "TOKEN",
    "PASS",
    "PWD",
    "AUTH",
    "APIKEY",
    "ACCESSKEY",
    "PRIVATEKEY",
];

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-_./+=".contains(&b)
}

fn is_secret_name(name: &str) -> bool {
    let name: String = name
        .chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .collect::<String>()
        .to_ascii_uppercase();
    // The working directory and authors are not secrets; Authorization is.
    if matches!(name.as_str(), "PWD" | "OLDPWD") {
        return false;
    }
    let name = name.replace("AUTHORIZATION", "AUTH").replace("AUTHOR", "");
    SECRET_NAMES.iter().any(|s| name.contains(s))
}

/// Redacts every string and object key in `v`, recursively. A string under a
/// secret-sounding key is replaced whole. Two keys that redact to the same
/// text are kept apart with a `#n` suffix.
pub fn redact_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(redact(s)),
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        Value::Object(o) => {
            let mut out = Map::new();
            for (k, v) in o {
                let base = redact(k);
                let mut key = base.clone();
                let mut n = 1;
                while out.contains_key(&key) {
                    n += 1;
                    key = format!("{base}#{n}");
                }
                let v = match v {
                    Value::String(_) if is_secret_name(k) => Value::String(REDACTED.into()),
                    v => redact_value(v),
                };
                out.insert(key, v);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// The one redaction function: PEM private keys, then known tokens, secret
/// assignments, secret flags and URL passwords.
pub fn redact(text: &str) -> String {
    redact_tokens(&redact_pem(text))
}

/// PEM private key blocks (any label containing PRIVATE KEY), from BEGIN to
/// the matching END. A block with no END
/// is redacted to the end of the text.
fn redact_pem(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("-----BEGIN ") {
        let after = &rest[start + 11..];
        let Some(label_len) = after.find("-----") else {
            break;
        };
        let label = &after[..label_len];
        if !label.contains("PRIVATE KEY") {
            out.push_str(&rest[..start + 11]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..start]);
        out.push_str(REDACTED);
        let end = format!("-----END {label}-----");
        rest = match after.find(&end) {
            Some(i) => &after[i + end.len()..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

fn redact_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let secret_end = token_at(text, i)
            .or_else(|| assigned_value_at(text, i))
            .or_else(|| flag_value_at(text, i))
            .or_else(|| url_password_at(text, i));
        match secret_end {
            Some(end) => {
                out.push_str(REDACTED);
                i = end;
            }
            None => {
                let c = text[i..].chars().next().unwrap();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// End of a known-prefix token starting at `i`.
fn token_at(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
        return None;
    }
    PREFIXES.iter().find_map(|p| {
        let head = bytes.get(i..i + p.len())?;
        let hit = if *p == "bearer " {
            head.eq_ignore_ascii_case(p.as_bytes())
        } else {
            head == p.as_bytes()
        };
        if !hit {
            return None;
        }
        let body = bytes[i + p.len()..]
            .iter()
            .take_while(|b| is_token_byte(**b))
            .count();
        (body >= MIN_BODY).then_some(i + p.len() + body)
    })
}

/// End of a value starting at `i`: a quoted value with its quotes, otherwise
/// up to whitespace or `"'&;,`, or a closing `}])` that ends the token.
fn value_end(text: &str, i: usize) -> usize {
    let bytes = text.as_bytes();
    match bytes[i] {
        q @ (b'"' | b'\'') => text[i + 1..]
            .find(q as char)
            .map_or(text.len(), |e| i + e + 2),
        _ => {
            let stop = |b: u8| b.is_ascii_whitespace() || b"\"'&;,".contains(&b);
            let mut j = i;
            while j < bytes.len() && !stop(bytes[j]) {
                // A closing bracket ends the value only when it ends the token.
                if b"}])".contains(&bytes[j])
                    && bytes
                        .get(j + 1)
                        .is_none_or(|n| stop(*n) || b"}])".contains(n))
                {
                    break;
                }
                j += 1;
            }
            j
        }
    }
}

/// End of the value of `name = value` or `name: value` starting at `i`, when
/// the name (optionally quoted) is secret-sounding. `Authorization: Basic x`
/// takes the scheme and the credentials. An all-digit value
/// (`max_tokens=4096`) is kept.
fn assigned_value_at(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let blank = |b: u8| b == b' ' || b == b'\t';
    if bytes[i].is_ascii_whitespace() {
        return None;
    }
    let mut j = i;
    while j > 0 && blank(bytes[j - 1]) {
        j -= 1;
    }
    if j == 0 || !b":=".contains(&bytes[j - 1]) {
        return None;
    }
    j -= 1;
    while j > 0 && blank(bytes[j - 1]) {
        j -= 1;
    }
    if j > 0 && b"\"'".contains(&bytes[j - 1]) {
        j -= 1;
    }
    let name_start = bytes[..j]
        .iter()
        .rposition(|b| !(b.is_ascii_alphanumeric() || b"_-".contains(b)))
        .map_or(0, |p| p + 1);
    if !is_secret_name(&text[name_start..j]) {
        return None;
    }
    let mut end = value_end(text, i);
    let scheme = text[i..end].to_ascii_lowercase();
    if matches!(scheme.as_str(), "basic" | "bearer" | "token" | "digest")
        && bytes.get(end) == Some(&b' ')
        && bytes.get(end + 1).is_some_and(|b| !b.is_ascii_whitespace())
    {
        end = value_end(text, end + 1);
    }
    let digits = bytes[i..end].iter().all(u8::is_ascii_digit);
    (end > i && !digits).then_some(end)
}

/// End of the value of `--name value` (a secret-sounding flag) or
/// `-u user:password` starting at `i`.
fn flag_value_at(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if i < 2 || bytes[i - 1] != b' ' || bytes[i] == b'-' || bytes[i].is_ascii_whitespace() {
        return None;
    }
    let flag_start = bytes[..i - 1]
        .iter()
        .rposition(u8::is_ascii_whitespace)
        .map_or(0, |p| p + 1);
    let flag = &text[flag_start..i - 1];
    let end = value_end(text, i);
    let hit = match flag {
        "-u" | "--user" => text[i..end].contains(':'),
        f => f.starts_with("--") && is_secret_name(f),
    };
    hit.then_some(end)
}

/// End of the password in `scheme://user:password@host` starting at `i`, just
/// after the `:`.
fn url_password_at(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if i == 0 || bytes[i - 1] != b':' {
        return None;
    }
    let user_len = bytes[..i - 1]
        .iter()
        .rev()
        .take_while(|b| !b"/@:".contains(b) && !b.is_ascii_whitespace())
        .count();
    if !text[..i - 1 - user_len].ends_with("://") {
        return None;
    }
    let len = bytes[i..]
        .iter()
        .take_while(|b| !b"/@".contains(b) && !b.is_ascii_whitespace())
        .count();
    (len > 0 && bytes.get(i + len) == Some(&b'@')).then_some(i + len)
}
