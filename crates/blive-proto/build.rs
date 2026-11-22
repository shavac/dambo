fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/event.proto");
    let mut config = prost_build::Config::new();
    if std::env::var_os("PROTOC").is_none() {
        let protoc = protoc_bin_vendored::protoc_bin_path()?;
        config.protoc_executable(protoc);
    }
    config.type_attribute(
        ".",
        "#[derive(serde::Serialize, serde::Deserialize)]\n#[serde(default)]",
    );
    config.compile_protos(&["proto/event.proto"], &["proto/"])?;
    Ok(())
}
