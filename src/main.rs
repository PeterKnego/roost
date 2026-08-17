fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // One subcommand only. Everything else keeps the historical contract:
    // the single argument is the port.
    if args.first().map(String::as_str) == Some("notify") {
        std::process::exit(deadlight::cli::run_notify(&args[1..]));
    }
    let port: u16 = args.first().and_then(|p| p.parse().ok()).unwrap_or(8444);
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind 127.0.0.1");
    eprintln!("deadlight listening on http://127.0.0.1:{port}");
    deadlight::serve(listener, deadlight::projects::roots());
}
