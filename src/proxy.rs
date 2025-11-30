use futures::future::BoxFuture;
use std::sync::{Arc, OnceLock};
use tower::ServiceBuilder;

use async_lsp::lsp_types::request::Request;
use async_lsp::router::Router;
use async_lsp::{ClientSocket, ResponseError, ServerSocket};

use crate::forward::{ForwardingLayer, TService};
use crate::state::State;

pub const JS_LANG_ID: &'static str = "javascript";

pub type ResFut<R> = BoxFuture<'static, Result<<R as Request>::Result, ResponseError>>;
pub type ResReq<R> = Result<<R as Request>::Result, async_lsp::Error>;

#[derive(Default, Clone)]
pub struct Proxy {
    client: Arc<OnceLock<ClientSocket>>,
    server: Arc<OnceLock<ServerSocket>>,
    pub state: Arc<State>,
}

impl Proxy {
    pub fn server(&self) -> ServerSocket {
        self.server.get().unwrap().clone()
    }

    pub fn client(&self) -> ClientSocket {
        self.client.get().unwrap().clone()
    }

    pub fn init(
        server: Arc<OnceLock<ServerSocket>>,
        client: Arc<OnceLock<ClientSocket>>,
    ) -> (impl TService<Future: Send>, impl TService<Future: Send>) {
        let proxy = Self {
            server,
            client,
            state: Arc::new(State::default()),
        };
        let sr = Router::from_language_server(proxy.clone());
        let cr = Router::from_language_client(proxy);
        let server = ServiceBuilder::new().layer(ForwardingLayer).service(sr);
        let client = ServiceBuilder::new().layer(ForwardingLayer).service(cr);

        (server, client)
    }
}
