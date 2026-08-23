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
            "resh: no project roots configured.\n\
             Set RESH_ROOTS to a colon-separated list of directories to scan:\n\
             \n    RESH_ROOTS=$HOME/projects resh 8444\n\
             \nFor a service, set it in the unit file (Environment=RESH_ROOTS=...).\n\
             Or list them in ~/.config/resh/config.toml, which callers that do not\n\
             inherit the unit's environment (such as `resh peers`) also read:\n\
             \n    roots = [\"~/projects\"]"
        );
        std::process::exit(2);
    }
    // Both sources naming roots and disagreeing is a misconfiguration that is
    // otherwise invisible: RESH_ROOTS wins here, while `resh peers` — which
    // inherits none of this process's environment — silently resolves the
    // other set. Loud, on the server's stderr, which systemd captures.
    if let Some((env, cfg)) = resh::projects::roots_conflict(
        std::env::var("RESH_ROOTS").ok().as_deref(),
        &resh::config::global_config_path(),
    ) {
        eprintln!(
            "resh: RESH_ROOTS and the global config's `roots` disagree.\n  \
             RESH_ROOTS (used here): {env:?}\n  \
             config `roots` (used by `resh peers` and anything without this environment): {cfg:?}\n  \
             Bring them into step, or callers outside this process will resolve different projects."
        );
        // Also to the one file, so a reader has a single place to look rather
        // than having to know which detector reports where. stderr stays
        // because journald already carries this process's startup messages.
        resh::errlog::record(
            &format!("RESH_ROOTS {env:?} disagrees with the global config's roots {cfg:?}"),
            resh::errlog::now_secs(),
        );
    }
    let port: u16 = args.first().and_then(|p| p.parse().ok()).unwrap_or(8444);
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind 127.0.0.1");
    eprintln!("resh listening on http://127.0.0.1:{port}");
    resh::serve(listener, roots);
}
