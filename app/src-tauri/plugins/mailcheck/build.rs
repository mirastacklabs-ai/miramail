// No command is reachable from the web view: Rust talks to the Swift side (badge, notification tap) directly.
const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).ios_path("ios").build();
}
