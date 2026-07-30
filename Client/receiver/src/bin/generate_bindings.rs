use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_file = PathBuf::from("bindings/csharp/ReceiverInterop.cs");
    let updated = pc_receiver::bindings_generation::generate_csharp_bindings(&out_file)?;

    if updated {
        println!("Updated C# bindings at {}", out_file.display());
    } else {
        println!("C# bindings already up to date at {}", out_file.display());
    }

    Ok(())
}
