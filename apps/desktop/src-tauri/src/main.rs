#[cfg(feature = "desktop")]
mod desktop;
#[cfg(any(feature = "desktop", test))]
mod security;
fn main() {
    #[cfg(feature = "desktop")]
    desktop::run();
    #[cfg(not(feature = "desktop"))]
    eprintln!("Build the native shell with --features desktop (see README prerequisites)");
}
