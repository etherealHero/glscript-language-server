use futures::future::BoxFuture;
use std::sync::{Arc, OnceLock};
use tower::ServiceBuilder;

use async_lsp::lsp_types::Url as Uri;
use async_lsp::lsp_types::request::Request;
use async_lsp::router::Router;
use async_lsp::{ClientSocket, ResponseError, ServerSocket};
use derive_more::Constructor;

use crate::forward::{ForwardingLayer, TService};
use crate::state::State;

pub const JS_LANG_ID: &str = "javascript";
pub const JS_FILE_EXT: &str = ".js";
pub const DECL_FILE_EXT: &str = ".d.ts";
pub const PROXY_WORKSPACE: &str = "./.local/gls-proxy-workspace";
pub const DEFAULT_TIMEOUT_MS: u64 = 5000;

pub type ResFut<R> = BoxFuture<'static, Result<<R as Request>::Result, ResponseError>>;
pub type ResReq<R> = Result<<R as Request>::Result, async_lsp::Error>;
pub type ResReqProxy<R> = Result<<R as Request>::Result, ResponseError>;

pub trait Canonicalize {
    fn canonicalize(&self) -> Self;
}

impl Canonicalize for Uri {
    fn canonicalize(&self) -> Self {
        Uri::from_file_path(self.to_file_path().unwrap()).unwrap()
    }
}

#[derive(Default, Clone, Constructor)]
pub struct Proxy {
    client: Arc<OnceLock<ClientSocket>>,
    server: Arc<OnceLock<ServerSocket>>,
    pub state: Arc<State>,
}

impl Proxy {
    pub fn server(&self) -> ServerSocket {
        self.server.get().expect("server socket linked").clone()
    }

    pub fn client(&self) -> ClientSocket {
        self.client.get().expect("client socket linked").clone()
    }

    pub fn init(
        server: Arc<OnceLock<ServerSocket>>,
        client: Arc<OnceLock<ClientSocket>>,
    ) -> (impl TService<Future: Send>, impl TService<Future: Send>) {
        let proxy = Self::new(client, server, Arc::new(State::default()));
        let sr = Router::from_language_server(proxy.clone());
        let cr = Router::from_language_client(proxy);
        let server;
        let client;

        #[cfg(debug_assertions)]
        {
            server = ServiceBuilder::new()
                .layer(ForwardingLayer)
                // .layer(async_lsp::tracing::TracingLayer::default())
                .service(sr);
            client = ServiceBuilder::new()
                .layer(ForwardingLayer)
                // .layer(async_lsp::tracing::TracingLayer::default())
                .service(cr);
        }

        #[cfg(not(debug_assertions))]
        {
            server = ServiceBuilder::new().layer(ForwardingLayer).service(sr);
            client = ServiceBuilder::new().layer(ForwardingLayer).service(cr);
        }

        (server, client)
    }
}

#[macro_export]
macro_rules! try_ensure_build {
    (
        $self:expr,
        $uri:expr,
        $params:expr,
        $method:ident
    ) => {{
        if let Some(build) = $self.state.get_build($uri) {
            $self.state.commit_build_changes($uri, &mut $self.server());
            build
        } else {
            let mut service = $self.server();
            return Box::pin(async move {
                let res = service.$method($params).await;
                res.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
            });
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
            let err = format!("Forward src position `{pos:?}` failed");
            return Err(ResponseError::new(ErrorCode::REQUEST_FAILED, err));
        };
    }};
}
