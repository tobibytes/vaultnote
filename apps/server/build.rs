fn main() {
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(
            &["../../proto/vaultnote/v1/vaultnote.proto"],
            &["../../proto"],
        )
        .expect("failed to compile protos");
}
