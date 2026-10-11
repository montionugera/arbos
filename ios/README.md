# iOS

The iPhone app in this directory is the phase 6 phone.

It lists projects, follows a project's Main chat, and sends a prompt. It speaks the same engine RPC as the desktop, on `/v1/rpc`. There is no voice and no call screen.

The version on screen is `Arbos 0.2.111`. The Rust package stays `zeron`. The phone's Rust client is `app/crates/phone`. The voice crate `app/crates/mobile` is not linked.
