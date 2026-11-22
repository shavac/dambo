fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/event.proto");
    let mut config = prost_build::Config::new();
    config.type_attribute(
        ".",
        "#[derive(serde::Serialize, serde::Deserialize)]\n#[serde(default)]",
    );
    config.compile_protos(&["proto/event.proto"], &["proto/"])?;
    Ok(())
}
