//! The sample server for the QEMU test tile: the real engine with made-up data on 127.0.0.1:8932, until it is killed.

use std::time::Duration;

use lucina_notebooks_sample::{SAMPLE_PORT, serve};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let sample = serve(SAMPLE_PORT, Duration::from_millis(25), true).await?;
    eprintln!("lucina-notebooks-sample: ready on {}", sample.addr);
    std::future::pending::<()>().await;
    Ok(())
}
