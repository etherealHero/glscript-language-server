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

pub const JS_LANG_ID: &'static str = "javascript";
pub const DECL_FILE_EXT: &'static str = ".d.ts";
pub const PROXY_WORKSPACE: &'static str = "./.local/gls-proxy-workspace";

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
