// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use PROTOC env var if set, otherwise fall back to system protoc.
    if std::env::var("PROTOC").is_err() {
        // Check common Chromium checkout location.
        let chromium_protoc = concat!(
            env!("HOME"),
            "/chromium/src/out/Default/protoc"
        );
        if std::path::Path::new(chromium_protoc).exists() {
            std::env::set_var("PROTOC", chromium_protoc);
        }
    }

    prost_build::Config::new()
        .compile_protos(&["proto/trace_processor.proto"], &["proto/"])?;
    Ok(())
}
