// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

pub(crate) mod proto {
    include!(concat!(env!("OUT_DIR"), "/perfetto.protos.rs"));
}

pub(crate) mod download;
pub mod error;
pub(crate) mod query;
pub mod server;
pub mod tp_client;
pub mod tp_manager;
