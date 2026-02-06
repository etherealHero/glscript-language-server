use async_lsp::lsp_types::Url as Uri;
use derive_more::Constructor;
use std::collections::HashSet;

#[cfg(debug_assertions)]
use crate::builder::options_builder::BuildOptions;

use crate::builder::PatternSources;
use crate::builder::source_map_builder::SourceMapBuilder;
use crate::parser::Token;
use crate::state::State;
use crate::types::{SourceHash, SourcePattern};

mod content;
mod source_map;

#[cfg(debug_assertions)]
pub fn emit_on_disk(
    opt: &BuildOptions<'_>,
    doc: &crate::types::Document,
    source_map: &sourcemap::SourceMap,
    content: &String,
) -> Result<(), anyhow::Error> {
    use crate::{builder::BUILD_FILE_EXT, proxy::PROXY_WORKSPACE};
    use base64::prelude::{BASE64_STANDARD, Engine as _};

    let mut sm_json = Vec::new();
    let _ = source_map.to_writer(&mut sm_json);
    let sm_base64 = BASE64_STANDARD.encode(&sm_json);
    let build = format!(
        "{}\n//# sourceMappingURL=data:application/json;base64,{}",
        &content, sm_base64
    );
    let debug_source = match opt.resolve_deps {
        true => doc.source.to_string() + BUILD_FILE_EXT,
        false => doc.source.to_string() + ".transpiled" + BUILD_FILE_EXT,
    };
    let proxy_ws = opt.st.get_project().join(PROXY_WORKSPACE);
    let debug_filepath = proxy_ws.join("./debug").join(debug_source);
    let mut sourcemap_file = debug_filepath.clone();
    sourcemap_file.add_extension("map");
    std::fs::create_dir_all(debug_filepath.parent().unwrap()).unwrap();
    std::fs::write(debug_filepath.clone(), build).unwrap();
    std::fs::write(sourcemap_file, String::from_utf8(sm_json)?).unwrap();
    Ok(())
}

#[derive(Constructor)]
pub struct Context<'a> {
    proxy_state: &'a State,
    defult_document: &'a Uri,
    visited_sources: HashSet<SourceHash>,
    pat: Option<SourcePattern<'a>>,
    pat_sources: Option<PatternSources>,
    resolve_deps: bool,
    is_default_context: bool,
}

pub enum Emit {
    WithSourceMapBuilderAndDstLine(SourceMapBuilder, u32),
    WithDstContent(String, Option<PatternSources>),
}

pub enum EmitResult {
    TokensCountAndSourceMap(usize, sourcemap::SourceMap),
    Content(String, Option<PatternSources>),
}

impl Emit {
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

    pub fn finish(self, state: &State) -> EmitResult {
        match self {
            Emit::WithDstContent(dst_content, pattern_sources) => {
                EmitResult::Content(dst_content, pattern_sources)
            }
            Emit::WithSourceMapBuilderAndDstLine(b, _) => {
                EmitResult::TokensCountAndSourceMap(b.tokens.len(), b.into_sourcemap(state))
            }
        }
    }
}
