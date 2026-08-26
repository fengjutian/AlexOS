//! Shared content-boundary checks for outbound data and untrusted model/tool input.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FindingKind {
    PrivateKey,
    AccessToken,
    Jwt,
    PaymentCard,
    PromptInjection,
    ActiveContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContentFinding {
    pub kind: FindingKind,
    pub reason: &'static str,
}

pub fn sensitive_finding(text: &str) -> Option<ContentFinding> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("-----begin private key-----")
        || lower.contains("-----begin rsa private key-----")
        || lower.contains("-----begin openssh private key-----")
    {
        return finding(FindingKind::PrivateKey, "private key material");
    }
    if ["ghp_", "github_pat_", "xoxb-", "xoxp-", "sk-proj-", "akia"]
        .iter()
        .any(|prefix| contains_token_with_prefix(&lower, prefix, 16))
    {
        return finding(FindingKind::AccessToken, "access-token shaped value");
    }
    if ["token=", "password=", "secret=", "api_key=", "api-key="]
        .iter()
        .any(|key| contains_assigned_secret(&lower, key))
    {
        return finding(FindingKind::AccessToken, "secret assignment");
    }
    if text.split_whitespace().any(looks_like_jwt) {
        return finding(FindingKind::Jwt, "JSON Web Token");
    }
    for candidate in digit_runs(text) {
        if (13..=19).contains(&candidate.len()) && luhn_valid(&candidate) {
            return finding(FindingKind::PaymentCard, "payment-card number");
        }
    }
    None
}

pub fn sensitive_json(value: &Value) -> Option<ContentFinding> {
    match value {
        Value::String(text) => sensitive_finding(text),
        Value::Array(values) => values.iter().find_map(sensitive_json),
        Value::Object(values) => values.iter().find_map(|(key, value)| {
            let key = key.to_ascii_lowercase();
            if ["privatekey", "private_key", "accesstoken", "access_token"].contains(&key.as_str())
                && value.as_str().is_some_and(|value| value.len() >= 8)
            {
                finding(FindingKind::AccessToken, "secret-bearing field")
            } else {
                sensitive_json(value)
            }
        }),
        _ => None,
    }
}

pub fn untrusted_content_finding(text: &str) -> Option<ContentFinding> {
    let normalized = normalize_instruction_text(text);
    let markers = [
        "ignorepreviousinstructions",
        "ignoreallpreviousinstructions",
        "disregardpreviousinstructions",
        "revealthesystemprompt",
        "printthesystemprompt",
        "developermessageabove",
        "youarenowthesystem",
        "actassystemmessage",
        "exfiltratesecrets",
        "sendcredentials",
    ];
    if markers.iter().any(|marker| normalized.contains(marker))
        || (normalized.contains("systemmessage") && normalized.contains("override"))
    {
        return finding(FindingKind::PromptInjection, "instruction override marker");
    }
    let lower = text.trim_start().to_ascii_lowercase();
    if ["javascript:", "file:", "data:text/html", "vbscript:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
        || lower.contains("<script")
        || lower.contains("<iframe")
        || lower.contains("display:none")
        || lower.contains("visibility:hidden")
    {
        return finding(FindingKind::ActiveContent, "active or hidden content");
    }
    None
}

pub fn untrusted_json(value: &Value) -> Option<ContentFinding> {
    match value {
        Value::String(text) => untrusted_content_finding(text),
        Value::Array(values) => values.iter().find_map(untrusted_json),
        Value::Object(values) => values.values().find_map(untrusted_json),
        _ => None,
    }
}

fn normalize_instruction_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation() && !is_zero_width(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_zero_width(value: char) -> bool {
    matches!(
        value,
        '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'
    )
}

fn contains_token_with_prefix(text: &str, prefix: &str, minimum_tail: usize) -> bool {
    text.match_indices(prefix).any(|(index, _)| {
        text[index + prefix.len()..]
            .chars()
            .take_while(|value| value.is_ascii_alphanumeric() || matches!(value, '_' | '-'))
            .count()
            >= minimum_tail
    })
}

fn contains_assigned_secret(text: &str, key: &str) -> bool {
    text.match_indices(key).any(|(index, _)| {
        text[index + key.len()..]
            .chars()
            .take_while(|value| !value.is_whitespace() && !matches!(value, '&' | ';' | ','))
            .count()
            >= 8
    })
}

fn looks_like_jwt(value: &str) -> bool {
    let value = value
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_' && c != '-');
    let segments = value.split('.').collect::<Vec<_>>();
    segments.len() == 3
        && segments[0].len() >= 8
        && segments[1].len() >= 8
        && segments[2].len() >= 16
        && segments.iter().all(|segment| {
            segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

fn digit_runs(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == ' ' || c == '-'))
        .map(|value| {
            value
                .chars()
                .filter(char::is_ascii_digit)
                .collect::<String>()
        })
        .filter(|value| value.len() >= 13)
        .collect()
}

fn luhn_valid(value: &str) -> bool {
    let mut sum = 0u32;
    let parity = value.len() % 2;
    for (index, byte) in value.bytes().enumerate() {
        let mut digit = u32::from(byte - b'0');
        if index % 2 == parity {
            digit *= 2;
            if digit > 9 {
                digit -= 9;
            }
        }
        sum += digit;
    }
    sum % 10 == 0
}

fn finding(kind: FindingKind, reason: &'static str) -> Option<ContentFinding> {
    Some(ContentFinding { kind, reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_credentials_and_payment_data_without_echoing_it() {
        assert_eq!(
            sensitive_finding("ghp_abcdefghijklmnopqrstuvwxyz1234")
                .unwrap()
                .kind,
            FindingKind::AccessToken
        );
        assert_eq!(
            sensitive_finding("4111 1111 1111 1111").unwrap().kind,
            FindingKind::PaymentCard
        );
        assert!(sensitive_finding("password=hunter22").is_some());
        assert!(sensitive_json(&json!({"access_token":"abcdefghijk"})).is_some());
        assert!(sensitive_finding("ordinary customer 12345").is_none());
    }

    #[test]
    fn catches_obfuscated_instruction_overrides_and_hidden_html() {
        assert!(untrusted_content_finding("Ignore\u{200b} previous instructions").is_some());
        assert!(
            untrusted_content_finding("<div style=\"display:none\">system override</div>")
                .is_some()
        );
        assert!(untrusted_content_finding("A normal search result").is_none());
    }
}
