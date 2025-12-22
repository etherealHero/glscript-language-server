mod grammar {
    // turn on some pest crate in Cargo.toml
    // use pest_derive::Parser;
    use faster_pest::*;

    #[derive(Parser)]
    #[grammar = "src/glscript_subset_grammar.pest"]
    pub struct GlScriptSubsetGrammar;
}

// for debug only
use crate::state::State;

use derive_more::{Constructor, From};

// turn on some pest crate in Cargo.toml
// /*
use grammar::{GlScriptSubsetGrammar, Ident, Rule};
type Pairs<'a> = faster_pest::Pairs2<'a, Ident<'a>>;
#[allow(unused)]
type Pair<'a> = faster_pest::Pair2<'a, Ident<'a>>;
// */
/*
use grammar::{GlScriptSubsetGrammar, Rule};
use pest::{
    Parser,
    iterators::{Pair as PestPair, Pairs as PestPairs},
};
type Pairs<'a> = PestPairs<'a, Rule>;
type Pair<'a> = PestPair<'a, Rule>;
// */

#[derive(Debug)]
pub enum Token<'a> {
    Include(RawToken<'a>),
    IncludePath(RawToken<'a>),
    RegionOpen(RawToken<'a>),
    RegionClose(RawToken<'a>),
    LineTerminator(RawToken<'a>),
    Common(RawToken<'a>),
    CommonWithLineEnding(RawToken<'a>),
    EOI(RawToken<'a>),
}

#[derive(Debug, Constructor)]
pub struct RawToken<'a> {
    pub line_col: LineCol,
    pub len: usize,
    pub text: Option<&'a str>,
}

#[derive(Debug, From)]
pub struct LineCol {
    pub line: usize,
    pub col: usize,
}

#[derive(Constructor)]
struct Pending {
    init_line_col: LineCol,
    init_pos: usize,
    pending_len: usize,
    has_linebreak: bool,
}

#[tracing::instrument(skip(raw_text))]
fn parse_raw_text(entry_rule: Rule, raw_text: &str) -> Pairs<'_> {
    GlScriptSubsetGrammar::parse(entry_rule, raw_text)
        .unwrap()
        .next()
        .unwrap()
        .into_inner()
}

#[tracing::instrument(skip_all)]
fn get_pairs<'a>(raw_text: &'a str, _state: &State) -> Pairs<'a> {
    let pairs = parse_raw_text(Rule::SourceFileFast, raw_text);
    let (mut pos, mut ok) = (0, true);

    for p in pairs.clone() {
        let s = p.as_span().as_str();
        let end = pos + s.len();

        if raw_text.get(pos..end) != Some(s) {
            ok = false;
            break;
        }

        pos = end;
    }

    match ok && pos == raw_text.len() {
        true => pairs,
        false => {
            #[cfg(debug_assertions)]
            {
                let mut emit_text = String::with_capacity(raw_text.len());
                let walk = |n: Pair<'_>| emit_text.push_str(n.as_span().as_str());
                pairs.clone().for_each(walk);

                std::fs::write(_state.get_project().join("emit_text.txt"), emit_text).unwrap();
                std::fs::write(_state.get_project().join("raw_text.txt"), raw_text).unwrap();
            }

            parse_raw_text(Rule::SourceFile, raw_text) // fallback
        }
    }
}

#[tracing::instrument(skip_all)]
pub fn parse<'a>(raw_text: &'a str, _state: &State) -> Vec<Token<'a>> {
    let pairs = get_pairs(raw_text, _state);
    let mut out = Vec::with_capacity(raw_text.lines().count());
    let (mut line, mut offset, mut pos, mut pending) = (0, 0, 0, None::<Pending>);
    let flush_pending_token = |p: Pending| {
        let pending_range = p.init_pos..(p.init_pos + p.pending_len);
        let text = raw_text.get(pending_range).unwrap();
        let token = RawToken::new(p.init_line_col, p.pending_len, Some(text));
        match p.has_linebreak {
            true => Token::CommonWithLineEnding(token),
            false => Token::Common(token),
        }
    };

    for ref pair in pairs {
        let (rule, pair_str) = (pair.as_rule(), pair.as_str());
        let pair_len = pair_str.len();
        let emit_token = || {
            let text = raw_text.get(pos..(pos + pair_len)).unwrap();
            RawToken::new((line, offset).into(), pair_len, Some(text))
        };

        if matches!(
            rule,
            Rule::IncludeToken | Rule::IncludePath | Rule::RegionOpen | Rule::RegionClose
        ) {
            pending.take().and_then(|p| {
                out.push(flush_pending_token(p));
                Some(())
            });
        }

        match rule {
            Rule::LineTerminator | Rule::CommonWithLineEnding if pending.is_some() => {
                pending.take().and_then(|mut p| {
                    p.has_linebreak = true;
                    p.pending_len += pair_len;
                    out.push(flush_pending_token(p));
                    Some(())
                });
            }
            Rule::LineTerminator => out.push(Token::LineTerminator(emit_token())),
            Rule::CommonWithLineEnding => out.push(Token::CommonWithLineEnding(emit_token())),
            Rule::IncludeToken => out.push(Token::Include(emit_token())),
            Rule::IncludePath => out.push(Token::IncludePath(emit_token())),
            Rule::RegionOpen => out.push(Token::RegionOpen(emit_token())),
            Rule::RegionClose => out.push(Token::RegionClose(emit_token())),
            _ => match pending {
                Some(ref mut acc) if acc.init_line_col.line == line => acc.pending_len += pair_len,
                _ => pending = Pending::new((line, offset).into(), pos, pair_len, false).into(),
            },
        };

        pos += pair_len;
        match matches!(rule, Rule::LineTerminator | Rule::CommonWithLineEnding) {
            true => (offset = 0, line += 1),
            false => (offset += pair_str.chars().count(), ()),
        };
    }

    pending.take().and_then(|p| {
        out.push(flush_pending_token(p));
        Some(())
    });

    let end_of_input: LineCol = match out.last() {
        Some(Token::LineTerminator(r)) => (r.line_col.line + 1, 0).into(),
        Some(Token::CommonWithLineEnding(r)) => (r.line_col.line + 1, 0).into(),
        Some(Token::IncludePath(r)) => (r.line_col.line, r.line_col.col + r.len).into(),
        Some(Token::RegionClose(r)) => (r.line_col.line, r.line_col.col + r.len).into(),
        Some(Token::Common(r)) => (r.line_col.line, r.line_col.col + r.len).into(),
        None => (0, 0).into(),
        _ => unreachable!(),
    };

    out.push(Token::EOI(RawToken::new(end_of_input, 0, None)));
    out
}
