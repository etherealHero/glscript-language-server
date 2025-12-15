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
    Include,
    IncludePath(RawSpan),
    RegionOpen(RawSpan),
    RegionClose(RawSpan),
    LineTerminator(PositionSpan),
    Common(RawSpan),
    CommonWithLineBreak(RawSpan),
}

#[derive(Clone, Debug)]
pub struct RawSpan {
    pub text: SmolStr,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug)]
pub struct PositionSpan {
    pub line: u32,
    pub col: u32,
}

struct PendingSpan {
    builder: SmolStrBuilder,
    line: u32,
    col: u32,
    ends_with_linebreak: bool,
}

impl RawSpan {
    fn new(pair: &faster_pest::Pair2<'_, Ident>) -> Self {
        let sp = pair.as_span();
        let text = sp.as_str().into();
        let line_col = sp.start_pos().line_col();
        let (line, col) = (line_col.0 as u32 - 1, line_col.1 as u32 - 1);
        Self { line, col, text }
    }
}

#[tracing::instrument(skip(raw_text))]
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
            let span = RawSpan { text, line, col };
            match p.ends_with_linebreak {
                true => out.push(Token::CommonWithLineBreak(span)),
                false => out.push(Token::Common(span)),
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
            Rule::IncludeToken => out.push(Token::Include),
            Rule::IncludePath => out.push(Token::IncludePath(RawSpan::new(pair))),
            Rule::RegionOpen => out.push(Token::RegionOpen(RawSpan::new(pair))),
            Rule::RegionClose => out.push(Token::RegionClose(RawSpan::new(pair))),
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
                out.push(Token::LineTerminator(PositionSpan { line, col }));
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
    out
}
