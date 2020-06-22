use std::convert::TryFrom;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc,
    Mutex
};
use std::task::{
    Context,
    Poll,
    Waker
};
use std::thread;

use crossbeam::crossbeam_channel as cbc;
use rusty_v8 as v8;

use crate::js::ateles::JsRequest;

pub mod ateles {
    // The string specified here must match the proto package name
    tonic::include_proto!("ateles");
}

// This is created in build.rs and is all the required js code added into
// a byte array
include!(concat!(env!("OUT_DIR"), "/js_code.rs"));

#[derive(Clone, Debug)]
pub enum Ops {
    REWRITE,
    EVAL,
    CALL,
    EXIT
}

#[derive(Clone, Debug)]
pub struct JSCommand {
    pub operation: Ops,
    pub payload: String,
    pub args: Vec<String>
}

#[derive(Debug)]
pub enum JSResult {
    Waiting,
    Ok(String),
    Error(String)
}

pub struct JSFutureState {
    cmd: JSCommand,
    result: JSResult,
    waker: Option<Waker>
}

#[derive(Clone)]
pub struct JSFuture {
    state: Arc<Mutex<JSFutureState>>
}

pub struct FortunaIsolate {
    isolate: v8::OwnedIsolate,
    global_context: v8::Global<v8::Context>
}

#[derive(Clone, Default)]
pub struct JSEnv {
    pub startup_data: Vec<u8>
}

struct JSServer {
    receiver: cbc::Receiver<JSFuture>,
    isolate: FortunaIsolate
}

#[derive(Clone)]
pub struct JSClient {
    pub sender: cbc::Sender<JSFuture>
}

pub fn init() {
    let platform = v8::new_default_platform().unwrap();
    v8::V8::initialize_platform(platform);
    v8::V8::initialize();
}

impl From<ateles::JsRequest> for JSCommand {
    fn from(js_request: JsRequest) -> Self {
        let op = match js_request.action {
            0 => Ops::REWRITE,
            1 => Ops::EVAL,
            2 => Ops::CALL,
            _ => Ops::EXIT
        };
        JSCommand {
            operation: op,
            payload: js_request.script,
            args: js_request.args
        }
    }
}

impl JSFuture {
    pub fn new(cmd: JSCommand) -> Self {
        let state = Arc::new(Mutex::new(JSFutureState {
            cmd,
            result: JSResult::Waiting,
            waker: None
        }));

        JSFuture { state }
    }
}

impl Future for JSFuture {
    type Output = JSResult;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.state.lock().unwrap();
        match &state.result {
            JSResult::Ok(data) => Poll::Ready(JSResult::Ok(data.clone())),
            JSResult::Error(data) => Poll::Ready(JSResult::Error(data.clone())),
            JSResult::Waiting => {
                state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl JSEnv {
    pub fn new() -> Self {
        let startup_data = JSEnv::create_startup_data();
        JSEnv {
            startup_data: startup_data.to_vec()
        }
    }

    // adapted from Deno https://github.com/denoland/rusty_v8/blob/master/tests/test_api.rs#L1714
    fn create_startup_data() -> v8::StartupData {
        let mut snapshot_creator = v8::SnapshotCreator::new(None);
        {
            // TODO(ry) this shouldn't be necessary. workaround unfinished business in
            // the scope type system.
            let mut isolate = unsafe { snapshot_creator.get_owned_isolate() };

            let mut hs = v8::HandleScope::new(&mut isolate);
            let scope = hs.enter();

            let context = v8::Context::new(scope);
            let mut cs = v8::ContextScope::new(scope, context);
            let scope = cs.enter();
            let source = v8::String::new(scope, JS_CODE).unwrap();
            let mut script =
                v8::Script::compile(scope, context, source, None).unwrap();
            script.run(scope, context).unwrap();

            snapshot_creator.set_default_context(context);
            std::mem::forget(isolate); // TODO(ry) this shouldn't be necessary.
        }

        snapshot_creator
            .create_blob(v8::FunctionCodeHandling::Clear)
            .unwrap()
    }
}

impl FortunaIsolate {
    pub fn new_from_snapshot(data: &[u8]) -> FortunaIsolate {
        FortunaIsolate::create_isolate(data.to_vec())
    }

    fn create_isolate(startup_data: Vec<u8>) -> FortunaIsolate {
        let mut global_context = v8::Global::<v8::Context>::new();
        let create_params =
            v8::Isolate::create_params().snapshot_blob(startup_data);
        let mut isolate = v8::Isolate::new(create_params);

        let mut handle_scope = v8::HandleScope::new(&mut isolate);
        let scope = handle_scope.enter();

        let context = v8::Context::new(scope);

        global_context.set(scope, context);

        FortunaIsolate {
            isolate,
            global_context
        }
    }

    pub fn eval(&mut self, script_str: &str, _args: &[String]) -> String {
        let mut hs = v8::HandleScope::new(&mut self.isolate);
        let scope = hs.enter();
        let context = self.global_context.get(scope).unwrap();

        let mut cs = v8::ContextScope::new(scope, context);
        let scope = cs.enter();

        let source = v8::String::new(scope, script_str).unwrap();
        let mut script =
            v8::Script::compile(scope, context, source, None).unwrap();

        let result = script.run(scope, context).unwrap();
        let result = v8::json::stringify(context, result).unwrap();
        let result = result.to_rust_string_lossy(scope);

        if result == "undefined" {
            return "null".to_string();
        }

        result
    }

    pub fn call(&mut self, raw_fun_name: &str, args: &[String]) -> String {
        let mut hs = v8::HandleScope::new(&mut self.isolate);
        let scope = hs.enter();

        let context = self.global_context.get(scope).unwrap();
        let mut cs = v8::ContextScope::new(scope, context);
        let scope = cs.enter();

        let global = context.global(scope);

        let name = v8::String::new(scope, raw_fun_name).unwrap();
        let func = global.get(scope, context, name.into()).unwrap();
        let func = v8::Local::<v8::Function>::try_from(func).unwrap();
        let receiver = context.global(scope);

        let args: Vec<v8::Local<v8::Value>> = args
            .iter()
            .map(|arg| {
                let arg = v8::String::new(scope, arg).unwrap();
                v8::Local::<v8::Value>::try_from(arg).unwrap()
            })
            .collect();

        let resp = func
            .call(scope, context, receiver.into(), args.as_slice())
            .unwrap();

        let result = v8::json::stringify(context, resp).unwrap();

        result.to_rust_string_lossy(scope)
    }
}

impl JSServer {
    fn create(js_env: &JSEnv, receiver: cbc::Receiver<JSFuture>) {
        let data = js_env.startup_data.clone();
        thread::spawn(move || {
            let mut server = JSServer {
                receiver,
                isolate: FortunaIsolate::new_from_snapshot(data.as_slice())
            };

            server.run()
        });
    }

    fn run(&mut self) {
        while let Ok(fut) = self.receiver.recv() {
            let mut state = fut.state.lock().unwrap();
            state.result = self.process(state.cmd.clone());
            if let Some(waker) = state.waker.take() {
                waker.wake()
            }
        }
    }

    fn process(&mut self, cmd: JSCommand) -> JSResult {
        match cmd.operation {
            Ops::EXIT => JSResult::Error(String::from("exiting")),
            Ops::EVAL => self.eval(cmd.payload),
            Ops::CALL => self.call(cmd.payload, cmd.args.as_slice()),
            Ops::REWRITE => self.call(cmd.payload, cmd.args.as_slice())
        }
    }

    fn eval(&mut self, script: String) -> JSResult {
        let resp = self.isolate.eval(script.as_str(), &[]);
        JSResult::Ok(resp)
    }

    fn call(&mut self, fun_name: String, args: &[String]) -> JSResult {
        let resp = self.isolate.call(fun_name.as_str(), args);
        JSResult::Ok(resp)
    }
}

impl JSClient {
    pub fn new(js_env: &JSEnv) -> Self {
        let (sender, receiver) = cbc::unbounded();
        JSServer::create(js_env, receiver);
        JSClient { sender }
    }

    pub fn run(&self, cmd: JSCommand) -> JSFuture {
        let fut = JSFuture::new(cmd);
        self.sender.send(fut.clone()).unwrap();
        fut
    }
}
