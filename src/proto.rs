//! Protobuf messages generated from the files vendored in `proto/`.
//!
//! librespot-protocol ships these `.proto` sources but does not compile
//! them, so `build.rs` generates them into `OUT_DIR` and this module makes
//! them part of the crate. Everything here is machine-written; the shapes
//! come from Spotify's own client and are documented in
//! `docs/adr/0006-library-order-and-pins-from-the-internal-protocol.md`.

include!(concat!(env!("OUT_DIR"), "/protos/mod.rs"));
