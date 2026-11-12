const COMMANDS: &[&str] = &["schedule", "request_notifications", "status", "simulate_fire"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).ios_path("ios").build();
}
