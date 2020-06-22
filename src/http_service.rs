use std::net::SocketAddr;
use std::task::Context;
use std::task::Poll;
use std::time::Instant;

use ateles::JsRequest;
use ateles::JsResponse;

use futures_util::future;

use hyper::server::conn::AddrIncoming;
use hyper::service::Service;
use hyper::Body;
use hyper::Method;
use hyper::Request;
use hyper::Response;
use hyper::Server;
use hyper::StatusCode;

use prost::Message;

use crate::js_server::create_js_env;
use crate::js_server::Command;
use crate::js_server::JSClient;
use crate::js_server::Ops;
use crate::js_engine::JSEnv;

pub mod ateles {
    // The string specified here must match the proto package name
    tonic::include_proto!("ateles");
}

impl From<ateles::JsRequest> for Command {
    fn from(js_request: JsRequest) -> Self {
        let op = match js_request.action {
            0 => Ops::REWRITE,
            1 => Ops::EVAL,
            2 => Ops::CALL,
            _ => Ops::EXIT
        };
        Command {
            operation: op,
            payload: js_request.script,
            args: js_request.args
        }
    }
}

#[derive(Clone)]
pub struct Svc {
    js_client: JSClient
}

impl Svc {
    pub async fn handle_resp(
        &mut self,
        req: Request<Body>
    ) -> Result<Response<Body>, hyper::Error> {
        match (req.method(), req.uri().path()) {
            (&Method::GET, "/") => Ok(Response::new(Body::from(
                "HELLO Ateles on Rust with V8!!!!"
            ))),
            (&Method::GET, "/Health") => Ok(Response::new(Body::from("OK"))),
            (&Method::POST, "/Ateles/Execute") => {
                let start = Instant::now();

                let full_body = hyper::body::to_bytes(req.into_body()).await?;
                let js_request = JsRequest::decode(full_body).unwrap();
                let cmd: Command = js_request.clone().into();
                let resp = self.js_client.run(js_request.into());
                let js_resp = JsResponse {
                    status: 0,
                    result: resp
                };

                let mut resp: Vec<u8> = Vec::new();
                js_resp.encode(&mut resp).unwrap();
                println!(
                    "request {:?} took {:?}",
                    cmd.operation,
                    start.elapsed()
                );
                Ok(Response::new(Body::from(resp)))
            }
            _ => {
                let mut not_found = Response::default();
                *not_found.status_mut() = StatusCode::NOT_FOUND;
                Ok(not_found)
            }
        }
    }
}

impl Service<Request<Body>> for Svc {
    type Response = Response<Body>;
    type Error = hyper::Error;
    type Future =
        future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut Context<'_>
    ) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let mut me = self.clone();
        let fut = async move { me.handle_resp(req).await };
        Box::pin(fut)
    }
}

pub struct MakeService {
    js_env: JSEnv
}

impl MakeService {
    pub fn new() -> MakeService {
        MakeService {
            js_env: JSEnv::new()
        }
    }
}

impl<T> Service<T> for MakeService {
    type Response = Svc;
    type Error = std::io::Error;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, _: T) -> Self::Future {
        let svc = Svc {
            js_client: create_js_env(&self.js_env)
        };
        future::ok(svc)
    }
}

pub fn create_server(addr: &SocketAddr) -> Server<AddrIncoming, MakeService> {
    Server::bind(&addr).serve(MakeService::new())
}
