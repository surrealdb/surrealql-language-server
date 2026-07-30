use std::fmt;

use serde::{Deserialize, Serialize};
use tree_sitter::Node;

use crate::semantic::node_kind as k;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeExpr {
    /// "We could not work this out." Distinct from the `any` scalar:
    /// `Unknown` means *absence of information*, and the type checker is
    /// required to stay silent whenever it appears on either side of a
    /// comparison. Never report against it.
    Unknown,
    Scalar(String),
    /// `record<person>` → `["person"]`; `record<a | b>` → `["a", "b"]`;
    /// a bare `record` (any table) → `[]`.
    Record(Vec<String>),
    Array(Box<TypeExpr>),
    Option(Box<TypeExpr>),
    Union(Vec<TypeExpr>),
    /// `set<T>`.
    Set(Box<TypeExpr>),
    /// An inline object type: `{ line: record<orderLine>, asset: record<asset> }`.
    /// Fields are kept in source order.
    Object(Vec<(String, TypeExpr)>),
    /// A fixed-arity positional array type: `[string, string]`.
    Tuple(Vec<TypeExpr>),
    /// A singleton literal type, stored as verbatim source text including
    /// any quotes — `'Started'`, `42`, `1h`. Verbatim so `Display` is
    /// byte-exact: `TYPE 'Started' | 'Not Started'` must hover unchanged.
    Literal(String),
    /// A type expression we recognised syntactically but cannot model.
    /// Treated exactly like [`Self::Unknown`] by the checker.
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
            // `record<a | b>` names two tables, not one called "a | b".
            // Splitting here matters: `record_tables` feeds the implicit
            // table registration in the analyzer, so getting it wrong
            // invents a phantom table with a pipe in its name.
            let tables = match split_top_level(inner, '|') {
                Some(parts) => parts
                    .into_iter()
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(ToOwned::to_owned)
                    .collect(),
                None => {
                    let single = inner.trim();
                    if single.is_empty() {
                        Vec::new()
                    } else {
                        vec![single.to_string()]
                    }
                }
            };
            return Self::Record(tables);
        }
        if let Some(inner) = unwrap_generic(trimmed, "array") {
            return Self::Array(Box::new(Self::parse(element_of(inner))));
        }
        // `set<T>` reaches this path from the generated builtin catalogue and
        // from any declared type that arrives as text rather than as a grammar
        // node. Without a case here it fell through to `Other`, which the
        // checker treats exactly like `Unknown` — so a `set` parameter or field
        // silently checked nothing.
        if let Some(inner) = unwrap_generic(trimmed, "set") {
            return Self::Set(Box::new(Self::parse(element_of(inner))));
        }
        if let Some(inner) = unwrap_generic(trimmed, "option") {
            return Self::Option(Box::new(Self::parse(inner)));
        }
        if trimmed.eq_ignore_ascii_case("record") {
            return Self::Record(Vec::new());
        }

        if trimmed
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | ':' | '$'))
        {
            return Self::Scalar(trimmed.to_string());
        }

        Self::Other(trimmed.to_string())
    }

    /// Build a type from the grammar's own type nodes rather than from
    /// source text.
    ///
    /// This is the path every declared type should take. The string
    /// [`Self::parse`] cannot express object or tuple types at all — it
    /// drops them into [`Self::Other`] — so round-tripping a node through
    /// its source text silently loses structure. Notably
    /// `$doc: { line: record<orderLine>, asset: record<asset> }` becomes
    /// an opaque blob, taking its `record<>` links with it.
    ///
    /// Falls back to [`Self::parse`] for kinds not covered here, so an
    /// unfamiliar node degrades to the old behaviour instead of vanishing.
    pub fn from_node(node: Node<'_>, source: &str) -> Self {
        let text = || {
            node.utf8_text(source.as_bytes())
                .ok()
                .map(str::trim)
                .unwrap_or_default()
        };

        match node.kind() {
            // `Type` is a thin wrapper around the real payload; `_safeType`
            // also admits a parenthesised `<...>` form.
            k::TYPE => named_children(node)
                .first()
                .map(|inner| Self::from_node(*inner, source))
                .unwrap_or_else(|| Self::parse(text())),

            k::TYPE_NAME => Self::parse(text()),

            // `record<person>`, `array<string>`, `option<T>`, `set<T>`.
            k::PARAMETERIZED_TYPE => Self::from_parameterized(node, source),

            k::UNION_TYPE => Self::union(
                named_children(node)
                    .into_iter()
                    .filter(|child| child.kind() != k::PIPE)
                    .map(|child| Self::from_node(child, source))
                    .collect(),
            ),

            // `LiteralType` wraps a String/Number/Duration literal, or an
            // ArrayType / ObjectType.
            k::LITERAL_TYPE => named_children(node)
                .first()
                .map(|inner| Self::from_node(*inner, source))
                .unwrap_or_else(|| Self::Literal(text().to_string())),

            k::ARRAY_TYPE => Self::Tuple(
                named_children(node)
                    .into_iter()
                    .map(|child| Self::from_node(child, source))
                    .collect(),
            ),

            k::OBJECT_TYPE => Self::Object(object_type_fields(node, source)),

            // A bare literal inside a `LiteralType`.
            k::STRING | k::NUMBER | k::DURATION | k::INT | k::FLOAT | k::DECIMAL => {
                Self::Literal(text().to_string())
            }

            _ => Self::parse(text()),
        }
    }

    /// `name<inner>` — `record`, `array`, `option`, `set`, or anything else.
    fn from_parameterized(node: Node<'_>, source: &str) -> Self {
        let children = named_children(node);
        // First child is the constructor name, the rest are arguments.
        let Some((head, args)) = children.split_first() else {
            return Self::Unknown;
        };
        let name = head
            .utf8_text(source.as_bytes())
            .ok()
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let args: Vec<Self> = args
            .iter()
            .filter(|child| child.kind() != k::PIPE)
            .map(|child| Self::from_node(*child, source))
            .collect();

        match name.as_str() {
            // `record<a | b>` may arrive as one `UnionType` argument or as
            // several arguments; flatten either into the table list.
            "record" => Self::Record(args.iter().flat_map(Self::type_names).collect()),
            "array" | "set" => {
                // `array<string, 5>` — the arity argument is not a type;
                // keep only the element type.
                let inner = args.into_iter().next().unwrap_or(Self::Unknown);
                if name == "set" {
                    Self::Set(Box::new(inner))
                } else {
                    Self::Array(Box::new(inner))
                }
            }
            "option" => Self::Option(Box::new(args.into_iter().next().unwrap_or(Self::Unknown))),
            _ => Self::Other(
                node.utf8_text(source.as_bytes())
                    .ok()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string(),
            ),
        }
    }

    /// Bare names inside this type, used to read table lists out of a
    /// `record<…>` argument.
    fn type_names(&self) -> Vec<String> {
        match self {
            Self::Scalar(name) => vec![name.clone()],
            Self::Union(parts) => parts.iter().flat_map(Self::type_names).collect(),
            Self::Record(names) => names.clone(),
            _ => Vec::new(),
        }
    }

    /// Build a union, normalising two things:
    ///
    /// * a single member is not a union;
    /// * a `none`/`null` member becomes an `option<…>` wrapper, so a local
    ///   `option<string>` and the `none | string` spelling that remote
    ///   `INFO FOR DB` returns compare equal.
    pub fn union(parts: Vec<Self>) -> Self {
        let (nullable, rest): (Vec<_>, Vec<_>) = parts.into_iter().partition(|part| {
            matches!(part, Self::Scalar(name)
                if name.eq_ignore_ascii_case("none") || name.eq_ignore_ascii_case("null"))
        });

        let inner = match rest.len() {
            0 => return nullable.into_iter().next().unwrap_or(Self::Unknown),
            1 => rest.into_iter().next().expect("checked len"),
            _ => Self::Union(rest),
        };

        if nullable.is_empty() {
            inner
        } else {
            Self::Option(Box::new(inner))
        }
    }

    pub fn record_tables(&self) -> Vec<String> {
        match self {
            Self::Record(names) => names.clone(),
            Self::Array(inner) | Self::Option(inner) | Self::Set(inner) => inner.record_tables(),
            Self::Union(parts) | Self::Tuple(parts) => {
                parts.iter().flat_map(Self::record_tables).collect()
            }
            Self::Object(fields) => fields
                .iter()
                .flat_map(|(_, value)| value.record_tables())
                .collect(),
            Self::Unknown | Self::Scalar(_) | Self::Literal(_) | Self::Other(_) => Vec::new(),
        }
    }
}

impl fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => write!(f, "unknown"),
            Self::Scalar(value) => write!(f, "{value}"),
            Self::Record(tables) if tables.is_empty() => write!(f, "record"),
            Self::Record(tables) => write!(f, "record<{}>", tables.join(" | ")),
            Self::Array(inner) => write!(f, "array<{inner}>"),
            Self::Set(inner) => write!(f, "set<{inner}>"),
            Self::Option(inner) => write!(f, "option<{inner}>"),
            Self::Literal(value) => write!(f, "{value}"),
            Self::Tuple(parts) => {
                let joined = parts
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "[{joined}]")
            }
            Self::Object(fields) => {
                let joined = fields
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "{{ {joined} }}")
            }
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

fn named_children<'tree>(node: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// Read `{ key: type, … }` out of an `ObjectType` node.
///
/// Grammar: `ObjectType(BraceOpen, ObjectTypeContent?, BraceClose)` and
/// `ObjectTypeProperty(ObjectKey, Colon, _type)`.
fn object_type_fields(node: Node<'_>, source: &str) -> Vec<(String, TypeExpr)> {
    let content = named_children(node)
        .into_iter()
        .find(|child| child.kind() == k::OBJECT_TYPE_CONTENT)
        .unwrap_or(node);

    named_children(content)
        .into_iter()
        .filter(|child| child.kind() == k::OBJECT_TYPE_PROPERTY)
        .filter_map(|property| {
            let children = named_children(property);
            let key = children
                .iter()
                .find(|child| matches!(child.kind(), k::OBJECT_KEY | k::KEY_NAME | k::STRING))
                .and_then(|child| child.utf8_text(source.as_bytes()).ok())
                .map(|text| text.trim().trim_matches(['"', '\'', '`']).to_string())?;
            let value = children
                .iter()
                .find(|child| k::TYPE_KINDS.contains(&child.kind()))
                .map(|child| TypeExpr::from_node(*child, source))
                .unwrap_or(TypeExpr::Unknown);
            Some((key, value))
        })
        .collect()
}

/// The element type of an `array<…>` or `set<…>` argument list.
///
/// `array<string, 5>` declares a fixed length, and the arity is not a type —
/// the same reason [`TypeExpr::from_parameterized`] keeps only the first
/// argument. Without this, the trailing `, 5` made the whole thing an `Other`
/// and silenced the element check.
fn element_of(inner: &str) -> &str {
    match split_top_level(inner, ',') {
        Some(parts) => parts.first().copied().unwrap_or(inner).trim(),
        None => inner.trim(),
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
    fn parses_set_from_a_string() {
        // The string path had no `set<>` case, so this degraded to `Other`,
        // which the checker treats as unknown — a `set` field or parameter
        // silently checked nothing.
        assert_eq!(
            TypeExpr::parse("set<string>"),
            TypeExpr::Set(Box::new(TypeExpr::Scalar("string".to_string())))
        );
        assert_eq!(TypeExpr::parse("set<string>").to_string(), "set<string>");
    }

    #[test]
    fn parses_set_of_records_and_keeps_the_link() {
        let expr = TypeExpr::parse("set<record<person>>");
        assert_eq!(expr.record_tables(), vec!["person".to_string()]);
    }

    #[test]
    fn drops_the_arity_argument_of_a_sized_collection() {
        // `array<string, 5>` declares a length, and a length is not a type. The
        // whole thing used to become an `Other`, silencing the element check.
        assert_eq!(
            TypeExpr::parse("array<string, 5>"),
            TypeExpr::Array(Box::new(TypeExpr::Scalar("string".to_string())))
        );
        assert_eq!(
            TypeExpr::parse("set<int, 3>"),
            TypeExpr::Set(Box::new(TypeExpr::Scalar("int".to_string())))
        );
    }

    #[test]
    fn parses_nested_record_types() {
        let expr = TypeExpr::parse("option<array<record<person>>>");
        assert_eq!(expr.record_tables(), vec!["person".to_string()]);
    }

    #[test]
    fn parses_record_union_as_multiple_tables() {
        let expr = TypeExpr::parse("record<orderData | project>");

        assert_eq!(
            expr.record_tables(),
            vec!["orderData".to_string(), "project".to_string()]
        );
        assert_eq!(expr.to_string(), "record<orderData | project>");
    }

    #[test]
    fn parses_bare_record_as_any_table() {
        let expr = TypeExpr::parse("record");

        assert!(expr.record_tables().is_empty());
        assert_eq!(expr.to_string(), "record");
    }

    #[test]
    fn round_trips_record_display() {
        for input in ["record<person>", "option<record<person>>", "array<string>"] {
            assert_eq!(TypeExpr::parse(input).to_string(), input);
        }
    }
}
