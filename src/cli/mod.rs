use std::io::{self, Write};

pub fn print_help() {
    println!(
        "/help  Show help\n/map   Show repo map (Rust fn only)\n/tools Show tools (fs_search, fs_read, fs_write)\n/session new|list|load <id>|delete <id>\n/clear Clear screen\n/quit  Quit\n/ask <text>  Send a single prompt to the LLM\n/read <path> [offset limit]\n/write <path> <text>\n/search <regex> [include_glob]"
    );
}

pub fn handle_command(line: &str) -> Option<bool> {
    match line.trim() {
        "/help" => {
            print_help();
            None
        }
        "/clear" => {
            print!("\x1B[2J\x1B[H");
            let _ = io::stdout().flush();
            None
        }
        "/quit" | "/exit" => Some(true),
        "/tools" => {
            println!("Available tools: fs_search, fs_read, fs_write");
            None
        }
        _ => None,
    }
}
