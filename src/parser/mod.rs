use derive_more::Constructor;
use entry::{Rule, find_interpolations, get_pairs};
use tokens::{Pending, RawToken, Span, StringLiteral};

pub use tokens::{LineCol, Token};

mod entry;
mod tokens;

#[derive(Constructor, Debug, Default)]
pub struct Parse<'a> {
    pub compressed_tokens: Vec<Token<'a>>,
    pub str_interpolations: Vec<LineCol>, // TODO:
}

pub fn parse<'a>(raw_text: &'a str) -> Parse<'a> {
    let raw_text_ptr = raw_text.as_ptr() as usize;
    let pairs = get_pairs(raw_text);
    let (mut line, mut offset, mut pending) = (0, 0, None::<Pending>);
    let mut out = Vec::with_capacity(raw_text.lines().count());
    let mut str_i = vec![];

    for ref pair in pairs {
        let (rule, pair_str) = (pair.as_rule(), pair.as_str());
        let pair_len = pair_str.len() as u32;
        let pos = unsafe { (pair_str.as_ptr() as usize).unchecked_sub(raw_text_ptr) };
        let lc = || LineCol { line, col: offset };

        let emit_span = || Span::new(lc(), pair_len);
        let emit_token = || RawToken::new(lc(), &raw_text[pos..pos + pair_len as usize]);
        let emit_sl = || StringLiteral::new(lc(), &raw_text[pos + 1..pos + pair_len as usize - 1]);

        let pend_common =
            |p: &mut Option<Pending>, str_i: &mut Vec<LineCol>, t: Option<(LineCol, &str)>| {
                if let Some((lc, text)) = t {
                    for i in find_interpolations(text) {
                        str_i.push((lc.line, lc.col + i).into())
                    }
                }

                match p {
                    Some(acc) if acc.init_line_col.line == line => acc.pending_len += pair_len,
                    _ => *p = Pending::new(lc(), pos, pair_len, false).into(),
                }
            };

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
            Rule::IncludePath => out.push(Token::IncludePath(emit_sl())),
            Rule::RegionOpen => out.push(Token::RegionOpen(emit_span())),
            Rule::RegionClose => out.push(Token::RegionClose(emit_span())),
            // common arms:
            Rule::RegionChars | Rule::TemplateStringChars => {
                let RawToken { line_col, text } = emit_token();
                pend_common(&mut pending, &mut str_i, (line_col, text).into())
            }
            Rule::DoubleStringLiteral | Rule::SingleStringLiteral => {
                let StringLiteral { line_col, lit } = emit_sl();
                let lit_line_col = (line_col.line, line_col.col + 1).into();
                pend_common(&mut pending, &mut str_i, (lit_line_col, lit).into())
            }
            _ => pend_common(&mut pending, &mut str_i, None),
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
            (r.line_col.line, r.line_col.col + r.lit.len() as u32 + 2).into()
        }
        None => (0, 0).into(),
        _ => unreachable!(),
    };

    out.push(Token::Eoi(end_of_input));
    Parse::new(out, str_i)
}
