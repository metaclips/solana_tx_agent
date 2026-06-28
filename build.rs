fn main() {
    tonic_prost_build::configure()
        .compile_protos(
            &[
                "mev-protos/auth.proto",
                "mev-protos/block.proto",
                "mev-protos/block_engine.proto",
                "mev-protos/bundle.proto",
                "mev-protos/packet.proto",
                "mev-protos/relayer.proto",
                "mev-protos/searcher.proto",
                "mev-protos/shared.proto",
                "mev-protos/shredstream.proto",
            ],
            &["mev-protos"],
        )
        .unwrap();
}
