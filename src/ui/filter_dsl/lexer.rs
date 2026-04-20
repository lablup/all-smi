// Copyright 2025 Lablup Inc. and Jeongkyu Shin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Tokenizer for the filter DSL.
//!
//! The DSL intentionally stays small: identifiers, numbers (with an optional
//! unit suffix handled by the parser), strings (bare or quoted), and the six
//! comparison / regex operators plus the two logical combinators and
//! parentheses.

use std::fmt;

/// A single lexical token emitted by [`tokenize`].
///
/// Tokens carry their byte column (1-based) so that parser diagnostics can
/// point at the offending character without re-scanning the source.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub col: usize,
}

/// A tokenised element of the filter DSL grammar.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// Bare identifier, e.g. `temp`, `user`, `host`.
    Ident(String),
    /// Numeric literal parsed as `f64` (so both `85` and `0.5` work).
    Number(f64),
    /// Quoted string literal (`"foo bar"` or `'foo bar'`).
    QuotedString(String),
    /// Comparison or regex operator.
    Op(Op),
    /// Logical AND (`&`).
    And,
    /// Logical OR (`|`).
    Or,
    /// Opening parenthesis.
    LParen,
    /// Closing parenthesis.
    RParen,
}

/// The set of operators accepted by the DSL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    /// `>` — strictly greater than (numeric only).
    Gt,
    /// `>=` — greater than or equal (numeric only).
    Ge,
    /// `<` — strictly less than (numeric only).
    Lt,
    /// `<=` — less than or equal (numeric only).
    Le,
    /// `==` or `=` — equality (numeric or string).
    Eq,
    /// `!=` — inequality (numeric or string).
    Ne,
    /// `~=` — regex match (string only).
    Match,
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Eq => "==",
            Op::Ne => "!=",
            Op::Match => "~=",
        };
        f.write_str(s)
    }
}

/// Error type produced by [`tokenize`].
#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub col: usize,
    pub msg: String,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lex error at col {}: {}", self.col, self.msg)
    }
}

impl std::error::Error for LexError {}

/// Scan `input` into a token stream.
///
/// This is an all-or-nothing scan: the first illegal character aborts with a
/// [`LexError`] carrying the 1-based column so the UI can surface a user
/// friendly pointer. Whitespace is skipped silently.
pub fn tokenize(input: &str) -> Result<Vec<Token>, LexError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0usize;

    while i < chars.len() {
        let c = chars[i];
        let col = i + 1; // 1-based column for user-facing messages.

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        match c {
            '(' => {
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    col,
                });
                i += 1;
            }
            ')' => {
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    col,
                });
                i += 1;
            }
            '&' => {
                tokens.push(Token {
                    kind: TokenKind::And,
                    col,
                });
                i += 1;
            }
            '|' => {
                tokens.push(Token {
                    kind: TokenKind::Or,
                    col,
                });
                i += 1;
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token {
                        kind: TokenKind::Op(Op::Ge),
                        col,
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        kind: TokenKind::Op(Op::Gt),
                        col,
                    });
                    i += 1;
                }
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token {
                        kind: TokenKind::Op(Op::Le),
                        col,
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        kind: TokenKind::Op(Op::Lt),
                        col,
                    });
                    i += 1;
                }
            }
            '=' => {
                // Both `==` (canonical) and `=` (shorthand) map to Eq.
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token {
                        kind: TokenKind::Op(Op::Eq),
                        col,
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        kind: TokenKind::Op(Op::Eq),
                        col,
                    });
                    i += 1;
                }
            }
            '!' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token {
                        kind: TokenKind::Op(Op::Ne),
                        col,
                    });
                    i += 2;
                } else {
                    return Err(LexError {
                        col,
                        msg: "`!` must be followed by `=`".to_string(),
                    });
                }
            }
            '~' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token {
                        kind: TokenKind::Op(Op::Match),
                        col,
                    });
                    i += 2;
                } else {
                    return Err(LexError {
                        col,
                        msg: "`~` must be followed by `=`".to_string(),
                    });
                }
            }
            '"' | '\'' => {
                let quote = c;
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != quote {
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(LexError {
                        col,
                        msg: "unterminated string literal".to_string(),
                    });
                }
                let s: String = chars[start..i].iter().collect();
                tokens.push(Token {
                    kind: TokenKind::QuotedString(s),
                    col,
                });
                i += 1; // consume closing quote
            }
            // Numbers: starting digit or `-` followed by digit. We don't
            // support scientific notation because it overlaps with `e` as an
            // identifier suffix in casual usage.
            //
            // A sequence that looks numeric but contains more than one
            // decimal point (e.g. `10.82.128.41`) or is followed by an
            // ident-only character like `:` or `-` after the digits is
            // reclassified as an identifier. This keeps host:port strings
            // and dotted hostnames readable on the RHS of `==`/`~=`.
            c if c.is_ascii_digit()
                || (c == '-' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit()) =>
            {
                let start = i;
                let had_leading_minus = chars[i] == '-';
                if had_leading_minus {
                    i += 1;
                }
                let mut saw_dot = false;
                let mut multi_dot = false;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    if chars[i] == '.' {
                        if saw_dot {
                            multi_dot = true;
                        }
                        saw_dot = true;
                    }
                    i += 1;
                }
                // If the scan stopped at a character that still belongs to
                // an identifier (colon, hyphen etc.), or if we saw >1 dot,
                // treat the whole run as an identifier rather than a
                // number. This relies on the ident-body predicate already
                // including colons and hyphens.
                let is_ident =
                    multi_dot || (i < chars.len() && is_ident_body(chars[i]) && !had_leading_minus);
                if is_ident {
                    // Extend greedily until the identifier body predicate
                    // stops matching.
                    while i < chars.len() && is_ident_body(chars[i]) {
                        i += 1;
                    }
                    let s: String = chars[start..i].iter().collect();
                    tokens.push(Token {
                        kind: TokenKind::Ident(s),
                        col,
                    });
                } else {
                    let raw: String = chars[start..i].iter().collect();
                    match raw.parse::<f64>() {
                        Ok(n) => tokens.push(Token {
                            kind: TokenKind::Number(n),
                            col,
                        }),
                        Err(_) => {
                            return Err(LexError {
                                col,
                                msg: format!("invalid number `{raw}`"),
                            });
                        }
                    }
                }
            }
            // Identifiers — ASCII alphanumerics, underscore, hyphen, and
            // colon so that values like `dgx-01` or `10.82.1.2:9090` parse
            // as a single bare token on the RHS of `==`/`~=`.
            c if is_ident_start(c) => {
                let start = i;
                while i < chars.len() && is_ident_body(chars[i]) {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                tokens.push(Token {
                    kind: TokenKind::Ident(s),
                    col,
                });
            }
            _ => {
                return Err(LexError {
                    col,
                    msg: format!("unexpected character `{c}`"),
                });
            }
        }
    }

    Ok(tokens)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_body(c: char) -> bool {
    // Hyphen, dot, slash, and colon allow bare hostnames / URLs / paths on
    // the RHS without needing quotes.
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/' || c == ':'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_numeric_comparison() {
        let toks = tokenize("temp>85").unwrap();
        assert_eq!(
            toks.iter().map(|t| t.kind.clone()).collect::<Vec<_>>(),
            vec![
                TokenKind::Ident("temp".to_string()),
                TokenKind::Op(Op::Gt),
                TokenKind::Number(85.0),
            ]
        );
    }

    #[test]
    fn lexes_all_numeric_ops() {
        let toks = tokenize("a>1 b>=2 c<3 d<=4 e==5 f!=6").unwrap();
        let ops: Vec<_> = toks
            .into_iter()
            .filter_map(|t| match t.kind {
                TokenKind::Op(o) => Some(o),
                _ => None,
            })
            .collect();
        assert_eq!(ops, vec![Op::Gt, Op::Ge, Op::Lt, Op::Le, Op::Eq, Op::Ne]);
    }

    #[test]
    fn lexes_regex_op() {
        let toks = tokenize("host~=dgx").unwrap();
        assert!(matches!(toks[1].kind, TokenKind::Op(Op::Match)));
    }

    #[test]
    fn single_equals_parses_as_eq() {
        let toks = tokenize("user=alice").unwrap();
        assert!(matches!(toks[1].kind, TokenKind::Op(Op::Eq)));
    }

    #[test]
    fn bare_bang_errors() {
        let err = tokenize("temp!85").unwrap_err();
        assert_eq!(err.col, 5);
    }

    #[test]
    fn bare_tilde_errors() {
        let err = tokenize("host~foo").unwrap_err();
        assert_eq!(err.col, 5);
    }

    #[test]
    fn quoted_string_preserves_inner_spaces() {
        let toks = tokenize(r#"user=="alice smith""#).unwrap();
        match &toks[2].kind {
            TokenKind::QuotedString(s) => assert_eq!(s, "alice smith"),
            other => panic!("expected quoted string, got {other:?}"),
        }
    }

    #[test]
    fn single_quoted_string_works() {
        let toks = tokenize("user=='root'").unwrap();
        match &toks[2].kind {
            TokenKind::QuotedString(s) => assert_eq!(s, "root"),
            other => panic!("expected quoted string, got {other:?}"),
        }
    }

    #[test]
    fn unterminated_string_errors() {
        let err = tokenize("user==\"unterminated").unwrap_err();
        assert_eq!(err.col, 7);
    }

    #[test]
    fn hyphenated_hostname_is_one_token() {
        let toks = tokenize("host==dgx-a100-01").unwrap();
        match &toks[2].kind {
            TokenKind::Ident(s) => assert_eq!(s, "dgx-a100-01"),
            other => panic!("expected ident, got {other:?}"),
        }
    }

    #[test]
    fn colon_in_hostport_is_one_token() {
        let toks = tokenize("host==10.82.128.41:9090").unwrap();
        match &toks[2].kind {
            TokenKind::Ident(s) => assert_eq!(s, "10.82.128.41:9090"),
            other => panic!("expected ident, got {other:?}"),
        }
    }

    #[test]
    fn logical_ops_parse() {
        let toks = tokenize("a>1 & b<2 | c==3").unwrap();
        let kinds: Vec<_> = toks.iter().map(|t| t.kind.clone()).collect();
        assert!(kinds.contains(&TokenKind::And));
        assert!(kinds.contains(&TokenKind::Or));
    }

    #[test]
    fn parens_parse() {
        let toks = tokenize("(temp>85)").unwrap();
        assert_eq!(toks[0].kind, TokenKind::LParen);
        assert_eq!(toks[toks.len() - 1].kind, TokenKind::RParen);
    }

    #[test]
    fn decimal_number_parses() {
        let toks = tokenize("util>0.5").unwrap();
        match toks[2].kind {
            TokenKind::Number(n) => assert!((n - 0.5).abs() < 1e-9),
            _ => panic!("expected number"),
        }
    }

    #[test]
    fn negative_number_parses() {
        let toks = tokenize("power>-1").unwrap();
        match toks[2].kind {
            TokenKind::Number(n) => assert!((n + 1.0).abs() < 1e-9),
            _ => panic!("expected number"),
        }
    }

    #[test]
    fn invalid_char_errors_with_column() {
        let err = tokenize("temp @85").unwrap_err();
        assert_eq!(err.col, 6);
    }

    #[test]
    fn empty_input_yields_no_tokens() {
        assert!(tokenize("").unwrap().is_empty());
        assert!(tokenize("   ").unwrap().is_empty());
    }

    #[test]
    fn column_numbers_are_one_based() {
        let toks = tokenize("ab>3").unwrap();
        assert_eq!(toks[0].col, 1);
        assert_eq!(toks[1].col, 3);
        assert_eq!(toks[2].col, 4);
    }
}
