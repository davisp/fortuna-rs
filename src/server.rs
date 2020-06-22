use std::net::SocketAddr;
use std::time::Instant;

use crate::js::ateles::JsRequest;
use crate::js::ateles::JsResponse;

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};

use prost::Message;

use crate::js::JSClient;
use crate::js::JSCommand;
use crate::js::JSEnv;
use crate::js::JSResult;

type GenericError = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, GenericError>;

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
            let cmd: JSCommand = js_request.clone().into();
            let result = client.run(cmd.clone()).await;
            let js_resp = match result {
                JSResult::Ok(result) => JsResponse { status: 0, result },
                JSResult::Err(result) => {
                    JsResponse { status: 1, result }
                }
            };

            let mut resp: Vec<u8> = Vec::new();
            js_resp.encode(&mut resp).unwrap();
            println!("request {:?} took {:?}", cmd.operation, start.elapsed());
            Ok(Response::new(Body::from(resp)))
        }
        _ => {
            let mut not_found = Response::default();
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}

pub async fn run_server(
    addr: &SocketAddr
) -> std::result::Result<(), hyper::Error> {
    let jsenv = JSEnv::new();

    let make_service = make_service_fn(move |_| {
        let jsenv = jsenv.clone();
        let client = JSClient::new(&jsenv);

        async move {
            Ok::<_, GenericError>(service_fn(move |req| {
                serve(client.clone(), req)
            }))
        }
    });

    let server = Server::bind(&addr).serve(make_service);
    info!("Listening on http://{}", addr);
    server.await
}
