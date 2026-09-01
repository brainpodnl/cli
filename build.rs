fn main() -> Result<(), Box<dyn std::error::Error>> {
    let descriptors = protox::compile(
        [
            "proto/brainpod/tunnel/v1/broker.proto",
            "proto/brainpod/tunnel/v1/tunnel.proto",
        ],
        ["proto"],
    )?;

    tonic_build::configure()
        .build_server(false)
        .build_transport(false)
        .compile_fds(descriptors)?;

    Ok(())
}
