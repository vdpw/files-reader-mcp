use anyhow::{Context, Result, ensure};
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h") {
        println!(
            "files-reader-mcp --config /absolute/path/config.toml\nLocal read-only MCP server; defaults to 127.0.0.1:3210/mcp. See config.example.toml."
        );
        return Ok(());
    }
    ensure!(
        args.len() == 3 && args[1] == "--config",
        "Usage: files-reader-mcp --config /path/config.toml"
    );
    let raw = std::fs::read_to_string(&args[2]).context("Cannot read configuration")?;
    let config = toml::from_str(&raw).context("Invalid TOML configuration")?;
    files_reader_mcp::server::serve(config).await
}
