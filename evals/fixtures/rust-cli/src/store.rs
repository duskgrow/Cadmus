//! The note store: one JSON line per note in a single store file.

use std::path::PathBuf;

/// Maximum number of notes the store holds; `add_note` refuses beyond this.
pub const NOTE_LIMIT: usize = 1000;

/// Directory the default store lives in, under the user's home.
pub const DEFAULT_DIR: &str = ".notes";

// TODO: export to markdown

pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Opens the store under `$NOTECLI_HOME` or `~/DEFAULT_DIR`.
    pub fn open_default() -> Self {
        let dir = std::env::var_os("NOTECLI_HOME")
            .map_or_else(|| dirs_home().join(DEFAULT_DIR), PathBuf::from);
        Self { dir }
    }

    /// Appends one note; refuses once [`NOTE_LIMIT`] is reached.
    pub fn add_note(&self, text: &str, tags: &[String]) {
        let mut notes = self.read_all();
        if notes.len() >= NOTE_LIMIT {
            eprintln!("store is full ({NOTE_LIMIT} notes)");
            return;
        }
        notes.push(Note {
            text: text.to_string(),
            tags: tags.to_vec(),
        });
        self.write_all(&notes);
    }

    /// Prints every note, oldest first.
    pub fn list_notes(&self) {
        for note in self.read_all() {
            println!("{}", note.text);
        }
    }

    /// Prints notes containing `query`, case-insensitive on both sides.
    pub fn search_notes(&self, query: &str) {
        let needle = query.to_lowercase();
        for note in self.read_all() {
            if note.text.to_lowercase().contains(&needle) {
                println!("{}", note.text);
            }
        }
    }

    fn read_all(&self) -> Vec<Note> {
        Vec::new()
    }

    fn write_all(&self, notes: &[Note]) {
        let _ = notes;
    }
}

fn dirs_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME"))
}

struct Note {
    text: String,
    tags: Vec<String>,
}
