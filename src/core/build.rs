use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = PathBuf::from("src/steam/proto/cloud.proto");
    println!("cargo:rerun-if-changed={}", proto.display());

    let descriptors = protox::compile([&proto], ["src/steam/proto"])?;
    let mut config = prost_build::Config::new();
    config.compile_fds(descriptors)?;
    Ok(())
}
