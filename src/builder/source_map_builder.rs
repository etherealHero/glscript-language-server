use std::sync::Arc;

use crate::state::State;
use crate::types::Source;

pub struct SourceMapBuilder {
    pub tokens: Vec<sourcemap::RawToken>,
    sources: Vec<Arc<Source>>,
    source_map: fxhash::FxHashMap<Arc<Source>, u32>,

    #[cfg(debug_assertions)]
    source_contents: Vec<Option<Arc<str>>>,
}

impl SourceMapBuilder {
    pub fn with_capacity(tokens_capacity: usize, sources_capacity: usize) -> Self {
        Self {
            tokens: Vec::with_capacity(tokens_capacity),
            sources: Vec::with_capacity(sources_capacity),
            source_map: fxhash::FxHashMap::default(),

            #[cfg(debug_assertions)]
            source_contents: Vec::with_capacity(sources_capacity),
        }
    }

    pub fn add_source_with_id(&mut self, source: Arc<Source>) -> u32 {
        let count = self.sources.len() as u32;
        let id = *self.source_map.entry(source.clone()).or_insert(count);
        if id == count {
            self.sources.push(source);
        }
        id
    }

    #[allow(unused_mut)]
    pub fn into_sourcemap(mut self, _state: &State) -> sourcemap::SourceMap {
        let contents;

        #[cfg(debug_assertions)]
        {
            let project = _state.get_project();
            if self.sources.len() > self.source_contents.len() {
                self.source_contents.resize(self.sources.len(), None);
            }
            for (id, source) in self.sources.iter().enumerate() {
                let path = source.as_str();
                let doc_uri = _state.path_to_uri(&project.join(path)).unwrap();
                let contents = _state.get_doc(&doc_uri).unwrap().buffer.to_string();
                self.source_contents[id] = Some(contents.into());
            }

            contents = match self.source_contents.is_empty() {
                false => Some(self.source_contents),
                true => None,
            };
        }

        #[cfg(not(debug_assertions))]
        {
            contents = None
        }

        let sources = self.sources.iter();
        let sources = sources.map(|s| s.as_str().into()).collect();

        let mut sm = sourcemap::SourceMap::new(None, self.tokens, vec![], sources, contents);

        sm.set_source_root(None::<Arc<str>>);
        sm.set_debug_id(None);
        sm
    }
}
