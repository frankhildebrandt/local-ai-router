fn main() {
    let mut args = std::env::args().skip(1).peekable();
    match args.peek().map(String::as_str) {
        Some("serve") => {
            args.next();
            if let Err(error) = local_ai_router_lib::serve_headless(args) {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
        Some("-h" | "--help" | "help") => {
            print!(
                "Local AI Router\n\nCommands:\n  serve    Start the gateway without a desktop window or tray\n\n{}Run without arguments to open the desktop app.\n",
                local_ai_router_lib::engine::serve_help()
            );
        }
        _ => local_ai_router_lib::run(),
    }
}
