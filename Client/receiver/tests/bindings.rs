use interoptopus::util::NamespaceMappings;
use interoptopus::{Error, Interop};

#[test]
#[cfg_attr(miri, ignore)]
fn bindings_csharp() -> Result<(), Error> {
    use interoptopus_backend_csharp::{Config, Generator};

    Generator::new(
        Config {
            class: "ReceiverInteropClass".to_string(),
            dll_name: "libpc_receiver".to_string(),
            namespace_mappings: NamespaceMappings::new("Be.Ugent"),
            ..Config::default()
        },
        pc_receiver::build_binding_inventory(),
    )
    .write_file("bindings/csharp/ReceiverInterop.cs")?;

    Ok(())
}