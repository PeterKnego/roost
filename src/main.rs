fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(8444);
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind 127.0.0.1");
    eprintln!("deadlight listening on http://127.0.0.1:{port}");
    deadlight::serve(listener, deadlight::projects::roots());
}
