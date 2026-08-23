fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Subcommands first. Everything else keeps the historical contract:
    // the single argument is the port.
    match args.first().map(String::as_str) {
        Some("notify") => std::process::exit(resh::cli::run_notify(&args[1..])),
        Some("peers") => std::process::exit(resh::cli::run_peers(&args[1..])),
        _ => {}
    }
    // No compiled-in roots any more, so an unset RESH_ROOTS is a
    // misconfiguration, not a default. Serving an empty root list would come
    // up healthy and show no projects at all, which reads as data loss.
    let roots = resh::projects::roots();
    if roots.is_empty() {
        eprintln!(
            "resh: RESH_ROOTS is unset or empty.\n\
             Set it to a colon-separated list of directories to scan for projects:\n\
             \n    RESH_ROOTS=$HOME/projects resh 8444\n\
             \nFor a service, set it in the unit file (Environment=RESH_ROOTS=...)."
        );
        std::process::exit(2);
    }
    let port: u16 = args.first().and_then(|p| p.parse().ok()).unwrap_or(8444);
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind 127.0.0.1");
    eprintln!("resh listening on http://127.0.0.1:{port}");
    resh::serve(listener, roots);
}
