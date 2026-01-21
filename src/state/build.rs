use std::path::PathBuf;
use std::sync::Arc;

use async_lsp::lsp_types::Url as Uri;

use crate::builder::Build;
use crate::proxy::Canonicalize;
use crate::state::State;
use crate::types::{BuildWithVersion, Source};

/// State of builds
impl State {
    pub fn set_build(&self, source_uri: &Uri) -> anyhow::Result<BuildWithVersion> {
        let path = &self.uri_to_path(source_uri)?;

        match self.builds.get_mut(path) {
            Some(mut b) => {
                let new_build = Build::create(self, source_uri, Some(b.build.clone()))?;
                b.build = new_build.into();
                b.version += 1;
            }
            None => {
                let new_build = Build::create(self, source_uri, None)?;
                let build_with_version = BuildWithVersion::new(new_build.into(), 1);
                self.builds.insert(path.into(), build_with_version);
            }
        }

        Ok(self.builds.get(path).map(|guard| guard.clone()).unwrap())
    }

    pub fn get_build(&self, source_uri: &Uri) -> Option<Arc<Build>> {
        let path = match self.uri_to_path(source_uri) {
            Ok(p) => p,
            Err(_) => return None,
        };

        self.builds.get(&path).map(|guard| guard.build.clone())
    }

    pub fn remove_build(&self, source_uri: &Uri) {
        let path = &self.uri_to_path(source_uri).unwrap();
        self.builds.remove(path);
        self.uncommitted_build_changes.remove(path);
        self.unforwarded_doc_changes.remove(path);
    }

    pub fn get_build_by_emit_uri(&self, emit_uri: &Uri) -> Option<Arc<Build>> {
        let emit_uri_canonicalized = emit_uri.try_canonicalize();
        self.builds
            .iter()
            .find(|e| e.build.uri.canonicalize().unwrap() == emit_uri_canonicalized)
            .map(|e| e.build.clone())
    }

    /// returns SourcePath for canonicalize interface
    pub fn get_builds_contains_source(&self, source: &Source) -> Vec<PathBuf> {
        self.builds
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
        self.get_build(&default_doc)
            .unwrap_or_else(|| self.set_build(&default_doc).unwrap().build)
            .sources()
            .iter()
            .map(map)
            .collect()
    }
}
