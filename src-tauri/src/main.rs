// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|arg| arg == "--hook-event") {
        if let Err(error) = codextray_lib::run_hook_event_process() {
            eprintln!("{}", error);
            std::process::exit(1);
        }

        return;
    }

    codextray_lib::run()
}
