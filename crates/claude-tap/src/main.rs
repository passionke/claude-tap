//! claude-tap binary entry. Author: kejiqing

use claude_tap::cli::parse_tap_args;
use claude_tap::export::export_main;
use claude_tap::run::async_main;

#[tokio::main]
async fn main() {
    let mut argv: Vec<String> = std::env::args().collect();
    if argv.len() > 1 && argv[1] == "export" {
        let code = export_main(&argv[2..]);
        std::process::exit(code);
    }
    // Drop program name for clap via OsString path
    let _ = argv;
    let args = parse_tap_args(std::env::args_os());
    match async_main(args).await {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("claude-tap error: {e:#}");
            std::process::exit(1);
        }
    }
}
