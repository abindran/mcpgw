//! `mcpgw` — the gateway binary.
//!
//! Wires together the server side (faces LLM clients), the client side
//! (faces backend tool servers), the store, and the protocol seam. The real
//! runtime is assembled across M0-03 onward; this skeleton just proves the
//! whole workspace links and runs.

fn main() {
    println!(
        "mcpgw {} — client-facing proto {}, backend-facing proto {}",
        env!("CARGO_PKG_VERSION"),
        mcpgw_server::client_facing_version(),
        mcpgw_client::backend_facing_version(),
    );
}
