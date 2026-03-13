use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeExpr {
    Unknown,
    Scalar(String),
    Record(String),
    Array(Box<TypeExpr>),
    Option(Box<TypeExpr>),
    Union(Vec<TypeExpr>),
    Other(String),
}

impl TypeExpr {
    pub fn parse(input: &str) -> Self {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Self::Unknown;
        }

        if let Some(parts) = split_top_level(trimmed, '|') {
            return Self::Union(parts.into_iter().map(Self::parse).collect());
        }

        if let Some(inner) = unwrap_generic(trimmed, "record") {
            return Self::Record(inner.trim().to_string());
        }
        if let Some(inner) = unwrap_generic(trimmed, "array") {
            return Self::Array(Box::new(Self::parse(inner)));
        }
        if let Some(inner) = unwrap_generic(trimmed, "option") {
            return Self::Option(Box::new(Self::parse(inner)));
        }

        if trimmed
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | ':' | '$'))
        {
            return Self::Scalar(trimmed.to_string());
        }

        Self::Other(trimmed.to_string())
    }

    pub fn record_tables(&self) -> Vec<String> {
        match self {
            Self::Record(name) => vec![name.clone()],
            Self::Array(inner) | Self::Option(inner) => inner.record_tables(),
            Self::Union(parts) => parts.iter().flat_map(Self::record_tables).collect(),
            Self::Unknown | Self::Scalar(_) | Self::Other(_) => Vec::new(),
        }
    }
}

impl fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => write!(f, "unknown"),
            Self::Scalar(value) => write!(f, "{value}"),
            Self::Record(value) => write!(f, "record<{value}>"),
            Self::Array(inner) => write!(f, "array<{inner}>"),
            Self::Option(inner) => write!(f, "option<{inner}>"),
            Self::Union(parts) => {
                let joined = parts
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" | ");
                write!(f, "{joined}")
            }
            Self::Other(value) => write!(f, "{value}"),
        }
    }
}

fn unwrap_generic<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}<");
    if !input.starts_with(&prefix) || !input.ends_with('>') {
        return None;
    }

    Some(&input[prefix.len()..input.len() - 1])
}

fn split_top_level(input: &str, delimiter: char) -> Option<Vec<&str>> {
    let mut depth = 0i32;
    let mut last = 0usize;
    let mut parts = Vec::new();
    let mut saw_delimiter = false;

    for (index, ch) in input.char_indices() {
        match ch {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            _ if ch == delimiter && depth == 0 => {
                saw_delimiter = true;
                parts.push(input[last..index].trim());
                last = index + ch.len_utf8();
            }
            _ => {}
        }
    }

    if saw_delimiter {
        parts.push(input[last..].trim());
        Some(parts)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::TypeExpr;

    #[test]
    fn parses_nested_record_types() {
        let expr = TypeExpr::parse("option<array<record<person>>>");
        assert_eq!(expr.record_tables(), vec!["person".to_string()]);
    }
}
