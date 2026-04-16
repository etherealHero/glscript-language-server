use async_lsp::lsp_types::Url as Uri;
use derive_more::Constructor;
use std::collections::{HashMap, HashSet};

use crate::builder::PatternSources;
use crate::builder::source_map_builder::SourceMapBuilder;
use crate::parser::LineCol;
use crate::state::State;
use crate::types::{Source, SourceHash, SourcePattern};

mod content;
mod dev;
mod prepare;
mod source_map;

#[cfg(debug_assertions)]
pub use dev::emit_on_disk;

#[allow(clippy::too_many_arguments)]
#[derive(Constructor)]
pub struct Context<'a> {
    proxy_state: &'a State,
    defult_document: &'a Uri,
    visited_sources: HashSet<SourceHash>,
    pat: Option<SourcePattern<'a>>,
    pat_sources: Option<PatternSources>,
    resolve_deps: bool,
    is_default_context: bool,
    stack: Stack,
}

pub enum Emit {
    WithSourceMapBuilderAndDstLine(SourceMapBuilder, u32, HashMap<Source, Stack>),
    WithDstContent(String, Option<PatternSources>),
}

/// Stack sequence of [`Source`]'s with [`crate::parser::Token::IncludePath`] position and
/// literal length where source were been included by prev source
pub type Stack = Vec<(Source, LineCol, usize)>;

pub enum EmitResult {
    TokensCountAndSourceMap(usize, sourcemap::SourceMap, HashMap<Source, Stack>),
    Content(String, Option<PatternSources>),
}

impl Emit {
    pub fn finish(self, state: &State) -> EmitResult {
        match self {
            Emit::WithDstContent(dst_content, pattern_sources) => {
                EmitResult::Content(dst_content, pattern_sources)
            }
            Emit::WithSourceMapBuilderAndDstLine(builder, _, sources_stack) => {
                EmitResult::TokensCountAndSourceMap(
                    builder.tokens.len(),
                    builder.into_sourcemap(state),
                    sources_stack,
                )
            }
        }
    }
}
