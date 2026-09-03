fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Subcommands first. Everything else keeps the historical contract:
    // the single argument is the port.
    match args.first().map(String::as_str) {
        Some("notify") => std::process::exit(roost::cli::run_notify(&args[1..])),
        Some("claude-hook") => std::process::exit(roost::cli::run_claude_hook()),
        _ => {}
    }
    // No compiled-in roots any more, so an unset ROOST_ROOTS is a
    // misconfiguration, not a default. Serving an empty root list would come
    // up healthy and show no projects at all, which reads as data loss.
    let roots = roost::projects::roots();
    if roots.is_empty() {
        eprintln!(
            "roost: no project roots configured.\n\
             Set ROOST_ROOTS to a colon-separated list of directories to scan:\n\
             \n    ROOST_ROOTS=$HOME/projects roost 8444\n\
             \nFor a service, set it in the unit file (Environment=ROOST_ROOTS=...).\n\
             Or list them in ~/.config/roost/config.toml:\n\
             \n    roots = [\"~/projects\"]"
        );
        std::process::exit(2);
    }
    // Both sources naming roots and disagreeing is a misconfiguration that is
    // otherwise invisible: ROOST_ROOTS wins here, while a caller that inherits
    // none of this process's environment — a hook, say — silently resolves
    // the other set. Loud, on the server's stderr, which systemd captures.
    if let Some((env, cfg)) = roost::projects::roots_conflict(
        std::env::var("ROOST_ROOTS").ok().as_deref(),
        &roost::config::global_config_path(),
    ) {
        eprintln!(
            "roost: ROOST_ROOTS and the global config's `roots` disagree.\n  \
             ROOST_ROOTS (used here): {env:?}\n  \
             config `roots` (used by anything without this environment): {cfg:?}\n  \
             Bring them into step, or callers outside this process will resolve different projects."
        );
        // Also to the one file, so a reader has a single place to look rather
        // than having to know which detector reports where. stderr stays
        // because journald already carries this process's startup messages.
        roost::errlog::record(
            &format!("ROOST_ROOTS {env:?} disagrees with the global config's roots {cfg:?}"),
            roost::errlog::now_secs(),
        );
    }
    let port: u16 = args.first().and_then(|p| p.parse().ok()).unwrap_or(8444);
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind 127.0.0.1");
    // Here rather than in `serve`: the check asks the user's real login shell,
    // and the test servers `serve` starts must not depend on what that shell
    // has installed. Background, so listening does not wait on a profile.
    roost::launch::probe_all_in_background();
    // Also here rather than in `serve`, and for the same reason: this walks
    // the host's real process table on a timer, which the test servers
    // `serve` starts have no business doing.
    roost::claudes::watch();
    eprintln!("roost listening on http://127.0.0.1:{port}");
    roost::serve(listener, roots);
}
