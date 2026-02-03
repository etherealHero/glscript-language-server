use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_lsp::lsp_types::Url as Uri;

use crate::proxy::Canonicalize;
use crate::state::State;

impl State {
    /// returns canonicalized [`PathBuf`]
    #[inline]
    pub fn uri_to_path(&self, uri: &Uri) -> anyhow::Result<PathBuf> {
        if let Some(canonicalized_path) = self.uri_to_canonicalized_path.get(uri) {
            return Ok(canonicalized_path.clone());
        }

        let path = uri.to_file_path();
        let path = path.map_err(|_| anyhow::anyhow!("uri to file path fail: {uri}"))?;
        let canonicalized_path = dunce::canonicalize(dunce::simplified(&path))?;

        self.uri_to_canonicalized_path
            .insert(uri.clone(), canonicalized_path.clone());

        Ok(canonicalized_path)
    }

    /// returns canonicalized [`Uri`]
    #[inline]
    pub fn path_to_uri(&self, path: &Path) -> anyhow::Result<Uri> {
        if let Some(canonicalized_uri) = self.path_to_canonicalized_uri.get(path) {
            return Ok(canonicalized_uri.clone());
        }

        let canonicalized_path = &dunce::canonicalize(dunce::simplified(path))?;
        let uri = Uri::from_file_path(canonicalized_path);
        let uri = uri.map_err(|_| anyhow::anyhow!("path to uri fail: {path:?}"))?;
        let canonicalized_uri = uri.canonicalize()?;

        self.path_to_canonicalized_uri
            .insert(path.to_path_buf(), canonicalized_uri.clone());

        Ok(canonicalized_uri)
    }

    pub fn path_resolver(&self, path_from: &Path, path_literal: &str) -> Arc<PathBuf> {
        let key = (path_from.into(), path_literal.to_string());
        if let Some(resolved_path) = self.path_resolver_cache.get(&key) {
            return resolved_path.clone();
        }

        let is_relative = |path: &str| {
            path.starts_with("./")
                || path.starts_with(".\\")
                || path.starts_with("../")
                || path.starts_with("..\\")
        };

        #[allow(clippy::unit_arg)]
        let normilize = |path: &Path| {
            let mut buf = PathBuf::new();
            for component in path.components() {
                match component {
                    Component::ParentDir => buf.pop().eq(&false).then(|| buf.push("..")),
                    Component::CurDir => None,
                    _ => buf.push(component.as_os_str()).into(),
                };
            }
            buf
        };

        let path = path_literal.replace("\\\\", "/").replace("\\", "/");
        let resolved_path: Arc<PathBuf> = match is_relative(&path) {
            true => normilize(&path_from.parent().unwrap().join(path)).into(),
            false => normilize(&self.get_project().join(path)).into(),
        };

        self.path_resolver_cache.insert(key, resolved_path.clone());
        resolved_path
    }
}
