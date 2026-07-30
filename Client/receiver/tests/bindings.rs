use std::path::Path;

#[test]
#[cfg_attr(miri, ignore)]
#[ignore = "Use the generate-bindings binary; this test is left for manual runs."]
fn bindings_csharp() -> Result<(), Box<dyn std::error::Error>> {
    pc_receiver::bindings_generation::generate_csharp_bindings(Path::new(
        "bindings/csharp/ReceiverInterop.cs",
    ))?;
    Ok(())
}
