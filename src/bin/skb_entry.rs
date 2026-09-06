mod disk_usage;

// Keep the validated v1 CLI source byte-for-byte unchanged. The legacy CLI is
// compiled as a nested module and receives every command except the disk-usage
// extension below.
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
        Some("help") | Some("--help") | Some("-h") => {
            legacy::print_legacy_help();
            println!("\nDISK USAGE EXTENSION:\n  skb largest <root> [limit]              Show the largest files by metadata size only\n\nEXAMPLE:\n  skb largest D:\\ 100");
        }
        _ => legacy::dispatch(),
    }
}
