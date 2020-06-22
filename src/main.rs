use fortuna::{init_v8, run_server};

#[tokio::main(core_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();

    init_v8();
    let addr = "127.0.0.1:8444".parse()?;
    run_server(&addr).await?;

    Ok(())
}
