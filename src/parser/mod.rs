use derive_more::Constructor;
use entry::{Rule, get_pairs};
use tokens::{PathLiteral, Pending, RawToken, Span};

pub use tokens::{LineCol, Token};

mod entry;
mod tokens;

#[derive(Constructor, Debug, Default)]
pub struct Parse<'a> {
    pub compressed_tokens: Vec<Token<'a>>,
    pub str_lit_injections: Vec<LineCol>, // TODO:
}

pub fn parse<'a>(raw_text: &'a str) -> Parse<'a> {
    let raw_text_ptr = raw_text.as_ptr() as usize;
    let pairs = get_pairs(raw_text);
    let (mut line, mut offset, mut pending) = (0, 0, None::<Pending>);
    let mut out = Vec::with_capacity(raw_text.lines().count());

    for ref pair in pairs {
        let (rule, pair_str) = (pair.as_rule(), pair.as_str());
        let pair_len = pair_str.len() as u32;
        let pos = unsafe { (pair_str.as_ptr() as usize).unchecked_sub(raw_text_ptr) };
        let lc = || LineCol { line, col: offset };

        let emit_span = || Span::new(lc(), pair_len);
        let emit_token = || RawToken::new(lc(), &raw_text[pos..pos + pair_len as usize]);
        let emit_pl = || PathLiteral::new(lc(), &raw_text[pos + 1..pos + pair_len as usize - 1]);

        let uncommon_stmt = matches!(
            rule,
            Rule::IncludeToken | Rule::IncludePath | Rule::RegionOpen | Rule::RegionClose
        );

        if uncommon_stmt && let Some(p) = pending.take() {
            out.push(p.flush(raw_text));
        }

        match rule {
            Rule::LineTerminator | Rule::CommonWithLineEnding if pending.is_some() => {
                if let Some(mut p) = pending.take() {
                    p.has_linebreak = true;
                    p.pending_len += pair_len;
                    out.push(p.flush(raw_text));
                }
            }
            Rule::LineTerminator => out.push(Token::LineTerminator(lc())),
            Rule::CommonWithLineEnding => out.push(Token::CommonWithLineEnding(emit_token())),
            Rule::IncludeToken => out.push(Token::Include(emit_span())),
            Rule::IncludePath => out.push(Token::IncludePath(emit_pl())),
            Rule::RegionOpen => out.push(Token::RegionOpen(emit_span())),
            Rule::RegionClose => out.push(Token::RegionClose(emit_span())),
            _ => match pending {
                Some(ref mut acc) if acc.init_line_col.line == line => acc.pending_len += pair_len,
                _ => pending = Pending::new(lc(), pos, pair_len, false).into(),
            },
        };

        match matches!(rule, Rule::LineTerminator | Rule::CommonWithLineEnding) {
            true => (offset = 0, line += 1),
            false => (offset += pair_str.chars().count() as u32, ()),
        };
    }

    if let Some(p) = pending.take() {
        out.push(p.flush(raw_text));
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
    Parse::new(out, vec![])
}
