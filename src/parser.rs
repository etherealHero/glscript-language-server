mod grammar {
    use faster_pest::*;

    #[derive(Parser)]
    #[grammar = "src/glscript_subset_grammar.pest"]
    pub struct GlScriptSubsetGrammar;
}

use crate::state::State;
use derive_more::{Constructor, From};
use grammar::{GlScriptSubsetGrammar, Ident, Rule};

#[allow(unused)]
type Pair<'a> = faster_pest::Pair2<'a, Ident<'a>>;
type Pairs<'a> = faster_pest::Pairs2<'a, Ident<'a>>;

#[derive(Debug)]
pub enum Token<'a> {
    Include(Span),
    IncludePath(PathLiteral<'a>),
    RegionOpen(Span),
    RegionClose(Span),
    LineTerminator(LineCol),
    Common(RawToken<'a>),
    CommonWithLineEnding(RawToken<'a>),
    Eoi(LineCol),
}

#[derive(Debug, Constructor)]
pub struct RawToken<'a> {
    pub line_col: LineCol,
    pub text: &'a str,
}

#[derive(Debug, Constructor)]
pub struct PathLiteral<'a> {
    pub line_col: LineCol,
    pub path: &'a str,
}

#[derive(Debug, Constructor)]
pub struct Span {
    pub line_col: LineCol,
    pub len: u32,
}

#[derive(Debug, From)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

#[derive(Constructor)]
struct Pending {
    init_line_col: LineCol,
    init_pos: usize,
    pending_len: u32,
    has_linebreak: bool,
}

fn parse_raw_text(entry_rule: Rule, raw_text: &str) -> Pairs<'_> {
    GlScriptSubsetGrammar::parse(entry_rule, raw_text)
        .unwrap()
        .next()
        .unwrap()
        .into_inner()
}

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

pub fn parse<'a>(raw_text: &'a str, _state: &State) -> Vec<Token<'a>> {
    let pairs = get_pairs(raw_text, _state);
    let mut out = Vec::with_capacity(raw_text.lines().count());
    let (mut line, mut offset, mut pos, mut pending) = (0, 0, 0usize, None::<Pending>);
    let flush_pending_token = |p: Pending| {
        let pending_range = p.init_pos..p.init_pos + p.pending_len as usize;
        let text = unsafe { raw_text.get_unchecked(pending_range) };
        let token = RawToken::new(p.init_line_col, text);
        match p.has_linebreak {
            true => Token::CommonWithLineEnding(token),
            false => Token::Common(token),
        }
    };

    for ref pair in pairs {
        let (rule, pair_str) = (pair.as_rule(), pair.as_str());
        let pair_len = pair_str.len() as u32;
        let emit_span = || Span::new((line, offset).into(), pair_len);
        let emit_token = || {
            let range = pos..pos + pair_len as usize;
            let text = unsafe { raw_text.get_unchecked(range) };
            RawToken::new((line, offset).into(), text)
        };
        let emit_path_literal = || {
            let path_range = pos + 1..pos + pair_len as usize - 1;
            let path = unsafe { raw_text.get_unchecked(path_range) };
            PathLiteral::new((line, offset).into(), path)
        };
        let uncommon_stmt = matches!(
            rule,
            Rule::IncludeToken | Rule::IncludePath | Rule::RegionOpen | Rule::RegionClose
        );

        if uncommon_stmt && let Some(p) = pending.take() {
            out.push(flush_pending_token(p));
        }

        match rule {
            Rule::LineTerminator | Rule::CommonWithLineEnding if pending.is_some() => {
                if let Some(mut p) = pending.take() {
                    p.has_linebreak = true;
                    p.pending_len += pair_len;
                    out.push(flush_pending_token(p));
                }
            }
            Rule::LineTerminator => out.push(Token::LineTerminator((line, offset).into())),
            Rule::CommonWithLineEnding => out.push(Token::CommonWithLineEnding(emit_token())),
            Rule::IncludeToken => out.push(Token::Include(emit_span())),
            Rule::IncludePath => out.push(Token::IncludePath(emit_path_literal())),
            Rule::RegionOpen => out.push(Token::RegionOpen(emit_span())),
            Rule::RegionClose => out.push(Token::RegionClose(emit_span())),
            _ => match pending {
                Some(ref mut acc) if acc.init_line_col.line == line => acc.pending_len += pair_len,
                _ => pending = Pending::new((line, offset).into(), pos, pair_len, false).into(),
            },
        };

        pos += pair_len as usize;
        match matches!(rule, Rule::LineTerminator | Rule::CommonWithLineEnding) {
            true => (offset = 0, line += 1),
            false => (offset += pair_str.chars().count() as u32, ()),
        };
    }

    if let Some(p) = pending.take() {
        out.push(flush_pending_token(p));
    }

    let end_of_input: LineCol = match out.last() {
        Some(Token::LineTerminator(r)) => (r.line + 1, 0).into(),
        Some(Token::CommonWithLineEnding(r)) => (r.line_col.line + 1, 0).into(),
        Some(Token::RegionClose(r)) => (r.line_col.line, r.line_col.col + r.len).into(),
        Some(Token::Common(r)) => (r.line_col.line, r.line_col.col + r.text.len() as u32).into(),
        Some(Token::IncludePath(r)) => {
            (r.line_col.line, r.line_col.col + r.path.len() as u32 + 2).into()
        }
        None => (0, 0).into(),
        _ => unreachable!(),
    };

    out.push(Token::Eoi(end_of_input));
    out
}
