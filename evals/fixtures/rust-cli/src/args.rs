//! Hand-rolled argument parsing: three subcommands, one flag.

/// Separator between tags in `--tags a,b,c`.
pub const TAG_SEPARATOR: char = ',';

pub enum Command {
    Add { text: String, tags: Vec<String> },
    List,
    Search { query: String },
}

/// Parses argv (without the program name) into a [`Command`].
pub fn parse_args(argv: Vec<String>) -> Command {
    let Some(head) = argv.first() else {
        return Command::List;
    };
    match head.as_str() {
        "add" => {
            let text = argv.get(1).cloned().unwrap_or_default();
            let tags = parse_tags(&argv);
            Command::Add { text, tags }
        }
        "list" => Command::List,
        "search" => Command::Search {
            query: argv.get(1).cloned().unwrap_or_default(),
        },
        other => {
            eprintln!("unknown subcommand: {other}");
            Command::List
        }
    }
}

/// Reads `--tags a,b,c` from anywhere in argv; defaults to no tags.
fn parse_tags(argv: &[String]) -> Vec<String> {
    let Some(position) = argv.iter().position(|arg| arg == "--tags") else {
        return Vec::new();
    };
    argv.get(position + 1).map_or_else(Vec::new, |joined| {
        joined
            .split(TAG_SEPARATOR)
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_string)
            .collect()
    })
}
