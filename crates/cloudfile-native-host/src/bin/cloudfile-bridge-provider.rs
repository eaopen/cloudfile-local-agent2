//! Independent CloudFile file Provider entry for the new Local Bridge stack.
//! It is NOT a CloudFile Chrome Native Messaging Host. Its private storage
//! does not reuse the installed CloudFile Agent2 data directory.
#[path = "../bridge_provider.rs"]
mod bridge_provider;

fn main() {
    if std::env::args_os().len() != 1 {
        eprintln!("CloudFile Bridge Provider accepts MCP stdio only (no CLI arguments)");
        std::process::exit(2);
    }
    if let Err(error) = bridge_provider::serve() {
        eprintln!("CloudFile Bridge Provider: {error:#}");
        std::process::exit(1);
    }
}
