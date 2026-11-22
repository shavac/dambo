fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=src/proto/event.proto");
    let mut config = prost_build::Config::new();
    config.type_attribute(
        ".",
        "#[derive(serde::Serialize, serde::Deserialize)]\n#[serde(default)]",
    );
    config.compile_protos(&["src/proto/event.proto"], &["src/proto/"])?;
    Ok(())
}
