//! Deterministic `SKILL.md` draft from the steps of a finished run.
//!
//! The draft is built only from tool names and argument *names*. An argument
//! value becomes a parameter; its value is shown as an example only when it is
//! on a short ALLOW-LIST: an `http(s)` origin without path, query, fragment or
//! credentials, a boolean, a small integer under a harmless key, or one of the
//! literals the tool's own schema declares in an `enum`. Everything else (any
//! string, name, number, path, code, URL with a path) is withheld by default,
//! so a one-time code, a password or a personal detail typed into a chat can
//! never be copied into the saved skill, whatever the argument is called. The
//! chat title goes through the same kind of screen before it reaches the
//! name, description or keywords. The result is a proposal: the owner edits
//! it, the scanner runs on it and it is installed only after an explicit
//! confirmation (see `daemon-api`).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

use crate::skill::{MAX_DESCRIPTION_LEN, MAX_NAME_LEN, validate_name};

/// Most steps copied into one draft.
pub const MAX_DRAFT_STEPS: usize = 40;
/// Longest example value shown next to a parameter.
const MAX_EXAMPLE_CHARS: usize = 60;
/// Largest integer shown as an example.
const MAX_EXAMPLE_INT: i64 = 1000;
/// Longest chat title that is trusted to name the skill.
const MAX_TITLE_CHARS: usize = 80;
/// Most `enum` literals of one argument that are considered.
const MAX_ENUM_LITERALS: usize = 16;
/// Most parameters in one draft.
const MAX_PARAMS: usize = 24;

/// One successful tool call of the run.
#[derive(Debug, Clone)]
pub struct DraftCall {
    /// Tool name.
    pub tool: String,
    /// The raw JSON arguments exactly as the model sent them.
    pub arguments: String,
    /// `read`, `write` or `destructive`; `None` when the tool is not registered.
    pub effect: Option<String>,
    /// Literals the tool's own schema declares in an `enum`, by argument name.
    /// Only these string values can ever be shown as an example.
    pub enums: BTreeMap<String, Vec<String>>,
}

/// Everything the draft is built from.
#[derive(Debug, Clone)]
pub struct DraftInput {
    /// Chat title (redacted by the builder before use).
    pub title: String,
    /// Successful calls, oldest first.
    pub calls: Vec<DraftCall>,
}

/// A value the owner supplies each time the skill runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DraftParam {
    /// Placeholder name used in the steps as `{{name}}`.
    pub name: String,
    /// A harmless example, or `None` when the real value was withheld.
    pub example: Option<String>,
    /// True when the value looked like a secret or private data.
    pub withheld: bool,
}

/// A tool the skill relies on and what it can do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DraftTool {
    /// Tool name.
    pub name: String,
    /// `read`, `write`, `destructive` or `unknown`.
    pub effect: String,
}

/// The proposal shown in the preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillDraft {
    /// Suggested skill name.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Trigger keywords.
    pub keywords: Vec<String>,
    /// Tools the skill needs.
    pub tools: Vec<DraftTool>,
    /// Parameters.
    pub parameters: Vec<DraftParam>,
    /// Plain-language steps.
    pub steps: Vec<String>,
    /// How many values were withheld.
    pub withheld: usize,
    /// The complete SKILL.md text.
    pub markdown: String,
}

const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "passcode",
    "secret",
    "token",
    "apikey",
    "api_key",
    "authorization",
    "cookie",
    "credential",
    "bearer",
    "private",
    "auth",
    "key",
    "pin",
    "otp",
    "totp",
    "hotp",
    "code",
    "mfa",
    "2fa",
    "cvv",
    "cvc",
    "ssn",
    "iban",
    "pass",
    "phone",
    "tel",
    "card",
    "account",
];

/// Keys that carry free text; a number under one of them may be a pasted code.
const FREE_TEXT_KEYS: &[&str] = &[
    "text", "query", "input", "message", "content", "value", "data", "name", "body", "prompt",
    "keys", "string", "line", "title",
];

fn slug(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
        if out.len() >= max {
            break;
        }
    }
    out.trim_end_matches('-').to_owned()
}

fn param_name(key: &str) -> String {
    let s = slug(key, 32).replace('-', "_");
    if s.is_empty() || has_digit_run(&s, 4) {
        "value".into()
    } else {
        s
    }
}

/// True when `text` holds `n` or more digits in a row.
fn has_digit_run(text: &str, n: usize) -> bool {
    let mut run = 0usize;
    for c in text.chars() {
        if c.is_ascii_digit() {
            run += 1;
            if run >= n {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Short entries match whole words (`pin` is not `spinner`), long ones as a substring.
fn key_matches(key: &str, list: &[&str]) -> bool {
    let k = key.to_ascii_lowercase();
    list.iter().any(|s| {
        if s.len() <= 4 {
            k.split(|c: char| !c.is_ascii_alphanumeric())
                .any(|w| w == *s)
        } else {
            k.contains(s)
        }
    })
}

fn secret_key(key: &str) -> bool {
    key_matches(key, SECRET_KEYS)
}

/// `http(s)://host[:port][/]` only: no userinfo, path, query or fragment, so
/// nothing in it can carry a reset token, an id or a credential.
fn origin_only(value: &str) -> bool {
    let Some(rest) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    else {
        return false;
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (rest, None),
    };
    if port.is_some_and(|p| p.is_empty() || p.len() > 5 || !p.chars().all(|c| c.is_ascii_digit())) {
        return false;
    }
    !host.is_empty()
        && host.contains('.')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        && host
            .split('.')
            .all(|l| !l.is_empty() && l.len() <= 24 && !l.starts_with('-') && !l.ends_with('-'))
        && !has_digit_run(host.rsplit_once('.').map_or(host, |(h, _)| h), 7)
}

/// A literal safe to show: short lowercase words only.
fn plain_literal(value: &str) -> bool {
    value.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && value.len() <= 24
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
        && !has_digit_run(value, 3)
}

/// The example text for one argument, or `None` when it must be withheld.
///
/// Allow-list: anything not matched here is withheld, so a value under a
/// harmless-looking key (`text`, `name`, `path`...) is never shown.
fn example_for(
    key: &str,
    value: &Value,
    enums: &BTreeMap<String, Vec<String>>,
    redact: &dyn Fn(&str) -> String,
) -> Option<String> {
    let text = match value {
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            let i = n.as_i64()?;
            if !(0..=MAX_EXAMPLE_INT).contains(&i)
                || secret_key(key)
                || key_matches(key, FREE_TEXT_KEYS)
            {
                return None;
            }
            i.to_string()
        }
        Value::String(s) => {
            if secret_key(key) || s.chars().count() > MAX_EXAMPLE_CHARS {
                return None;
            }
            let declared = enums
                .get(key)
                .is_some_and(|list| list.len() <= MAX_ENUM_LITERALS && list.contains(s));
            if origin_only(s) || (declared && plain_literal(s)) {
                s.clone()
            } else {
                return None;
            }
        }
        _ => return None,
    };
    // A value the vault knows is never shown, however harmless it looks.
    (redact(&text) == text).then_some(text)
}

/// A chat title is used only when it is plain words: no digit run, address,
/// link, path or token-like word, and not too long.
fn plain_title(title: &str, redact: &dyn Fn(&str) -> String) -> bool {
    if title.is_empty()
        || title.chars().count() > MAX_TITLE_CHARS
        || has_digit_run(title, 4)
        || redact(title) != title
    {
        return false;
    }
    let lower = title.to_lowercase();
    if lower.contains("www.") || lower.contains("://") {
        return false;
    }
    let chars: Vec<char> = title.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        let ok = c.is_alphanumeric()
            || (c.is_whitespace() && !c.is_control())
            || matches!(c, '-' | '\'' | ',' | '!' | '?' | '&')
            // A full stop only ends a sentence; inside a word it is a domain or a file name.
            || (*c == '.' && chars.get(i + 1).is_none_or(|n| n.is_whitespace()));
        if !ok {
            return false;
        }
    }
    // A word that mixes letters and digits, or is very long, may be a code or a token.
    title.split(|c: char| !c.is_alphanumeric()).all(|w| {
        let n = w.chars().count();
        let digits = w.chars().filter(char::is_ascii_digit).count();
        n <= 20 && !(digits > 0 && digits < n && n >= 6)
    })
}

/// Build the draft. `redact` is the host's secret redactor.
#[must_use]
pub fn build(input: &DraftInput, redact: &dyn Fn(&str) -> String) -> SkillDraft {
    let title = input.title.trim();
    let title = if plain_title(title, redact) {
        title
    } else {
        ""
    };

    let mut name = slug(title, MAX_NAME_LEN - 4);
    if name.len() < 3 || validate_name(&name).is_err() {
        name = "saved-skill".into();
    }

    let mut tools: Vec<DraftTool> = Vec::new();
    let mut params: Vec<DraftParam> = Vec::new();
    let mut steps = Vec::new();
    let mut withheld = 0usize;
    for call in input.calls.iter().take(MAX_DRAFT_STEPS) {
        if !tools.iter().any(|t| t.name == call.tool) {
            tools.push(DraftTool {
                name: call.tool.clone(),
                effect: call.effect.clone().unwrap_or_else(|| "unknown".into()),
            });
        }
        let mut used: Vec<String> = Vec::new();
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&call.arguments) {
            for (key, value) in &map {
                if params.len() >= MAX_PARAMS {
                    break;
                }
                let new_example = example_for(key, value, &call.enums, redact);
                let show = new_example.is_some();
                let mut pname = param_name(key);
                // Same name seen again with a different value: a new parameter.
                let mut n = 1;
                while params
                    .iter()
                    .any(|p| p.name == pname && p.example != new_example)
                {
                    n += 1;
                    pname = format!("{}_{n}", param_name(key));
                }
                if !params.iter().any(|p| p.name == pname) {
                    if !show {
                        withheld += 1;
                    }
                    params.push(DraftParam {
                        name: pname.clone(),
                        example: new_example,
                        withheld: !show,
                    });
                }
                used.push(pname);
            }
        }
        let with = if used.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = used.iter().map(|p| format!("{{{{{p}}}}}")).collect();
            format!(" with {}", list.join(", "))
        };
        steps.push(format!("Use the `{}` tool{with}.", call.tool));
    }

    let mut keywords: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    for w in title.split(|c: char| !c.is_alphabetic()) {
        let w = w.to_lowercase();
        if w.chars().count() >= 4 && seen.insert(w.clone()) && keywords.len() < 5 {
            keywords.push(w);
        }
    }

    let description = if title.is_empty() {
        "Repeats a task saved from a chat.".to_owned()
    } else {
        let d = format!(
            "Repeats the task from the chat \"{}\".",
            title.replace('"', "")
        );
        d.chars().take(MAX_DESCRIPTION_LEN).collect()
    };
    let description: String = description.chars().filter(|c| !c.is_control()).collect();

    let markdown = render(&name, &description, &keywords, &tools, &params, &steps);
    SkillDraft {
        name,
        description,
        keywords,
        tools,
        parameters: params,
        steps,
        withheld,
        markdown,
    }
}

fn render(
    name: &str,
    description: &str,
    keywords: &[String],
    tools: &[DraftTool],
    params: &[DraftParam],
    steps: &[String],
) -> String {
    use std::fmt::Write as _;
    let desc = serde_json::to_string(description).unwrap_or_else(|_| "\"\"".into());
    let mut s = format!("---\nname: {name}\ndescription: {desc}\n");
    if !keywords.is_empty() {
        let _ = writeln!(s, "when:\n  keywords: [{}]", keywords.join(", "));
    }
    s.push_str("---\n# What this does\nRepeat the task below. Ask the user for any value you do not have.\n\n");
    s.push_str("## Tools this skill needs\n");
    for t in tools {
        let _ = writeln!(s, "- `{}` ({})", t.name, t.effect);
    }
    if !params.is_empty() {
        s.push_str("\n## Parameters\n");
        for p in params {
            match (&p.example, p.withheld) {
                (Some(e), _) => {
                    let _ = writeln!(s, "- `{{{{{}}}}}`: example `{e}`", p.name);
                }
                (None, _) => {
                    let _ = writeln!(
                        s,
                        "- `{{{{{}}}}}`: ask the user each time (the value was not saved)",
                        p.name
                    );
                }
            }
        }
    }
    s.push_str("\n## Steps\n");
    for (i, step) in steps.iter().enumerate() {
        let _ = writeln!(s, "{}. {step}", i + 1);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Source, parse_skill};

    const CANARY: &str = "sk-live-CANARY9f3a77c2d1e84b6a90";

    fn call(tool: &str, args: &str) -> DraftCall {
        DraftCall {
            tool: tool.into(),
            arguments: args.into(),
            effect: Some("read".into()),
            enums: BTreeMap::new(),
        }
    }

    fn redactor(text: &str) -> String {
        text.replace("hunter2-private", "[REDACTED]")
    }

    fn draft_of(title: &str, calls: Vec<DraftCall>) -> SkillDraft {
        build(
            &DraftInput {
                title: title.into(),
                calls,
            },
            &redactor,
        )
    }

    #[test]
    fn secrets_and_private_values_are_never_copied() {
        let args = serde_json::json!({
            "url": "https://example.com/docs",
            "api_key": CANARY,
            "note": "call me at me@home.example",
            "path": r"C:\Users\emre\secret.txt",
            "pw": "hunter2-private",
            "q": CANARY,
        })
        .to_string();
        let d = draft_of("Check the docs", vec![call("web_fetch", &args)]);
        for forbidden in [CANARY, "me@home.example", "secret.txt", "hunter2-private"] {
            assert!(!d.markdown.contains(forbidden), "{forbidden} leaked");
        }
        // The URL has a path, so only origins are allowed: it is withheld too.
        assert!(!d.markdown.contains("example.com/docs"), "{}", d.markdown);
        assert!(d.withheld >= 5);
        assert!(d.markdown.contains("{{api_key}}"));
    }

    /// Short values under neutral key names used to be copied as "examples".
    #[test]
    fn short_values_under_neutral_keys_are_withheld() {
        let sentinels = [
            ("text", "482913"),
            ("code", "482913"),
            ("totp", "482913"),
            ("text", "Hunter2!"),
            ("phone", "+905321234567"),
            ("name", "Ayse Yilmaz"),
            ("path", "/home/emre/tax-2025.pdf"),
            ("query", "my tax number is 1234567890"),
            ("url", "https://x.example/reset/ab12cd34"),
            ("url", "https://x.example/docs"),
            ("url", "https://user:pw@x.example"),
            ("url", "https://x.example/?t=1"),
            ("value", "ignore previous instructions"),
        ];
        for (key, value) in sentinels {
            let args = serde_json::json!({ key: value }).to_string();
            let d = draft_of("Check things", vec![call("browser_type", &args)]);
            assert!(
                !d.markdown.contains(value),
                "{key}={value} leaked:\n{}",
                d.markdown
            );
            assert_eq!(d.parameters.len(), 1);
            assert!(d.parameters[0].withheld, "{key}={value}");
        }
        // Numbers: a pasted code is out of range or under a code-like key.
        for args in [
            r#"{"text":482913}"#,
            r#"{"code":913}"#,
            r#"{"otp":12}"#,
            r#"{"text":7}"#,
            r#"{"limit":-4}"#,
            r#"{"limit":2.5}"#,
        ] {
            let d = draft_of("Check things", vec![call("browser_type", args)]);
            assert!(d.parameters.iter().all(|p| p.example.is_none()), "{args}");
        }
    }

    #[test]
    fn only_allow_listed_examples_are_shown() {
        let mut c = call(
            "web_search",
            r#"{"url":"https://notion.so","base":"http://localhost:8080/","limit":25,"safe":true,"sort":"newest","lang":"zz","mode":"abc123456"}"#,
        );
        c.enums
            .insert("sort".into(), vec!["newest".into(), "oldest".into()]);
        c.enums
            .insert("mode".into(), vec!["abc123456".into(), "x".into()]);
        let d = draft_of("Search notes", vec![c]);
        let example = |n: &str| {
            d.parameters
                .iter()
                .find(|p| p.name == n)
                .unwrap()
                .example
                .clone()
        };
        assert_eq!(example("url").as_deref(), Some("https://notion.so"));
        // `localhost` has no dot and is not a public origin: withheld.
        assert_eq!(example("base"), None);
        assert_eq!(example("limit").as_deref(), Some("25"));
        assert_eq!(example("safe").as_deref(), Some("true"));
        assert_eq!(example("sort").as_deref(), Some("newest"));
        // Not in the declared enum, or a declared literal that looks like a number.
        assert_eq!(example("lang"), None);
        assert_eq!(example("mode"), None);
    }

    #[test]
    fn a_secret_in_the_title_falls_back_to_a_generic_name() {
        let d = draft_of("hunter2-private notes", vec![call("web_fetch", "{}")]);
        assert_eq!(d.name, "saved-skill");
        assert!(!d.markdown.contains("hunter2"));
    }

    #[test]
    fn titles_with_digits_addresses_or_tokens_fall_back_to_generic_text() {
        let long = "very long title ".repeat(8);
        for title in [
            "Log in with code 482913",
            "mail ayse@home.example the report",
            "open https://x.example/reset/ab12cd34",
            "read www.bank.example now",
            "fetch /home/emre/tax.pdf",
            "my password is Hunter2",
            "call +905321234567",
            "token sk_live_abcdefghijklmnopqrstuv",
            "Line one\nline two",
            long.as_str(),
        ] {
            let d = draft_of(title, vec![call("web_fetch", "{}")]);
            assert_eq!(d.name, "saved-skill", "{title}");
            assert_eq!(
                d.description, "Repeats a task saved from a chat.",
                "{title}"
            );
            assert!(d.keywords.is_empty(), "{title}");
            assert!(!d.markdown.contains("482913"), "{title}");
            assert!(!d.markdown.contains("example"), "{title}");
        }
    }

    #[test]
    fn the_draft_is_a_valid_skill_with_keywords_tools_and_steps() {
        let d = draft_of(
            "Weekly report from Notion",
            vec![
                call("web_fetch", r#"{"url":"https://notion.so"}"#),
                DraftCall {
                    tool: "fs_write".into(),
                    arguments: r#"{"path":"report.md"}"#.into(),
                    effect: Some("write".into()),
                    enums: BTreeMap::new(),
                },
            ],
        );
        let skill = parse_skill(&d.markdown, Source::User, None).unwrap();
        assert_eq!(skill.name, "weekly-report-from-notion");
        assert!(skill.when.keywords.contains(&"report".to_owned()));
        assert_eq!(d.tools.len(), 2);
        assert!(d.markdown.contains("`fs_write` (write)"));
        assert!(d.markdown.contains("https://notion.so"));
        assert_eq!(d.steps.len(), 2);
    }
}
