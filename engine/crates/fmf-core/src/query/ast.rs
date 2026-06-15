//! Tokenizer and parser: query text → [`Ast`] (OR of AND groups).

use thiserror::Error;

use super::dates::Civil;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("unclosed quote")]
    UnclosedQuote,
    #[error("invalid size filter `{0}`")]
    InvalidSize(String),
    #[error("invalid date filter `{0}`")]
    InvalidDate(String),
    #[error("`{0}` cannot be negated")]
    CannotNegate(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    /// Substring in the file name.
    Name(String),
    /// Substring in the full path (term contained `\` or used `path:`).
    Path(String),
    /// `*`/`?` pattern matching the whole file name.
    Wildcard(String),
    /// `*`/`?` pattern matched unanchored against the full path.
    PathWildcard(String),
    /// `regex:` — applied to the file name.
    Regex(String),
    /// `ext:` — any of these extensions (without dots, original case).
    Ext(Vec<String>),
    /// `size:` — inclusive byte range.
    Size {
        min: u64,
        max: u64,
    },
    /// `dm:` — [start, end) at local midnight; `None` = unbounded.
    Mtime {
        start: Option<Civil>,
        end: Option<Civil>,
    },
    /// `folder:` / `file:`.
    IsDir(bool),
    Not(Box<Self>),
}

/// OR of AND groups: `a b | c` → `[[a, b], [c]]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Ast {
    pub groups: Vec<Vec<Term>>,
}

/// Tokenize and parse query text into an [`Ast`].
///
/// # Errors
///
/// Returns a [`ParseError`] for malformed input: an unclosed quote, or an
/// invalid `size:`/date filter, or a term that cannot be negated.
///
/// # Panics
///
/// Does not panic: `groups` is seeded with one group and only grows, so the
/// `last_mut` access always succeeds.
pub fn parse(input: &str) -> Result<Ast, ParseError> {
    let mut groups: Vec<Vec<Term>> = vec![Vec::new()];
    let mut rest = input;

    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if let Some(r) = rest.strip_prefix('|') {
            groups.push(Vec::new());
            rest = r;
            continue;
        }

        let mut negated = false;
        while let Some(r) = rest.strip_prefix('!') {
            negated = !negated;
            rest = r;
        }

        let (atom, r) = read_atom(rest)?;
        rest = r;
        if atom.is_empty() {
            continue;
        }
        let terms = terms_from_atom(&atom, negated)?;
        for t in terms {
            groups.last_mut().unwrap().push(t);
        }
    }

    Ok(Ast { groups })
}

/// Read one atom: up to whitespace or `|`, honoring quoted sections
/// (`"two words"`, `path:"Program Files"`).
fn read_atom(input: &str) -> Result<(String, &str), ParseError> {
    let mut out = String::new();
    let mut it = input.char_indices();
    let mut in_quotes = false;
    loop {
        let Some((i, c)) = it.next() else {
            if in_quotes {
                return Err(ParseError::UnclosedQuote);
            }
            return Ok((out, ""));
        };
        match c {
            '"' => {
                in_quotes = !in_quotes;
                out.push(c);
            }
            c if !in_quotes && (c.is_whitespace() || c == '|') => {
                return Ok((out, &input[i..]));
            }
            c => out.push(c),
        }
    }
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
}

const FIELDS: &[&str] = &["ext", "path", "size", "dm", "regex", "file", "folder"];

fn terms_from_atom(atom: &str, negated: bool) -> Result<Vec<Term>, ParseError> {
    let wrap = |t: Term| if negated { Term::Not(Box::new(t)) } else { t };

    // field:value?
    if !atom.starts_with('"')
        && let Some(colon) = atom.find(':')
    {
        let field = &atom[..colon];
        let raw_value = &atom[colon + 1..];
        if FIELDS.contains(&field.to_ascii_lowercase().as_str()) {
            let value = unquote(raw_value);
            return match field.to_ascii_lowercase().as_str() {
                "ext" => {
                    let exts: Vec<String> = value
                        .split([';', ','])
                        .map(|e| e.trim().trim_start_matches('.').to_string())
                        .filter(|e| !e.is_empty())
                        .collect();
                    Ok(vec![wrap(Term::Ext(exts))])
                }
                "path" => Ok(vec![wrap(if has_wildcard(value) {
                    Term::PathWildcard(value.to_string())
                } else {
                    Term::Path(value.to_string())
                })]),
                "size" => {
                    let (min, max) = parse_size_range(value)
                        .ok_or_else(|| ParseError::InvalidSize(value.to_string()))?;
                    Ok(vec![wrap(Term::Size { min, max })])
                }
                "dm" => {
                    let (start, end) = parse_date_range(value)
                        .ok_or_else(|| ParseError::InvalidDate(value.to_string()))?;
                    Ok(vec![wrap(Term::Mtime { start, end })])
                }
                // The regex value is the raw remainder (may itself contain ':').
                "regex" => Ok(vec![wrap(Term::Regex(unquote(raw_value).to_string()))]),
                "file" | "folder" => {
                    let is_dir = field.eq_ignore_ascii_case("folder");
                    if value.is_empty() {
                        Ok(vec![wrap(Term::IsDir(is_dir))])
                    } else if negated {
                        // `!folder:foo` is ambiguous; refuse rather than guess.
                        Err(ParseError::CannotNegate(format!("{field}:{value}")))
                    } else {
                        Ok(vec![Term::IsDir(is_dir), name_or_path_term(value)])
                    }
                }
                _ => unreachable!(),
            };
        }
    }

    Ok(vec![wrap(name_or_path_term(unquote(atom)))])
}

fn name_or_path_term(value: &str) -> Term {
    let wild = has_wildcard(value);
    let pathy = value.contains('\\');
    match (pathy, wild) {
        (true, true) => Term::PathWildcard(value.to_string()),
        (true, false) => Term::Path(value.to_string()),
        (false, true) => Term::Wildcard(value.to_string()),
        (false, false) => Term::Name(value.to_string()),
    }
}

fn has_wildcard(s: &str) -> bool {
    s.contains(['*', '?'])
}

/// `size:` value → inclusive byte range.
/// Forms: `123`, `1kb`, `1.5mb`, `>1gb`, `>=`, `<`, `<=`, `1mb..2gb`.
fn parse_size_range(v: &str) -> Option<(u64, u64)> {
    if let Some((a, b)) = v.split_once("..") {
        return Some((parse_size(a)?, parse_size(b)?));
    }
    if let Some(r) = v.strip_prefix(">=") {
        return Some((parse_size(r)?, u64::MAX));
    }
    if let Some(r) = v.strip_prefix("<=") {
        return Some((0, parse_size(r)?));
    }
    if let Some(r) = v.strip_prefix('>') {
        return Some((parse_size(r)?.checked_add(1)?, u64::MAX));
    }
    if let Some(r) = v.strip_prefix('<') {
        return Some((0, parse_size(r)?.checked_sub(1)?));
    }
    let exact = parse_size(v)?;
    Some((exact, exact))
}

fn parse_size(v: &str) -> Option<u64> {
    let v = v.trim().to_ascii_lowercase();
    if v.is_empty() {
        return None;
    }
    let (num, mult) = if let Some(n) = v.strip_suffix("kb").or_else(|| v.strip_suffix('k')) {
        (n, 1u64 << 10)
    } else if let Some(n) = v.strip_suffix("mb").or_else(|| v.strip_suffix('m')) {
        (n, 1u64 << 20)
    } else if let Some(n) = v.strip_suffix("gb").or_else(|| v.strip_suffix('g')) {
        (n, 1u64 << 30)
    } else if let Some(n) = v.strip_suffix("tb").or_else(|| v.strip_suffix('t')) {
        (n, 1u64 << 40)
    } else if let Some(n) = v.strip_suffix('b') {
        (n, 1)
    } else {
        (v.as_str(), 1)
    };
    let num = num.trim();
    if num.is_empty() {
        return None;
    }
    if let Ok(i) = num.parse::<u64>() {
        return i.checked_mul(mult);
    }
    let f: f64 = num.parse().ok()?;
    if !f.is_finite() || f < 0.0 {
        return None;
    }
    Some((f * mult as f64) as u64)
}

/// `dm:` value → [start, end) civil-date bounds.
/// Forms: `2024`, `2024-03`, `2024/03/05`, `a..b`, `>x`, `>=x`, `<x`, `<=x`.
fn parse_date_range(v: &str) -> Option<(Option<Civil>, Option<Civil>)> {
    if let Some((a, b)) = v.split_once("..") {
        let (sa, _) = parse_date_period(a)?;
        let (_, eb) = parse_date_period(b)?;
        return Some((Some(sa), Some(eb)));
    }
    if let Some(r) = v.strip_prefix(">=") {
        let (s, _) = parse_date_period(r)?;
        return Some((Some(s), None));
    }
    if let Some(r) = v.strip_prefix("<=") {
        let (_, e) = parse_date_period(r)?;
        return Some((None, Some(e)));
    }
    if let Some(r) = v.strip_prefix('>') {
        let (_, e) = parse_date_period(r)?;
        return Some((Some(e), None));
    }
    if let Some(r) = v.strip_prefix('<') {
        let (s, _) = parse_date_period(r)?;
        return Some((None, Some(s)));
    }
    let (s, e) = parse_date_period(v)?;
    Some((Some(s), Some(e)))
}

/// One date period → [start, `end_exclusive`).
fn parse_date_period(v: &str) -> Option<(Civil, Civil)> {
    let parts: Vec<&str> = v.trim().split(['-', '/']).collect();
    let nums: Vec<u32> = parts
        .iter()
        .map(|p| p.parse::<u32>().ok())
        .collect::<Option<_>>()?;
    match nums.as_slice() {
        [y] => {
            let start = Civil {
                y: *y as i32,
                m: 1,
                d: 1,
            };
            let end = Civil {
                y: *y as i32 + 1,
                m: 1,
                d: 1,
            };
            (start.is_valid() && end.is_valid()).then_some((start, end))
        }
        [y, m] => {
            let start = Civil {
                y: *y as i32,
                m: *m,
                d: 1,
            };
            start
                .is_valid()
                .then_some((start, start.first_of_next_month()))
        }
        [y, m, d] => {
            let start = Civil {
                y: *y as i32,
                m: *m,
                d: *d,
            };
            start.is_valid().then_some((start, start.next_day()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Ast {
        parse(s).unwrap()
    }

    #[test]
    fn plain_words_are_anded() {
        assert_eq!(
            p("foo bar").groups,
            vec![vec![Term::Name("foo".into()), Term::Name("bar".into())]]
        );
    }

    #[test]
    fn or_splits_groups() {
        assert_eq!(
            p("foo | bar baz").groups,
            vec![
                vec![Term::Name("foo".into())],
                vec![Term::Name("bar".into()), Term::Name("baz".into())],
            ]
        );
    }

    #[test]
    fn negation_and_double_negation() {
        assert_eq!(
            p("!tmp").groups[0][0],
            Term::Not(Box::new(Term::Name("tmp".into())))
        );
        assert_eq!(p("!!tmp").groups[0][0], Term::Name("tmp".into()));
    }

    #[test]
    fn phrase_keeps_spaces() {
        assert_eq!(
            p(r#""two words""#).groups[0][0],
            Term::Name("two words".into())
        );
    }

    #[test]
    fn backslash_switches_to_path() {
        assert_eq!(
            p(r"docs\reports").groups[0][0],
            Term::Path(r"docs\reports".into())
        );
    }

    #[test]
    fn wildcards_detected() {
        assert_eq!(p("*.rs").groups[0][0], Term::Wildcard("*.rs".into()));
        assert_eq!(
            p(r"src\*.rs").groups[0][0],
            Term::PathWildcard(r"src\*.rs".into())
        );
    }

    #[test]
    fn ext_filter_splits_csv() {
        assert_eq!(
            p("ext:jpg;png,.gif").groups[0][0],
            Term::Ext(vec!["jpg".into(), "png".into(), "gif".into()])
        );
    }

    #[test]
    fn quoted_filter_value() {
        assert_eq!(
            p(r#"path:"Program Files""#).groups[0][0],
            Term::Path("Program Files".into())
        );
    }

    #[test]
    fn size_ranges() {
        assert_eq!(
            p("size:>1kb").groups[0][0],
            Term::Size {
                min: 1025,
                max: u64::MAX
            }
        );
        assert_eq!(
            p("size:1kb..1mb").groups[0][0],
            Term::Size {
                min: 1024,
                max: 1 << 20
            }
        );
        assert_eq!(
            p("size:1.5kb").groups[0][0],
            Term::Size {
                min: 1536,
                max: 1536
            }
        );
        assert!(parse("size:abc").is_err());
    }

    #[test]
    fn date_ranges() {
        let y2024 = Civil {
            y: 2024,
            m: 1,
            d: 1,
        };
        let y2025 = Civil {
            y: 2025,
            m: 1,
            d: 1,
        };
        assert_eq!(
            p("dm:2024").groups[0][0],
            Term::Mtime {
                start: Some(y2024),
                end: Some(y2025)
            }
        );
        assert_eq!(
            p("dm:>=2024-03").groups[0][0],
            Term::Mtime {
                start: Some(Civil {
                    y: 2024,
                    m: 3,
                    d: 1
                }),
                end: None
            }
        );
        assert_eq!(
            p("dm:2024/02/28..2024/02/29").groups[0][0],
            Term::Mtime {
                start: Some(Civil {
                    y: 2024,
                    m: 2,
                    d: 28
                }),
                end: Some(Civil {
                    y: 2024,
                    m: 3,
                    d: 1
                }),
            }
        );
        assert!(parse("dm:2023-02-29").is_err());
    }

    #[test]
    fn folder_with_value_expands() {
        assert_eq!(
            p("folder:src").groups[0],
            vec![Term::IsDir(true), Term::Name("src".into())]
        );
        assert!(parse("!folder:src").is_err());
        assert_eq!(
            p("!folder:").groups[0][0],
            Term::Not(Box::new(Term::IsDir(true)))
        );
    }

    #[test]
    fn unknown_colon_atom_is_a_name() {
        assert_eq!(p("12:30").groups[0][0], Term::Name("12:30".into()));
    }

    #[test]
    fn unclosed_quote_errors() {
        assert_eq!(parse(r#""abc"#), Err(ParseError::UnclosedQuote));
    }

    #[test]
    fn empty_query_is_match_all() {
        assert_eq!(p("").groups, vec![Vec::<Term>::new()]);
    }
}

#[cfg(test)]
mod proptests {
    use proptest::{prop_assert, proptest};

    use super::parse;

    proptest! {
        // `parse` must never panic on ANY input — the documented
        // "# Panics: Does not panic" contract, validated across the whole input
        // space (a lightweight fuzz). On success the AST always has at least one
        // group (it is seeded with one and only grows).
        #[test]
        fn parse_never_panics(s in ".*") {
            if let Ok(ast) = parse(&s) {
                prop_assert!(!ast.groups.is_empty());
            }
        }

        // Same property, biased to the query alphabet (operators, filters,
        // quotes, backslash) so the filter/operator parsing paths get dense
        // coverage rather than mostly-inert random Unicode.
        #[test]
        fn parse_never_panics_on_query_alphabet(
            s in "[a-zA-Z0-9 |!\"*?:;.<>=,/\\\\-]{0,40}"
        ) {
            if let Ok(ast) = parse(&s) {
                prop_assert!(!ast.groups.is_empty());
            }
        }
    }
}
