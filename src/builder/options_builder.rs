use async_lsp::lsp_types::Url as Uri;
use derive_more::Constructor;
use std::sync::Arc;

use crate::builder::{Build, PatternSources};
use crate::state::State;
use crate::types::SourcePattern;

#[derive(Constructor)]
pub struct BuildOptionsBuilder<'a> {
    pub(in crate::builder) options: BuildOptions<'a>,
}

impl<'a> BuildOptionsBuilder<'a> {
    pub fn init(uri: &'a Uri, st: &'a State) -> Self {
        Self::new(BuildOptions::new(st, uri, None, None, None, true))
    }

    pub fn with_previous_build(mut self, pb: Arc<Build>) -> Self {
        self.options.pb = Some(pb);
        self
    }

    pub fn with_source_pattern(mut self, pat: SourcePattern<'a>) -> Self {
        self.options.pat = Some(pat);
        self
    }

    pub fn transpile_mode(mut self) -> Self {
        self.options.resolve_deps = false;
        self
    }

    pub fn target(&self) -> &'a Uri {
        self.options.uri
    }
}

#[derive(Constructor)]
pub(in crate::builder) struct BuildOptions<'a> {
    pub st: &'a State,
    pub uri: &'a Uri,
    pub pb: Option<Arc<Build>>,
    pub pat: Option<SourcePattern<'a>>,
    pub pat_sources: Option<PatternSources>,
    pub resolve_deps: bool,
}
