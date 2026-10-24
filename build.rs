// Uninstalling and cleaning Program Files / HKLM needs admin, like Geek does.
// Release only, so `cargo test` / `cargo run` stay unelevated.
fn main() {
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:level='requireAdministrator'");
    }
}
