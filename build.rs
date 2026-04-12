// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use PROTOC env var if set, otherwise fall back to system protoc.
    if std::env::var_os("PROTOC").is_none() {
        // Convenience: check common Chromium checkout location on Unix.
        if let Some(home) = std::env::var_os("HOME") {
            let chromium_protoc =
                std::path::Path::new(&home).join("chromium/src/out/Default/protoc");
            if chromium_protoc.exists() {
                std::env::set_var("PROTOC", chromium_protoc);
            }
        }
    }

    prost_build::Config::new().compile_protos(&["proto/trace_processor.proto"], &["proto/"])?;
    Ok(())
}
