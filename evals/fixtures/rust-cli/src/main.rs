//! notecli: a tiny command-line notebook.

mod args;
mod store;

use args::Command;
use store::Store;

fn main() {
    let command = args::parse_args(std::env::args().skip(1).collect());
    let store = Store::open_default();
    match command {
        Command::Add { text, tags } => store.add_note(&text, &tags),
        Command::List => store.list_notes(),
        Command::Search { query } => store.search_notes(&query),
    }
}
