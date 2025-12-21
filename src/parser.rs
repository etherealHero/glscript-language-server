mod grammar {
    use faster_pest::*;

    #[derive(Parser)]
    #[grammar = "src/glscript_subset_grammar.pest"]
    pub struct GlScriptSubsetGrammar;
}

use derive_more::{Constructor, From};
use grammar::{GlScriptSubsetGrammar, Ident, Rule};
use smol_str::{SmolStr, SmolStrBuilder};

// for debug only
use crate::state::State;

#[derive(Debug)]
pub enum Token {
    Include(RawToken),
    IncludePath(RawToken),
    RegionOpen(RawToken),
    RegionClose(RawToken),
    LineTerminator(RawToken),
    Common(RawToken),
    CommonWithLineBreak(RawToken),
    EOI(RawToken),
}

#[derive(Debug, Constructor)]
pub struct RawToken {
    pub pos: Position,
    pub len: usize,
    pub text: Option<SmolStr>,
}

#[derive(Debug, From)]
pub struct Position {
    pub line: usize,
    pub col: usize,
}

#[derive(Constructor)]
struct PendingSpan {
    pos: Position,
    builder: SmolStrBuilder,
    ends_with_linebreak: bool,
}

impl<'a> From<&faster_pest::Pair2<'_, Ident<'a>>> for RawToken {
    fn from(pair: &faster_pest::Pair2<'_, Ident>) -> Self {
        let sp = pair.as_span();
        let text = sp.as_str();
        let len = text.len();
        let line_col = sp.start_pos().line_col();
        let pos = (line_col.0 - 1, line_col.1 - 1).into();
        let text = Some(text.into());
        Self { pos, text, len }
    }
}

#[tracing::instrument(skip(raw_text))]
fn parse_raw_text(entry_rule: Rule, raw_text: &str) -> faster_pest::Pairs2<'_, grammar::Ident<'_>> {
    GlScriptSubsetGrammar::parse(entry_rule, raw_text)
        .unwrap()
        .next()
        .unwrap()
        .into_inner()
}

#[inline]
fn flush(out: &mut Vec<Token>, p: PendingSpan) {
    let text = p.builder.finish();
    let raw_token = RawToken::new(p.pos, text.len(), text.into());
    match p.ends_with_linebreak {
        true => out.push(Token::CommonWithLineBreak(raw_token)),
        false => out.push(Token::Common(raw_token)),
    };
}

#[tracing::instrument(skip_all)]
fn push_eoi_token(out: &mut Vec<Token>) {
    let end_of_input: Position = match out.last() {
        Some(Token::LineTerminator(r)) => (r.pos.line + 1, 0).into(),
        Some(Token::CommonWithLineBreak(r)) => (r.pos.line + 1, 0).into(),
        Some(Token::IncludePath(r)) => (r.pos.line, r.pos.col + r.len).into(),
        Some(Token::RegionClose(r)) => (r.pos.line, r.pos.col + r.len).into(),
        Some(Token::Common(r)) => (r.pos.line, r.pos.col + r.len).into(),
        None => (0, 0).into(),
        _ => unreachable!(),
    };

    out.push(Token::EOI(RawToken::new(end_of_input, 0, None)));
}

#[tracing::instrument(skip_all)]
fn get_pairs<'a>(raw_text: &'a str, _state: &State) -> faster_pest::Pairs2<'a, grammar::Ident<'a>> {
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
                let walk =
                    |n: faster_pest::Pair2<'_, Ident<'_>>| emit_text.push_str(n.as_span().as_str());
                pairs.clone().for_each(walk);

                std::fs::write(_state.get_project().join("emit_text.txt"), emit_text).unwrap();
                std::fs::write(_state.get_project().join("raw_text.txt"), raw_text).unwrap();
            }

            parse_raw_text(Rule::SourceFile, raw_text) // fallback
        }
    }
}

#[tracing::instrument(skip_all)]
pub fn parse(raw_text: &str, _state: &State) -> Vec<Token> {
    let pairs = get_pairs(raw_text, _state);
    let mut out = Vec::with_capacity(raw_text.lines().count());
    let mut pending: Option<PendingSpan> = None;

    for ref pair in pairs {
        let rule = pair.as_rule();

        if matches!(
            rule,
            Rule::IncludeToken | Rule::IncludePath | Rule::RegionOpen | Rule::RegionClose
        ) {
            pending.take().and_then(|p| flush(&mut out, p).into());
        }

        match rule {
            Rule::IncludeToken => out.push(Token::Include(pair.into())),
            Rule::IncludePath => out.push(Token::IncludePath(pair.into())),
            Rule::RegionOpen => out.push(Token::RegionOpen(pair.into())),
            Rule::RegionClose => out.push(Token::RegionClose(pair.into())),
            Rule::LineTerminator if pending.is_some() => {
                let acc = pending.as_mut().unwrap();
                acc.builder.push('\n');
                acc.ends_with_linebreak = true;
                pending.take().and_then(|p| flush(&mut out, p).into());
            }
            Rule::LineTerminator => out.push(Token::LineTerminator(pair.into())),
            Rule::UntrackedWithLineEnding if pending.is_some() => {
                let acc = pending.as_mut().unwrap();
                acc.builder.push_str(pair.as_span().as_str());
                acc.ends_with_linebreak = true;
                pending.take().and_then(|p| flush(&mut out, p).into());
            }
            Rule::UntrackedWithLineEnding => out.push(Token::CommonWithLineBreak(pair.into())),
            _ => {
                let token: RawToken = pair.into();
                let (text, line) = (&token.text.unwrap(), token.pos.line);
                match pending {
                    Some(ref mut acc) if acc.pos.line == line => acc.builder.push_str(text),
                    _ => {
                        let mut builder = SmolStrBuilder::new();
                        builder.push_str(text);
                        pending = Some(PendingSpan::new(token.pos, builder, false));
                    }
                };
            }
        };
    }

    pending.take().and_then(|p| flush(&mut out, p).into());
    push_eoi_token(&mut out);

    out
}
