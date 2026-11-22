fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=src/proto/event.proto");
    prost_build::compile_protos(&["src/proto/event.proto"], &["src/proto/"])?;
    Ok(())
}
