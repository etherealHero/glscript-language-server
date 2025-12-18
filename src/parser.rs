mod grammar {
    use faster_pest::*;

    #[derive(Parser)]
    #[grammar = "src/glscript_subset_grammar.pest"]
    pub struct GlScriptSubsetGrammar;
}

use grammar::{GlScriptSubsetGrammar, Ident, Rule};
use smol_str::{SmolStr, SmolStrBuilder};

// TODO: emit buld hash to doc hash (on parse) - CHECK bench without hasher on emit
#[derive(Debug)]
pub enum Token {
    Include(Span),
    IncludePath(RawToken),
    RegionOpen(Span),
    RegionClose(Span),
    LineTerminator(Position),
    Common(RawToken),
    CommonWithLineBreak(RawToken),
    FinalNewLine(Line),
}

type Line = u32;

#[derive(Debug, Clone)]
pub struct RawToken {
    pub pos: Position,
    pub text: SmolStr,
}

#[derive(Debug, Clone)]
pub struct Span {
    pub pos: Position,
    pub len: u32,
}

#[derive(Debug, Clone)]
pub struct Position {
    pub line: u32,
    pub col: u32,
}

struct PendingSpan {
    builder: SmolStrBuilder,
    line: u32,
    col: u32,
    ends_with_linebreak: bool,
}

fn pos_text_from_pair<'a>(pair: &'a faster_pest::Pair2<'_, Ident>) -> (Position, &'a str) {
    let sp = pair.as_span();
    let text = sp.as_str();
    let line_col = sp.start_pos().line_col();
    let (line, col) = (line_col.0 as u32 - 1, line_col.1 as u32 - 1);
    let pos = Position { line, col };
    (pos, text)
}

impl Span {
    pub fn new(pair: &faster_pest::Pair2<'_, Ident>) -> Self {
        let (pos, text) = pos_text_from_pair(pair);
        let len = text.len() as u32;
        Self { pos, len }
    }
}

impl RawToken {
    fn new(pair: &faster_pest::Pair2<'_, Ident>) -> Self {
        let (pos, text) = pos_text_from_pair(pair);
        let text = text.into();
        Self { pos, text }
    }
}

// #[tracing::instrument(skip(raw_text))]
pub fn parse(raw_text: &str) -> Vec<Token> {
    let pairs = GlScriptSubsetGrammar::parse(Rule::SourceFile, raw_text)
        .unwrap()
        .next()
        .unwrap()
        .into_inner();

    let mut out = Vec::with_capacity(raw_text.lines().count());
    let mut pending: Option<PendingSpan> = None;

    let flush = |out: &mut Vec<Token>, pending: &mut Option<PendingSpan>| {
        if let Some(p) = pending.take() {
            let (text, line, col) = (p.builder.finish(), p.line, p.col);
            let pos = Position { line, col };
            let raw_token = RawToken { text, pos };
            match p.ends_with_linebreak {
                true => out.push(Token::CommonWithLineBreak(raw_token)),
                false => out.push(Token::Common(raw_token)),
            };
        }
    };

    for ref pair in pairs {
        let rule = pair.as_rule();

        match rule {
            Rule::IncludeToken | Rule::IncludePath => flush(&mut out, &mut pending),
            Rule::RegionOpen | Rule::RegionClose => flush(&mut out, &mut pending),
            _ => {}
        }

        match rule {
            Rule::IncludeToken => out.push(Token::Include(Span::new(pair))),
            Rule::IncludePath => out.push(Token::IncludePath(RawToken::new(pair))),
            Rule::RegionOpen => out.push(Token::RegionOpen(Span::new(pair))),
            Rule::RegionClose => out.push(Token::RegionClose(Span::new(pair))),
            Rule::LineTerminator if pending.is_some() => {
                let acc = pending.as_mut().unwrap();
                acc.builder.push_str(pair.as_span().as_str());
                acc.ends_with_linebreak = true;
                flush(&mut out, &mut pending);
            }
            Rule::LineTerminator if pending.is_none() => {
                flush(&mut out, &mut pending);
                let line_col = pair.as_span().start_pos().line_col();
                let (line, col) = (line_col.0 as u32 - 1, line_col.1 as u32 - 1);
                out.push(Token::LineTerminator(Position { line, col }));
            }
            _ => {
                let sp = pair.as_span();
                let text = sp.as_str();
                let line_col = sp.start_pos().line_col();
                let (line, col) = (line_col.0 as u32 - 1, line_col.1 as u32 - 1);

                match pending {
                    Some(ref mut acc) if acc.line == line => acc.builder.push_str(text),
                    _ => {
                        flush(&mut out, &mut pending);
                        let mut builder = SmolStrBuilder::new();
                        builder.push_str(text);
                        pending = Some(PendingSpan {
                            ends_with_linebreak: false,
                            builder,
                            line,
                            col,
                        });
                    }
                };
            }
        };
    }

    flush(&mut out, &mut pending);

    match out.last() {
        Some(Token::LineTerminator(p)) => out.push(Token::FinalNewLine(p.line + 1)),
        Some(Token::CommonWithLineBreak(r)) => out.push(Token::FinalNewLine(r.pos.line + 1)),
        _ => {}
    }

    out
}
