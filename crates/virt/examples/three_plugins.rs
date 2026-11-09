mod support;

#[tokio::main]
async fn main() -> virt_protocol::Result<()> {
    support::scenario().await?;
    println!(
        "PASS: dependency ordering, service replacement/restoration, events, dynamic unload and resource cleanup"
    );
    Ok(())
}
