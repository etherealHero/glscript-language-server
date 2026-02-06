use std::path::PathBuf;
use std::sync::Arc;

use async_lsp::lsp_types::Url as Uri;

use crate::builder::{Build, BuildOptionsBuilder};
use crate::proxy::Canonicalize;
use crate::state::State;
use crate::types::{BuildWithVersion, Source, SourcePattern};

type TStorage = dashmap::DashMap<PathBuf, BuildWithVersion>;

/// State of builds
impl State {
    pub fn set_bundle(&self, source_uri: &Uri) -> anyhow::Result<BuildWithVersion> {
        let opt = BuildOptionsBuilder::init(source_uri, self);
        self.build(opt, &self.doc_to_bundle)
    }

    pub fn set_transpile(&self, source_uri: &Uri) -> anyhow::Result<BuildWithVersion> {
        let opt = BuildOptionsBuilder::init(source_uri, self).transpile_mode();
        self.build(opt, &self.doc_to_transpile)
    }

    pub fn set_bundle_with_tree_shaking(
        &self,
        source_uri: &Uri,
        pat: SourcePattern,
    ) -> anyhow::Result<BuildWithVersion> {
        let opt = BuildOptionsBuilder::init(source_uri, self).with_source_pattern(pat);
        self.build(opt, &self.doc_to_bundle)
    }

    pub fn get_bundle(&self, source_uri: &Uri) -> Option<Arc<Build>> {
        self.get_build_from_storage(source_uri, &self.doc_to_bundle)
    }

    pub fn get_transpile(&self, source_uri: &Uri) -> Option<Arc<Build>> {
        self.get_build_from_storage(source_uri, &self.doc_to_transpile)
    }

    pub fn remove_bundle(&self, source_uri: &Uri) {
        let path = &self.uri_to_path(source_uri).unwrap();
        self.doc_to_bundle.remove(path);
        self.uncommitted_bundle_changes.remove(path);
        self.unforwarded_doc_changes.remove(path);
    }

    pub fn remove_transpile(&self, source_uri: &Uri) {
        let path = &self.uri_to_path(source_uri).unwrap();
        self.doc_to_transpile.remove(path);
        self.uncommitted_transpile_changes.remove(path);
    }

    pub fn get_build_by_emit_uri(&self, emit_uri: &Uri) -> Option<Arc<Build>> {
        let emit_uri_canonicalized = emit_uri.try_canonicalize();
        let bundle = self
            .doc_to_bundle
            .iter()
            .find(|e| e.build.uri.canonicalize().unwrap() == emit_uri_canonicalized)
            .map(|e| e.build.clone());

        if bundle.is_some() {
            return bundle;
        }

        self.doc_to_transpile
            .iter()
            .find(|e| e.build.uri.canonicalize().unwrap() == emit_uri_canonicalized)
            .map(|e| e.build.clone())
    }

    /// returns SourcePath for canonicalize interface
    pub fn get_bundles_contains_source(&self, source: &Source) -> Vec<PathBuf> {
        self.doc_to_bundle
            .iter()
            .filter(|e| e.value().build.sources().contains(source))
            .map(|e| e.key().clone())
            .collect()
    }

    pub fn get_default_sources(&self) -> Vec<PathBuf> {
        let default_doc = self.get_default_doc();
        let map = |s: &Source| {
            let path = self.get_project().join(s.as_str());
            let uri = self.path_to_uri(&path).unwrap();
            self.uri_to_path(&uri).unwrap()
        };
        self.get_bundle(&default_doc)
            .unwrap_or_else(|| self.set_bundle(&default_doc).unwrap().build)
            .sources()
            .iter()
            .map(map)
            .collect()
    }
}

impl State {
    fn build(
        &self,
        opt: BuildOptionsBuilder,
        storage: &TStorage,
    ) -> anyhow::Result<BuildWithVersion> {
        let path = &self.uri_to_path(opt.target())?;
        let Some(mut cur_build) = storage.get_mut(path) else {
            let new_build = Build::create(opt)?;
            let build_with_version = BuildWithVersion::new(new_build.into(), 1);
            storage.insert(path.into(), build_with_version.clone());
            return Ok(build_with_version);
        };

        let new_build = Build::create(opt.with_previous_build(cur_build.build.clone()))?;

        cur_build.build = new_build.into();
        cur_build.version += 1;

        Ok(cur_build.clone())
    }

    fn get_build_from_storage(&self, source_uri: &Uri, storage: &TStorage) -> Option<Arc<Build>> {
        let path = self.uri_to_path(source_uri).ok()?;
        storage.get(&path).map(|guard| guard.build.clone())
    }
}
