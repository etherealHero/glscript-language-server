#[macro_export]
macro_rules! try_ensure_bundle {
    (
        $self:expr,
        $uri:expr,
        $params:expr,
        $method:ident
    ) => {{
        if let Some(bundle) = $self.state.get_bundle($uri) {
            $self.state.commit_changes($uri, &mut $self.server());
            bundle
        } else {
            use $crate::proxy::language_server::Error;
            tracing::warn!("{}", Error::unbuild_fallback());
            let mut service = $self.server();
            return Box::pin(
                async move { service.$method($params).await.map_err(Error::internal) },
            );
        }
    }};
}

#[macro_export]
macro_rules! try_ensure_transpile {
    (
        $self:expr,
        $uri:expr,
        $params:expr,
        $method:ident
    ) => {{
        if let Some(transpile) = $self.state.get_transpile($uri) {
            $self.state.commit_changes($uri, &mut $self.server());
            transpile
        } else {
            use $crate::proxy::language_server::Error;
            tracing::warn!("{}", Error::unbuild_fallback());
            let mut service = $self.server();
            return Box::pin(
                async move { service.$method($params).await.map_err(Error::internal) },
            );
        }
    }};
}

#[macro_export]
macro_rules! try_forward_text_document_position_params {
    (
        $state:expr,
        $build:expr,
        $text_document_position_params:expr
    ) => {{
        let uri = &mut $text_document_position_params.text_document.uri;
        let pos = &mut $text_document_position_params.position;
        let source = $state.get_doc(uri).unwrap().source.clone();

        if let Some(build_pos) = $build.forward_src_position(pos, &source) {
            *pos = build_pos;
            *uri = $build.uri.clone();
        } else {
            use $crate::proxy::language_server::Error;
            return Err(Error::forward_failed());
        };
    }};
}
