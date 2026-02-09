use async_lsp::lsp_types::Url as Uri;

use crate::builder::emit::{Context, Emit};
use crate::parser::Token;

impl Emit {
    /// called before parallel content & source_map emit tasks for sync sources
    pub fn prepare_par_iter(st: &mut Emit, ctx: &mut Context, target: &Uri) {
        let d = match ctx.proxy_state.get_doc(target) {
            Ok(doc) => doc,
            Err(_) => return,
        };
        let (path, tokens) = (&d.path, d.parse.compressed_tokens.iter());
        match ctx.visited_sources.contains(&d.source_hash) {
            true => return,
            false => ctx.visited_sources.insert(d.source_hash),
        };
        Emit::prepare_par_iter(st, ctx, ctx.defult_document);
        st.line_break(); // < DocumentDeclarationStatement
        st.line_break(); // <
        let mut lt_ro_skip = false;
        for t in tokens {
            match t {
                Token::IncludePath(t) => {
                    let dep_path = ctx.proxy_state.path_resolver(path, t.lit);
                    let dep_uri = ctx.proxy_state.path_to_uri(&dep_path);
                    let doc_uri = if let Ok(uri) = dep_uri {
                        match ctx.proxy_state.get_doc(&uri).is_ok() {
                            true => Some(uri),
                            false => None,
                        }
                    } else {
                        None
                    };

                    st.line_break(); // < DocumentLinkStatement
                    st.line_break(); // <

                    if let Some(target) = doc_uri {
                        Emit::prepare_par_iter(st, ctx, &target);
                    }

                    st.line_break(); // traling statements after include path on current line
                }
                Token::RegionOpen(_) => lt_ro_skip = true,
                Token::LineTerminator(_) if lt_ro_skip => lt_ro_skip = false,
                Token::LineTerminator(_) | Token::CommonWithLineEnding(_) => st.line_break(),
                _ => {}
            }
        }
    }
}
