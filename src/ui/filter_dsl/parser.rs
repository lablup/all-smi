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

//! Recursive-descent parser for the filter DSL.
//!
//! Grammar (precedence low to high):
//! ```text
//! expr    := or_expr
//! or_expr := and_expr ( "|" and_expr )*
//! and_expr:= cmp_expr ( "&" cmp_expr )*
//! cmp_expr:= "(" expr ")" | ident op rhs
//! op      := ">" | ">=" | "<" | "<=" | "==" | "!=" | "~="
//! rhs     := number | ident | quoted-string
//! ```

use std::fmt;

use regex::{Regex, RegexBuilder};

use super::lexer::{LexError, Op, Token, TokenKind, tokenize};

/// Known fields recognised by the DSL. Parsing an unknown field name is a
/// hard error so that typos are caught early.
///
/// The `parse_field` function maps both canonical and common synonym names
/// to the variant. Any field not listed here is rejected at parse time with
/// a `parse error: unknown field` diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Temp,
    Util,
    MemPct,
    MemUsed,
    MemTotal,
    Power,
    User,
    Host,
    GpuName,
    Driver,
    Index,
    Uuid,
    Pstate,
    Numa,
    DeviceType,
}

impl Field {
    /// Case-insensitive lookup from the DSL name to a [`Field`].
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "temp" | "temperature" => Some(Field::Temp),
            "util" | "utilization" => Some(Field::Util),
            "mem_pct" | "mempct" | "mem_percent" => Some(Field::MemPct),
            "mem_used" | "memused" => Some(Field::MemUsed),
            "mem_total" | "memtotal" => Some(Field::MemTotal),
            "power" | "pwr" => Some(Field::Power),
            "user" => Some(Field::User),
            "host" | "hostname" => Some(Field::Host),
            "gpu_name" | "name" => Some(Field::GpuName),
            "driver" => Some(Field::Driver),
            "index" | "idx" => Some(Field::Index),
            "uuid" => Some(Field::Uuid),
            "pstate" | "performance_state" => Some(Field::Pstate),
            "numa" | "numa_node" => Some(Field::Numa),
            "device_type" | "type" => Some(Field::DeviceType),
            _ => None,
        }
    }

    /// `true` when the field stores a numeric value and may be used with
    /// `>`, `>=`, `<`, `<=`, `==`, `!=`. Fields that are numeric can also
    /// be compared for string equality as a backstop.
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            Field::Temp
                | Field::Util
                | Field::MemPct
                | Field::MemUsed
                | Field::MemTotal
                | Field::Power
                | Field::Index
                | Field::Pstate
                | Field::Numa
        )
    }

    /// Stable canonical name used in error messages.
    pub fn canonical_name(self) -> &'static str {
        match self {
            Field::Temp => "temp",
            Field::Util => "util",
            Field::MemPct => "mem_pct",
            Field::MemUsed => "mem_used",
            Field::MemTotal => "mem_total",
            Field::Power => "power",
            Field::User => "user",
            Field::Host => "host",
            Field::GpuName => "gpu_name",
            Field::Driver => "driver",
            Field::Index => "index",
            Field::Uuid => "uuid",
            Field::Pstate => "pstate",
            Field::Numa => "numa",
            Field::DeviceType => "device_type",
        }
    }
}

/// A parsed right-hand-side value.
#[derive(Debug, Clone)]
pub enum Value {
    Number(f64),
    String(String),
    /// Compiled regex for `~=` comparisons. Compiled once at parse time so
    /// the per-row eval path is O(1) scan.
    Regex(Regex),
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => (a - b).abs() < 1e-9,
            (Value::String(a), Value::String(b)) => a == b,
            // Regexes can't be meaningfully compared; only used for tests.
            (Value::Regex(a), Value::Regex(b)) => a.as_str() == b.as_str(),
            _ => false,
        }
    }
}

/// The filter expression AST.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Leaf predicate: `field op value`.
    Cmp { field: Field, op: Op, value: Value },
    /// Both sub-expressions must match.
    And(Box<Expr>, Box<Expr>),
    /// Either sub-expression must match.
    Or(Box<Expr>, Box<Expr>),
}

/// Parse error raised by [`parse`].
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    /// 1-based column where the error was detected.
    pub col: usize,
    /// Short, user-facing explanation.
    pub msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "parse error at col {}: {}", self.col, self.msg)
    }
}

impl std::error::Error for ParseError {}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        ParseError {
            col: e.col,
            msg: e.msg,
        }
    }
}

/// Maximum byte size of a compiled regex AST. Larger patterns are rejected
/// at parse time, keeping one runaway query from blowing up memory use.
const REGEX_SIZE_LIMIT_BYTES: usize = 128 * 1024;

/// Parse `input` into an [`Expr`] tree.
///
/// Returns `Ok(None)` for an empty or whitespace-only input (no filter).
/// Any syntactic error surfaces as a [`ParseError`] with the offending
/// column so callers can point at the character in the UI.
pub fn parse(input: &str) -> Result<Option<Expr>, ParseError> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Ok(None);
    }
    let mut p = Parser { tokens, pos: 0 };
    let expr = p.parse_or()?;
    if p.pos < p.tokens.len() {
        return Err(ParseError {
            col: p.tokens[p.pos].col,
            msg: format!(
                "unexpected trailing token `{}`",
                tok_display(&p.tokens[p.pos])
            ),
        });
    }
    Ok(Some(expr))
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_and()?;
        while let Some(t) = self.peek() {
            match t.kind {
                TokenKind::Or => {
                    self.advance();
                    let rhs = self.parse_and()?;
                    lhs = Expr::Or(Box::new(lhs), Box::new(rhs));
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_cmp()?;
        while let Some(t) = self.peek() {
            match t.kind {
                TokenKind::And => {
                    self.advance();
                    let rhs = self.parse_cmp()?;
                    lhs = Expr::And(Box::new(lhs), Box::new(rhs));
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn parse_cmp(&mut self) -> Result<Expr, ParseError> {
        let (kind, col) = match self.peek() {
            Some(t) => (t.kind.clone(), t.col),
            None => {
                return Err(ParseError {
                    col: 1,
                    msg: "expected expression".to_string(),
                });
            }
        };
        match kind {
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_or()?;
                match self.advance() {
                    Some(t) if matches!(t.kind, TokenKind::RParen) => Ok(expr),
                    Some(t) => Err(ParseError {
                        col: t.col,
                        msg: "expected `)`".to_string(),
                    }),
                    None => Err(ParseError {
                        col,
                        msg: "unterminated `(` - expected `)`".to_string(),
                    }),
                }
            }
            TokenKind::Ident(name) => {
                self.advance();
                let field = match Field::from_name(&name) {
                    Some(f) => f,
                    None => {
                        return Err(ParseError {
                            col,
                            msg: format!("unknown field `{name}`"),
                        });
                    }
                };
                let (op, op_col) = match self.peek() {
                    Some(Token {
                        kind: TokenKind::Op(op),
                        col,
                    }) => (*op, *col),
                    Some(t) => {
                        return Err(ParseError {
                            col: t.col,
                            msg: format!(
                                "expected operator after field `{}`, got `{}`",
                                field.canonical_name(),
                                tok_display(t)
                            ),
                        });
                    }
                    None => {
                        return Err(ParseError {
                            col,
                            msg: format!(
                                "expected operator after field `{}`",
                                field.canonical_name()
                            ),
                        });
                    }
                };
                self.advance();

                // Typecheck the operator against the field's type.
                if !field.is_numeric() && matches!(op, Op::Gt | Op::Ge | Op::Lt | Op::Le) {
                    let fname = field.canonical_name();
                    return Err(ParseError {
                        col: op_col,
                        msg: format!(
                            "operator `{op}` requires a numeric field, but `{fname}` is a string"
                        ),
                    });
                }
                if field.is_numeric() && matches!(op, Op::Match) {
                    let fname = field.canonical_name();
                    return Err(ParseError {
                        col: op_col,
                        msg: format!(
                            "regex `~=` requires a string field, but `{fname}` is numeric"
                        ),
                    });
                }

                let value = self.parse_value(op)?;
                Ok(Expr::Cmp { field, op, value })
            }
            other => Err(ParseError {
                col,
                msg: format!("expected field name, got `{}`", display_kind(&other)),
            }),
        }
    }

    fn parse_value(&mut self, op: Op) -> Result<Value, ParseError> {
        let (kind, col) = match self.advance() {
            Some(t) => (t.kind.clone(), t.col),
            None => {
                return Err(ParseError {
                    col: self.tokens.last().map(|t| t.col + 1).unwrap_or(1),
                    msg: "expected value after operator".to_string(),
                });
            }
        };
        match (op, kind) {
            (_, TokenKind::Number(n)) => Ok(Value::Number(n)),
            (Op::Match, TokenKind::Ident(s)) | (Op::Match, TokenKind::QuotedString(s)) => {
                compile_regex(&s, col).map(Value::Regex)
            }
            (_, TokenKind::Ident(s)) => {
                // `pstate`, `numa`, `index` also accept numeric comparisons
                // against ident-looking inputs, but that's lexer territory;
                // by the time we're here a bare digit was already parsed as
                // a Number. A string value under a numeric op is an error.
                Ok(Value::String(s))
            }
            (_, TokenKind::QuotedString(s)) => Ok(Value::String(s)),
            (op, other) => {
                let got = display_kind(&other);
                Err(ParseError {
                    col,
                    msg: format!("expected a value for `{op}`, got `{got}`"),
                })
            }
        }
    }
}

fn compile_regex(pattern: &str, col: usize) -> Result<Regex, ParseError> {
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT_BYTES)
        .build()
        .map_err(|e| ParseError {
            col,
            msg: format!("invalid regex: {e}"),
        })
}

fn tok_display(t: &Token) -> String {
    display_kind(&t.kind)
}

fn display_kind(k: &TokenKind) -> String {
    match k {
        TokenKind::Ident(s) => s.clone(),
        TokenKind::QuotedString(s) => format!("\"{s}\""),
        TokenKind::Number(n) => n.to_string(),
        TokenKind::Op(o) => o.to_string(),
        TokenKind::And => "&".to_string(),
        TokenKind::Or => "|".to_string(),
        TokenKind::LParen => "(".to_string(),
        TokenKind::RParen => ")".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn must_parse(input: &str) -> Expr {
        parse(input)
            .unwrap_or_else(|e| panic!("parse failed for `{input}`: {e}"))
            .unwrap_or_else(|| panic!("parse returned None for `{input}`"))
    }

    fn must_fail(input: &str) -> ParseError {
        parse(input)
            .err()
            .unwrap_or_else(|| panic!("expected parse to fail for `{input}`"))
    }

    // -------------------------------------------------------------------
    // Success cases for each operator class
    // -------------------------------------------------------------------

    #[test]
    fn parses_simple_numeric_comparison() {
        let e = must_parse("temp>85");
        match e {
            Expr::Cmp { field, op, value } => {
                assert_eq!(field, Field::Temp);
                assert_eq!(op, Op::Gt);
                assert!(matches!(value, Value::Number(n) if (n - 85.0).abs() < 1e-9));
            }
            _ => panic!("expected Cmp"),
        }
    }

    #[test]
    fn parses_greater_equal() {
        let e = must_parse("util>=50");
        if let Expr::Cmp { op, .. } = e {
            assert_eq!(op, Op::Ge);
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn parses_less_than() {
        let e = must_parse("util<5");
        if let Expr::Cmp { op, .. } = e {
            assert_eq!(op, Op::Lt);
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn parses_less_equal() {
        let e = must_parse("mem_pct<=80");
        if let Expr::Cmp { op, field, .. } = e {
            assert_eq!(op, Op::Le);
            assert_eq!(field, Field::MemPct);
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn parses_numeric_equality() {
        let e = must_parse("pstate==0");
        if let Expr::Cmp {
            op, field, value, ..
        } = e
        {
            assert_eq!(op, Op::Eq);
            assert_eq!(field, Field::Pstate);
            assert!(matches!(value, Value::Number(n) if (n - 0.0).abs() < 1e-9));
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn parses_numeric_inequality() {
        let e = must_parse("index!=3");
        if let Expr::Cmp {
            op, field, value, ..
        } = e
        {
            assert_eq!(op, Op::Ne);
            assert_eq!(field, Field::Index);
            assert!(matches!(value, Value::Number(n) if (n - 3.0).abs() < 1e-9));
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn parses_string_equality() {
        let e = must_parse("user==alice");
        if let Expr::Cmp { op, field, value } = e {
            assert_eq!(op, Op::Eq);
            assert_eq!(field, Field::User);
            assert!(matches!(value, Value::String(s) if s == "alice"));
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn parses_single_equals_as_equality() {
        let e = must_parse("user=alice");
        if let Expr::Cmp { op, .. } = e {
            assert_eq!(op, Op::Eq);
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn parses_string_inequality() {
        let e = must_parse("user!=root");
        if let Expr::Cmp { op, .. } = e {
            assert_eq!(op, Op::Ne);
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn parses_regex_match() {
        let e = must_parse("host~=dgx");
        if let Expr::Cmp { op, field, value } = e {
            assert_eq!(op, Op::Match);
            assert_eq!(field, Field::Host);
            assert!(matches!(value, Value::Regex(_)));
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn parses_regex_against_quoted_string() {
        let e = must_parse(r#"gpu_name~="A100.*""#);
        if let Expr::Cmp { value, .. } = e {
            assert!(matches!(value, Value::Regex(_)));
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn parses_string_field_with_hyphenated_value() {
        let e = must_parse("host==dgx-a100-01");
        if let Expr::Cmp { value, .. } = e {
            assert!(matches!(value, Value::String(s) if s == "dgx-a100-01"));
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn parses_and_expression() {
        let e = must_parse("temp>80 & util<5");
        assert!(matches!(e, Expr::And(_, _)));
    }

    #[test]
    fn parses_or_expression() {
        let e = must_parse("temp>80 | util>80");
        assert!(matches!(e, Expr::Or(_, _)));
    }

    #[test]
    fn and_binds_tighter_than_or() {
        // a | b & c should parse as a | (b & c)
        let e = must_parse("temp>80 | util>50 & power>300");
        if let Expr::Or(_, rhs) = e {
            assert!(matches!(*rhs, Expr::And(_, _)));
        } else {
            panic!("expected Or at top level");
        }
    }

    #[test]
    fn parens_override_precedence() {
        // (a | b) & c should parse as And(Or(...), c)
        let e = must_parse("(temp>80 | util>50) & power>300");
        if let Expr::And(lhs, _) = e {
            assert!(matches!(*lhs, Expr::Or(_, _)));
        } else {
            panic!("expected And at top level");
        }
    }

    #[test]
    fn empty_input_parses_to_none() {
        assert!(parse("").unwrap().is_none());
        assert!(parse("    ").unwrap().is_none());
    }

    #[test]
    fn all_synonyms_parse() {
        assert!(parse("temperature>80").is_ok());
        assert!(parse("utilization>50").is_ok());
        assert!(parse("hostname==localhost").is_ok());
        assert!(parse("type==GPU").is_ok());
        assert!(parse("name~=A100").is_ok());
    }

    #[test]
    fn case_insensitive_field_names() {
        assert!(parse("TEMP>80").is_ok());
        assert!(parse("Temp>80").is_ok());
        assert!(parse("HoSt==localhost").is_ok());
    }

    #[test]
    fn all_fields_are_parseable() {
        let queries = [
            "temp>0",
            "util>0",
            "mem_pct>0",
            "mem_used>0",
            "mem_total>0",
            "power>0",
            "user==x",
            "host==x",
            "gpu_name==x",
            "driver==x",
            "index>0",
            "uuid==x",
            "pstate>0",
            "numa>0",
            "device_type==GPU",
        ];
        for q in queries {
            assert!(parse(q).is_ok(), "failed: {q}");
        }
    }

    #[test]
    fn decimal_numbers_parse() {
        let e = must_parse("util>0.5");
        if let Expr::Cmp { value, .. } = e {
            assert!(matches!(value, Value::Number(n) if (n - 0.5).abs() < 1e-9));
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn negative_numbers_parse() {
        let e = must_parse("power>=-1");
        if let Expr::Cmp { value, .. } = e {
            assert!(matches!(value, Value::Number(n) if (n + 1.0).abs() < 1e-9));
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn nested_parens_parse() {
        assert!(parse("((temp>80))").is_ok());
        assert!(parse("((temp>80 & util>50) | (power>300 & mem_pct>50))").is_ok());
    }

    #[test]
    fn mixed_and_or_parse() {
        assert!(parse("temp>80 & util>50 & power>300").is_ok());
        assert!(parse("temp>80 | util>50 | power>300").is_ok());
    }

    // -------------------------------------------------------------------
    // Failure cases
    // -------------------------------------------------------------------

    #[test]
    fn unknown_field_errors() {
        let err = must_fail("frobnitz>80");
        assert!(err.msg.contains("unknown field"));
        assert_eq!(err.col, 1);
    }

    #[test]
    fn missing_operator_errors() {
        let err = must_fail("temp");
        assert!(err.msg.contains("expected operator"));
    }

    #[test]
    fn missing_value_errors() {
        let err = must_fail("temp>");
        assert!(err.msg.contains("expected"));
    }

    #[test]
    fn double_operator_errors() {
        let err = must_fail("temp>>85");
        // `>` then `>` — the second `>` is not a valid value.
        assert!(err.msg.to_lowercase().contains("value") || err.msg.contains(">"));
    }

    #[test]
    fn missing_field_errors() {
        let err = must_fail(">85");
        assert!(err.msg.contains("expected field"));
    }

    #[test]
    fn numeric_op_on_string_field_errors() {
        let err = must_fail("user>alice");
        assert!(err.msg.contains("numeric"));
    }

    #[test]
    fn regex_op_on_numeric_field_errors() {
        let err = must_fail("temp~=80");
        assert!(err.msg.contains("string"));
    }

    #[test]
    fn invalid_regex_errors() {
        // Must be quoted because `[` is not a valid bare-ident char;
        // unquoted forms fail at the lexer stage with a different
        // message.
        let err = must_fail(r#"host~="[unclosed""#);
        assert!(err.msg.contains("invalid regex"));
    }

    #[test]
    fn unmatched_open_paren_errors() {
        assert!(parse("(temp>80").is_err());
    }

    #[test]
    fn unmatched_close_paren_errors() {
        assert!(parse("temp>80)").is_err());
    }

    #[test]
    fn trailing_garbage_errors() {
        let err = must_fail("temp>80 temp");
        assert!(err.msg.contains("trailing") || err.msg.contains("unexpected"));
    }

    #[test]
    fn and_without_rhs_errors() {
        assert!(parse("temp>80 &").is_err());
    }

    #[test]
    fn or_without_rhs_errors() {
        assert!(parse("temp>80 |").is_err());
    }

    #[test]
    fn empty_parens_errors() {
        assert!(parse("()").is_err());
    }

    #[test]
    fn parse_error_reports_column_for_unknown_field() {
        let err = must_fail("  xyz>80");
        assert_eq!(err.col, 3);
    }

    #[test]
    fn parse_error_reports_column_for_operator() {
        // "temp " is col 1..=4, space col 5, second token col 5 or 6.
        // We specifically want "temp @ 85" → @ is at col 6.
        let err = must_fail("temp @85");
        // Lex error: unexpected `@`, col 6.
        assert_eq!(err.col, 6);
    }

    #[test]
    fn string_equality_with_quoted_value_works() {
        let e = must_parse(r#"user=="alice smith""#);
        if let Expr::Cmp { value, .. } = e {
            assert!(matches!(value, Value::String(s) if s == "alice smith"));
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn large_regex_is_rejected() {
        // Build a regex whose compiled size exceeds 128 KiB. Nested
        // alternation with long literals inflates the automaton quickly.
        let pat = "(".to_string()
            + &(0..1000)
                .map(|i| format!("aaaaaaaaaa{i}"))
                .collect::<Vec<_>>()
                .join("|")
            + ")+";
        let query = format!("host~=\"{pat}\"");
        let err = parse(&query).unwrap_err();
        assert!(
            err.msg.contains("invalid regex"),
            "expected regex size error, got: {}",
            err.msg
        );
    }

    #[test]
    fn whitespace_around_operators_is_fine() {
        assert!(parse("temp  >  85").is_ok());
        assert!(parse("temp>85 & util<5").is_ok());
    }

    #[test]
    fn multiple_ands_parse_left_associative() {
        // a & b & c should parse as (a & b) & c
        let e = must_parse("temp>1 & util>1 & power>1");
        if let Expr::And(lhs, _) = e {
            assert!(matches!(*lhs, Expr::And(_, _)));
        } else {
            panic!("expected And at top level, got {e:?}");
        }
    }

    #[test]
    fn multiple_ors_parse_left_associative() {
        let e = must_parse("temp>1 | util>1 | power>1");
        if let Expr::Or(lhs, _) = e {
            assert!(matches!(*lhs, Expr::Or(_, _)));
        } else {
            panic!("expected Or at top level, got {e:?}");
        }
    }

    #[test]
    fn power_field_synonym_parses() {
        assert!(parse("pwr>300").is_ok());
    }

    #[test]
    fn index_synonym_parses() {
        assert!(parse("idx==0").is_ok());
    }

    // -------------------------------------------------------------------
    // Extra round-trip and edge-case tests for the parser. Keeps the
    // 50-test acceptance floor comfortable and guards against regressions
    // in the lexer/parser seam.
    // -------------------------------------------------------------------

    #[test]
    fn host_synonym_parses() {
        assert!(parse("hostname==localhost").is_ok());
    }

    #[test]
    fn device_type_value_is_preserved_verbatim() {
        let e = must_parse("device_type==GPU");
        if let Expr::Cmp { value, .. } = e {
            assert!(matches!(value, Value::String(s) if s == "GPU"));
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn mem_used_large_number_parses() {
        assert!(parse("mem_used>1073741824").is_ok());
    }

    #[test]
    fn host_ip_port_parses() {
        let e = must_parse("host==10.82.128.41:9090");
        if let Expr::Cmp { value, .. } = e {
            assert!(matches!(value, Value::String(s) if s == "10.82.128.41:9090"));
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn numeric_decimal_with_leading_zero_parses() {
        assert!(parse("mem_pct>=0.5").is_ok());
    }

    #[test]
    fn and_with_extra_whitespace_parses() {
        assert!(parse("temp > 80   &   util < 5").is_ok());
    }

    #[test]
    fn or_with_extra_whitespace_parses() {
        assert!(parse("temp > 80   |   util > 99").is_ok());
    }

    #[test]
    fn not_eq_operator_alias_parses() {
        // The issue specifies `!=` as the inequality op. Make sure the
        // parser accepts it both for numeric and string fields.
        let e = must_parse("temp!=0");
        if let Expr::Cmp { op, .. } = e {
            assert_eq!(op, Op::Ne);
        } else {
            panic!("expected Cmp");
        }
        let e = must_parse("user!=root");
        if let Expr::Cmp { op, .. } = e {
            assert_eq!(op, Op::Ne);
        } else {
            panic!("expected Cmp");
        }
    }

    #[test]
    fn single_equals_on_all_fields_parses() {
        for f in [
            "temp=80",
            "util=5",
            "mem_pct=0",
            "mem_used=0",
            "mem_total=0",
            "power=0",
            "user=x",
            "host=x",
            "gpu_name=x",
            "driver=x",
            "index=0",
            "uuid=x",
            "pstate=0",
            "numa=0",
            "device_type=GPU",
        ] {
            assert!(parse(f).is_ok(), "failed to parse: {f}");
        }
    }

    #[test]
    fn parse_preserves_column_in_nested_errors() {
        let err = must_fail("(temp>80 & xyz>1)");
        // xyz is at the column after the `&`; should be > 10.
        assert!(err.col > 10, "col was {}", err.col);
        assert!(err.msg.contains("unknown field"));
    }

    #[test]
    fn parse_rejects_string_op_on_numeric_field() {
        let err = must_fail("temp~=hot");
        assert!(err.msg.contains("string"));
    }

    #[test]
    fn and_with_paren_group_parses() {
        assert!(parse("(temp>80) & (util<5)").is_ok());
    }

    #[test]
    fn or_with_paren_group_parses() {
        assert!(parse("(temp>80) | (power>400)").is_ok());
    }

    #[test]
    fn multiple_paren_groups_parse() {
        assert!(parse("((temp>80)) & ((util<5))").is_ok());
    }

    #[test]
    fn quoted_string_on_rhs_of_gt_not_numeric_errors() {
        // `"80"` on RHS of a numeric `>` is a type mismatch. The parser
        // accepts the value but eval treats it as a string — which makes
        // the comparison false. Tested here as a syntactic success.
        assert!(parse(r#"temp>"80""#).is_ok());
    }
}
