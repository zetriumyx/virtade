/// Keep app authority on the bundled origin or the explicit development server.
pub fn app_origin(scheme: &str, host: Option<&str>, port: Option<u16>, development: bool) -> bool {
    match (scheme, host, port) {
        ("tauri", Some("localhost"), None) => true,
        ("http" | "https", Some("tauri.localhost"), None) => true,
        ("http", Some("localhost" | "127.0.0.1"), Some(1420)) => development,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::app_origin;
    #[test]
    fn external_pages_and_lookalike_origins_never_receive_app_authority() {
        for (scheme, host, port) in [
            ("https", Some("example.com"), None),
            ("http", Some("tauri.localhost.evil.invalid"), None),
            ("http", Some("localhost"), Some(3000)),
            ("file", None, None),
            ("data", None, None),
            ("about", None, None),
        ] {
            assert!(!app_origin(scheme, host, port, true));
        }
        assert!(!app_origin("http", Some("localhost"), Some(1420), false));
        assert!(app_origin("http", Some("127.0.0.1"), Some(1420), true));
        assert!(app_origin("tauri", Some("localhost"), None, false));
        assert!(app_origin("http", Some("tauri.localhost"), None, false));
    }
}
