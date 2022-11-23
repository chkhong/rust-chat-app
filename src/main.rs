mod server;
mod client;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn main() -> Result<()> {
    let mut args = std::env::args();
    match args.nth(1).as_ref().map(String::as_str) {
        Some("client") => client::main(),
        Some("server") => server::main(),
        _ => Err("No arguments: use with `cargo run [client|server]".into())
    }
    
}