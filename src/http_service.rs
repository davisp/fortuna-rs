use std::net::SocketAddr;
use std::time::Instant;

use ateles::JsRequest;
use ateles::JsResponse;

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};

use prost::Message;

use crate::js_server::create_js_env;
use crate::js_server::Command;
use crate::js_server::JSClient;
use crate::js_server::Ops;
use crate::js_engine::JSEnv;

type GenericError = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, GenericError>;

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


async fn serve(client: JSClient, req: Request<Body>) -> Result<Response<Body>> {
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
            let resp = client.run(js_request.into());
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

pub async fn run_server(addr: &SocketAddr) -> std::result::Result<(), hyper::Error> {
    let jsenv = JSEnv::new();

    let make_service = make_service_fn(move |_| {
        let svc_jsenv = jsenv.clone();
        let client = create_js_env(&svc_jsenv);

        async move {
            Ok::<_, GenericError>(service_fn(move |req| {
                serve(client.clone(), req)
            }))
        }
    });

    let server = Server::bind(&addr).serve(make_service);
    println!("Listening on http://{}", addr);
    server.await
}
