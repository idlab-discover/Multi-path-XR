use interoptopus::util::NamespaceMappings;
use interoptopus::writer::IndentWriter;
use interoptopus::Interop;
use interoptopus_backend_csharp::overloads::DotNet;
use interoptopus_backend_csharp::{Config, Generator};
use std::fs;
use std::path::Path;

/// Generate the C# bindings file. Returns `true` if the file was rewritten.
pub fn generate_csharp_bindings<P: AsRef<Path>>(
    out_file: P,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut generator = Generator::new(
        Config {
            class: "ReceiverInteropClass".to_string(),
            dll_name: "libpc_receiver".to_string(),
            namespace_mappings: NamespaceMappings::new("Be.Ugent"),
            ..Config::default()
        },
        crate::build_binding_inventory(),
    );

    generator.add_overload_writer(DotNet::new());
    //generator.add_overload_writer(Unity::new());

    // Render into memory first so we can avoid touching the file if nothing changed.
    let mut buffer = Vec::new();
    {
        let mut writer = IndentWriter::new(&mut buffer);
        generator.write_to(&mut writer)?;
    }

    let new_contents = String::from_utf8(buffer)?;
    let out_path = out_file.as_ref();

    let unchanged = fs::read_to_string(out_path)
        .map(|existing| existing == new_contents)
        .unwrap_or(false);

    if unchanged {
        return Ok(false);
    }

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(out_path, new_contents)?;

    Ok(true)
}
