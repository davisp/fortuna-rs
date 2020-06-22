extern crate pretty_env_logger;
#[macro_use] extern crate log;

pub mod js;
pub mod server;

pub use crate::server::run_server;
pub use js::init as init_v8;
