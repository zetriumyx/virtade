fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    anyhow::ensure!(
        args.len() == 2,
        "virt-componentize <core.wasm> <component.wasm>"
    );
    let bytes = std::fs::read(&args[0])?;
    let component = wit_component::ComponentEncoder::default()
        .module(&bytes)?
        .validate(true)
        .encode()?;
    std::fs::write(&args[1], component)?;
    Ok(())
}
