#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    if let Some(code) = image_ranker_lib::decoder::worker_entry() {
        std::process::exit(code);
    }
    image_ranker_lib::run()
}
