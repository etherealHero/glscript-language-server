use async_lsp::lsp_types::Url as Uri;
use derive_more::Constructor;
use std::collections::HashSet;

use crate::builder::PatternSources;
use crate::builder::source_map_builder::SourceMapBuilder;
use crate::state::State;
use crate::types::{SourceHash, SourcePattern};

mod content;
mod dev;
mod prepare;
mod source_map;

#[cfg(debug_assertions)]
pub use dev::emit_on_disk;

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
