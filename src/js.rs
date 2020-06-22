use std::convert::TryFrom;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
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

pub type JSResult = Result<String, String>;

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

pub struct JSFutureState {
    cmd: JSCommand,
    completed: bool,
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
            completed: false,
            result: Result::Err(String::from("<waiting>")),
            waker: None
        }));

        JSFuture { state }
    }
}

impl Future for JSFuture {
    type Output = JSResult;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.state.lock().unwrap();
        if state.completed {
            Poll::Ready(state.result.clone())
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
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

    // Adapted from Deno:
    // https://github.com/denoland/rusty_v8/blob/master/tests/test_api.rs#L1714
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
    pub fn new_from_snapshot(snapshot: &[u8]) -> Self {
        let mut global_context = v8::Global::<v8::Context>::new();
        let params = v8::Isolate::create_params();
        let params = params.snapshot_blob(snapshot.to_vec());
        let mut isolate = v8::Isolate::new(params);

        let mut scope = v8::HandleScope::new(&mut isolate);
        let scope = scope.enter();

        let context = v8::Context::new(scope);

        global_context.set(scope, context);

        FortunaIsolate {
            isolate,
            global_context
        }
    }

    pub fn eval(
        &mut self,
        script_str: &str,
        _args: &[String]
    ) -> JSResult {
        let mut scope = v8::HandleScope::new(&mut self.isolate);
        let scope = scope.enter();

        let context = self
            .global_context
            .get(scope)
            .ok_or("error getting context")?;

        let mut cs = v8::ContextScope::new(scope, context);
        let scope = cs.enter();

        let source = v8::String::new(scope, script_str)
            .ok_or("error creating string")?;

        let mut script = v8::Script::compile(scope, context, source, None)
            .ok_or("error compiling script")?;

        let result =
            script.run(scope, context).ok_or("error running script")?;
        let result = v8::json::stringify(context, result)
            .ok_or("error encoding result as JSON")?;
        let result = result.to_rust_string_lossy(scope);

        if result == "undefined" {
            return Result::Err("null".to_string());
        }

        Result::Ok(result)
    }

    pub fn call(
        &mut self,
        func_name: &str,
        args: &[String]
    ) -> JSResult {
        let mut scope = v8::HandleScope::new(&mut self.isolate);
        let scope = scope.enter();

        let context = self
            .global_context
            .get(scope)
            .ok_or("error getting context")?;

        let mut scope = v8::ContextScope::new(scope, context);
        let scope = scope.enter();

        let global = context.global(scope);

        let name = v8::String::new(scope, func_name)
            .ok_or("error creating function name string")?;

        let func = global
            .get(scope, context, name.into())
            .ok_or(format!("function '{}' not found", func_name))?;
        let func = v8::Local::<v8::Function>::try_from(func).ok()
            .ok_or(format!("'{}' is not a function", func_name))?;

        let mut js_args: Vec<v8::Local<v8::Value>> = Vec::new();
        js_args.reserve_exact(args.len());
        for arg in args.iter() {
            let arg = v8::String::new(scope, arg).ok_or("error creating argument")?;
            let arg = v8::Local::<v8::Value>::try_from(arg).unwrap();
            js_args.push(arg);
        }

        let resp = func
            .call(scope, context, global.into(), js_args.as_slice())
            .ok_or(format!("error calling '{}'", func_name))?;

        let result = v8::json::stringify(context, resp)
            .ok_or("error converting result to JSON")?;

        Result::Ok(result.to_rust_string_lossy(scope))
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
            state.completed = true;
            state.result = self.process(state.cmd.clone());
            if let Some(waker) = state.waker.take() {
                waker.wake()
            }
        }
    }

    fn process(&mut self, cmd: JSCommand) -> JSResult {
        match cmd.operation {
            Ops::EXIT => Result::Err(String::from("exiting")),
            Ops::EVAL => self.eval(cmd.payload),
            Ops::CALL => self.call(cmd.payload, cmd.args.as_slice()),
            Ops::REWRITE => self.call(cmd.payload, cmd.args.as_slice())
        }
    }

    fn eval(&mut self, script: String) -> JSResult {
        self.isolate.eval(script.as_str(), &[])
    }

    fn call(&mut self, fun_name: String, args: &[String]) -> JSResult {
        self.isolate.call(fun_name.as_str(), args)
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
