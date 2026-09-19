//! Shared harness behind one small Interface: one [`Server`] and one
//! [`Client`], assembled fluently — the Server spawns the real binaries
//! against a tempdir socket (or stands in with scripted frames), and the
//! Client drives the real `ncap` binary or speaks the raw wire protocol.
//!
//! Tests cross this Seam via `Server::builder()`, `Client::at()`, the
//! [`probe`] wire helpers, the [`child`] lifecycle helpers, the [`assert`]
//! predicates, the [`script`] builders, and the [`fixture`] Ctl world —
//! never past it via log strings, socket paths, or direct field access.

#![allow(dead_code)]
// Re-export facade: each test target uses a different subset, so unused
// re-exports are expected per target.
#![allow(unused_imports)]

pub mod assert;
pub mod child;
pub mod client;
pub mod fixture;
pub mod probe;
pub mod script;
pub mod server;

pub(crate) use child::{WAIT_PHASE, WAIT_ROOMY, WAIT_TIGHT};
pub(crate) use client::{Client, ClientOutput, ClientProc, bin_path, wait_bounded};
pub(crate) use server::{Server, ServerBuilder, missing_socket};
