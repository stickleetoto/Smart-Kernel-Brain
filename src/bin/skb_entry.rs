mod disk_usage;
mod hardening;

// Keep the validated v1 CLI source byte-for-byte unchanged. The legacy CLI is
// compiled as a nested module and receives commands that do not require a
// hardening wrapper below.
mod legacy {
    include!("skb.rs");

    pub(super) fn dispatch() {
        main();
    }

    pub(super) fn print_legacy_help() {
        print_help();
    }
}

use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("largest") => {
            if let Err(e) = disk_usage::run(&args[2..]) {
                eprintln!("skb: {e}");
                std::process::exit(1);
            }
        }
        Some("scan") => {
            if let Err(e) = hardening::run_scan(&args[2..]) {
                eprintln!("skb: {e}");
                std::process::exit(1);
            }
        }
        Some("help") | Some("--help") | Some("-h") => {
            legacy::print_legacy_help();
            println!("\nDISK USAGE EXTENSION:\n  skb largest <root> [limit] [--json <file>]  Show largest files with live scan progress\n\nEXAMPLES:\n  skb largest D:\\ 100\n  skb largest D:\\ 1000 --json drive.json");
        }
        Some(command) => {
            if let Err(e) = hardening::preflight_default_index_for(command) {
                eprintln!("skb: {e}");
                std::process::exit(1);
            }
            legacy::dispatch();
        }
        None => legacy::dispatch(),
    }
}
