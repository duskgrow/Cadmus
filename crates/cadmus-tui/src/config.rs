//! The item-7 settings loader (ADR-0018): human config is TOML data, machine
//! boundaries stay JSON. This module owns `settings.toml` — discovery across
//! the precedence layers, parsing, and resolution of the values the app
//! consumes. The keymap and theme files are siblings-to-come (their loaders
//! follow the same rules); provider defaults and persisted approval rules
//! join when their slices land (docs/open-items.md).
//!
//! Two standing rules, decided with this loader:
//!
//! - **No serde derives.** The arch test's serialization tripwire (xtask,
//!   ADR-0002) exempts `cadmus-contract` only, and config files are local
//!   data, not wire protocol — so parsing walks `toml::Value` by hand. The
//!   theme/keymap loaders do the same; widening the tripwire instead is a
//!   deliberate ADR-level decision, not a convenience to take here.
//! - **Strict files.** Unknown keys and wrong types are startup errors, not
//!   warnings: a typo'd key that silently does nothing costs the user more
//!   than a clear failure. Forward compatibility is not a concern at
//!   single-user scale (ADR-0018 item 7's rejection of config sprawl).
//!
//! Precedence (ADR-0012 item 1): flags > env > project > user > system.
//! No flag exists yet (the first one lands with its consumer); the
//! environment layer is capability detection, not a parallel settings
//! vocabulary — `TERM` decides what the files don't (see `resolve`). Missing files skip silently; a present
//! but unreadable or malformed file fails the boot with its path (and for
//! parse errors, the parser's position), surfaced as the binary's
//! `cadmus::settings` diagnostic.
//!
//! All IO is injected (`Env`, `Read`): process-env mutation is `unsafe` in
//! edition 2024 and forbidden in this workspace, so the discovery logic is
//! tested through fakes and only the [`motion`] wrapper touches the
//! process.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The environment lookup seam: non-empty values only, mirroring the
/// binary's `env_path` convention (an empty var is an unset var).
type Env<'a> = dyn Fn(&str) -> Option<OsString> + 'a;

/// The file-read seam.
type Read<'a> = dyn Fn(&Path) -> std::io::Result<String> + 'a;

/// The stream emission's motion profile (the 2026-09-20 second amendment's
/// two-state switch, widened to its decided three-state shape).
///
/// `Reduced` currently emits instantly — the same behavior as `None`. The
/// accessibility reading of "reduced motion" is less animation, and the
/// differentiated middle profile (a calmer drain, not no drain) is the
/// pacing-refinement item's consumer (docs/open-items.md); accepting the
/// key now keeps the file schema stable when it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// The paced typewriter: stable rows drain on the tick, budgeted by
    /// queue depth.
    Full,
    /// Less animation: instant emission today, a calmer drain once the
    /// refinement pass defines it.
    Reduced,
    /// No animation: instant emission.
    None,
}

impl Motion {
    /// Whether the paced drain paces. `Reduced` and `None` both bypass the
    /// budget (see the variant docs).
    #[must_use]
    pub fn paces(self) -> bool {
        matches!(self, Self::Full)
    }

    /// Parse the settings-file spelling.
    fn from_value(value: &str) -> Option<Self> {
        match value {
            "full" => Some(Self::Full),
            "reduced" => Some(Self::Reduced),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// A loader failure, one per file. Library-crate discipline (thiserror);
/// the binary maps it onto its `cadmus::settings` miette diagnostic.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read settings file {path}: {reason}")]
    Read { path: PathBuf, reason: String },
    #[error("settings file {path}: {reason}")]
    Parse { path: PathBuf, reason: String },
}

/// One file's parsed content — every field optional, absent keys abstain.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Layer {
    motion: Option<Motion>,
}

impl Layer {
    /// `over` wins per key: the higher-precedence file's set keys replace
    /// the lower's, unset keys inherit.
    fn merge(self, over: Self) -> Self {
        Self {
            motion: over.motion.or(self.motion),
        }
    }
}

/// The app boundary's motion resolution: read the layer files that exist,
/// resolve against the environment. The binary's one call, made before the
/// terminal goes raw.
pub fn motion() -> Result<Motion, ConfigError> {
    let get = |key: &str| std::env::var_os(key).filter(|value| !value.is_empty());
    let read = |path: &Path| std::fs::read_to_string(path);
    let cwd = std::env::current_dir().ok();
    let merged = load(&get, cwd.as_deref(), &read)?;
    let term = get("TERM");
    Ok(resolve(
        merged,
        term.as_deref().and_then(|value| value.to_str()),
    ))
}

/// Merge the tiers, lowest precedence first: system, user, project. Within
/// a tier the first readable candidate wins (the tier's own search order).
fn load(get: &Env, cwd: Option<&Path>, read: &Read) -> Result<Layer, ConfigError> {
    let user = user_candidate(get).into_iter().collect();
    let project = cwd.map_or_else(Vec::new, project_candidates);
    let mut merged = Layer::default();
    for tier in [system_candidates(get), user, project] {
        if let Some(layer) = read_first(&tier, read)? {
            merged = merged.merge(layer);
        }
    }
    Ok(merged)
}

/// Read the tier's candidates in order; the first present file parses into
/// the tier's layer. Missing files skip; unreadable or malformed files fail
/// (strict — module docs).
fn read_first(paths: &[PathBuf], read: &Read) -> Result<Option<Layer>, ConfigError> {
    for path in paths {
        match read(path) {
            Ok(text) => return parse_layer(path, &text).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ConfigError::Read {
                    path: path.clone(),
                    reason: error.to_string(),
                });
            }
        }
    }
    Ok(None)
}

/// The system tier: `cadmus/settings.toml` under each `$XDG_CONFIG_DIRS`
/// entry, in order (default `/etc/xdg`). Empty or relative entries are
/// invalid per the basedir spec and skipped; an empty or unset variable is
/// the default.
fn system_candidates(get: &Env) -> Vec<PathBuf> {
    let dirs = get("XDG_CONFIG_DIRS").unwrap_or_else(|| OsString::from("/etc/xdg"));
    dirs.to_string_lossy()
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join("cadmus/settings.toml"))
        .filter(|path| path.is_absolute())
        .collect()
}

/// The user tier: `$XDG_CONFIG_HOME/cadmus/settings.toml`, else
/// `~/.config/…`, else `%USERPROFILE%/AppData/Roaming/…` — the trace-root
/// chain's config twin, hand-rolled under the zero-new-dependency policy.
fn user_candidate(get: &Env) -> Option<PathBuf> {
    if let Some(xdg) = get("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("cadmus/settings.toml"));
    }
    if let Some(home) = get("HOME") {
        return Some(PathBuf::from(home).join(".config/cadmus/settings.toml"));
    }
    get("USERPROFILE")
        .map(|profile| PathBuf::from(profile).join("AppData/Roaming/cadmus/settings.toml"))
}

/// The project tier: `.cadmus/settings.toml` in the cwd and each ancestor,
/// nearest first — the walk stops at the first readable file, so the
/// nearest project wins.
fn project_candidates(cwd: &Path) -> Vec<PathBuf> {
    cwd.ancestors()
        .map(|dir| dir.join(".cadmus/settings.toml"))
        .collect()
}

/// Resolve the merged layers against the terminal's declaration.
/// `TERM=dumb` forces [`Motion::None`] over every file layer (ADR-0012's
/// honor-`TERM=dumb` floor); otherwise a file setting wins. With no file
/// speaking, the default follows the terminal's trustworthiness: an unset
/// `TERM` is an unknown terminal, and unknown gets no animation (the
/// pre-config detection's conservative reading); anything else paces.
fn resolve(merged: Layer, term: Option<&str>) -> Motion {
    if term == Some("dumb") {
        return Motion::None;
    }
    if let Some(motion) = merged.motion {
        return motion;
    }
    if term.is_none() {
        Motion::None
    } else {
        Motion::Full
    }
}

/// Parse one settings file. Strict: unknown keys and wrong types fail
/// (module docs).
fn parse_layer(path: &Path, text: &str) -> Result<Layer, ConfigError> {
    let value: toml::Value = toml::from_str(text).map_err(|error| ConfigError::Parse {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let table = value
        .as_table()
        .expect("a TOML document always parses to a table");
    let mut layer = Layer::default();
    for (key, value) in table {
        match key.as_str() {
            "display" => layer = layer.merge(parse_display(path, value)?),
            unknown => {
                return Err(ConfigError::Parse {
                    path: path.to_path_buf(),
                    reason: format!("unknown key `{unknown}` (known top-level keys: `display`)"),
                });
            }
        }
    }
    Ok(layer)
}

/// The `[display]` table: terminal rendering behavior.
fn parse_display(path: &Path, value: &toml::Value) -> Result<Layer, ConfigError> {
    let table = value.as_table().ok_or_else(|| ConfigError::Parse {
        path: path.to_path_buf(),
        reason: "`display` must be a table (`[display]`)".to_string(),
    })?;
    let mut layer = Layer::default();
    for (key, value) in table {
        match key.as_str() {
            "motion" => {
                let text = value.as_str().ok_or_else(|| ConfigError::Parse {
                    path: path.to_path_buf(),
                    reason: "`display.motion` must be a string".to_string(),
                })?;
                layer.motion = Some(Motion::from_value(text).ok_or_else(|| {
                    ConfigError::Parse {
                        path: path.to_path_buf(),
                        reason: format!(
                            "`display.motion` must be \"full\", \"reduced\" or \"none\", got \"{text}\""
                        ),
                    }
                })?);
            }
            unknown => {
                return Err(ConfigError::Parse {
                    path: path.to_path_buf(),
                    reason: format!("unknown key `display.{unknown}` (known keys: `motion`)"),
                });
            }
        }
    }
    Ok(layer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{Error, ErrorKind};

    fn layer(text: &str) -> Layer {
        parse_layer(Path::new("settings.toml"), text).expect("valid settings")
    }

    /// A fake environment + filesystem: `files` maps paths to contents
    /// (absent = `NotFound`; `Err` values model unreadable files). The env
    /// side honors the seam's non-empty contract like the real lookup.
    struct Fake {
        env: HashMap<String, OsString>,
        files: HashMap<PathBuf, Result<String, ErrorKind>>,
    }

    impl Fake {
        fn get(&self) -> impl Fn(&str) -> Option<OsString> + '_ {
            |key| self.env.get(key).cloned().filter(|value| !value.is_empty())
        }
        fn read(&self) -> impl Fn(&Path) -> std::io::Result<String> + '_ {
            |path| match self.files.get(path) {
                Some(Ok(text)) => Ok(text.clone()),
                Some(Err(kind)) => Err(Error::new(*kind, "fake")),
                None => Err(Error::new(ErrorKind::NotFound, "fake")),
            }
        }
    }

    #[test]
    fn an_empty_file_abstains() {
        assert_eq!(layer(""), Layer::default());
    }

    #[test]
    fn the_motion_key_parses_each_profile() {
        assert_eq!(
            layer("[display]\nmotion = \"full\"").motion,
            Some(Motion::Full)
        );
        assert_eq!(
            layer("[display]\nmotion = \"reduced\"").motion,
            Some(Motion::Reduced)
        );
        assert_eq!(
            layer("[display]\nmotion = \"none\"").motion,
            Some(Motion::None)
        );
    }

    #[test]
    fn malformed_toml_fails_with_the_path() {
        let error = parse_layer(Path::new("a/settings.toml"), "[display\n").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("a/settings.toml"), "{message}");
    }

    #[test]
    fn unknown_keys_fail() {
        for text in [
            "motion = \"full\"",
            "[display]\npacing = \"full\"",
            "[theme]",
        ] {
            assert!(parse_layer(Path::new("s.toml"), text).is_err(), "{text}");
        }
    }

    #[test]
    fn wrong_types_fail() {
        for text in [
            "display = 1",
            "[display]\nmotion = 2",
            "[display]\nmotion = \"fast\"",
        ] {
            assert!(parse_layer(Path::new("s.toml"), text).is_err(), "{text}");
        }
    }

    #[test]
    fn higher_layers_override_per_key() {
        let low = layer("[display]\nmotion = \"none\"");
        // An unset key in the higher layer inherits rather than erases.
        assert_eq!(low.merge(Layer::default()).motion, Some(Motion::None));
        let high = layer("[display]\nmotion = \"full\"");
        assert_eq!(low.merge(high).motion, Some(Motion::Full));
    }

    #[test]
    fn term_dumb_overrides_every_file_layer() {
        let full = layer("[display]\nmotion = \"full\"");
        assert_eq!(resolve(full, Some("dumb")), Motion::None);
    }

    #[test]
    fn without_a_file_setting_the_terminal_decides_the_default() {
        let bare = Layer::default();
        assert_eq!(resolve(bare, Some("xterm-256color")), Motion::Full);
        // An unset TERM is an unknown terminal: no animation (the pre-config
        // detection's conservative reading, kept).
        assert_eq!(resolve(bare, None), Motion::None);
        // A file setting wins over the terminal's default either way.
        let full = layer("[display]\nmotion = \"full\"");
        assert_eq!(resolve(full, None), Motion::Full);
    }

    #[test]
    fn only_full_paces() {
        assert!(Motion::Full.paces());
        assert!(!Motion::Reduced.paces());
        assert!(!Motion::None.paces());
    }

    #[test]
    fn the_tiers_apply_in_system_user_project_order() {
        let mut fake = Fake {
            env: HashMap::from([("XDG_CONFIG_HOME".into(), OsString::from("/home/u/.config"))]),
            files: HashMap::new(),
        };
        let system = PathBuf::from("/etc/xdg/cadmus/settings.toml");
        let user = PathBuf::from("/home/u/.config/cadmus/settings.toml");
        let project = PathBuf::from("/repo/.cadmus/settings.toml");
        for path in [&system, &user, &project] {
            fake.files
                .insert(path.clone(), Ok("[display]\nmotion = \"none\" ".into()));
        }
        // All three say `none`; flipping one tier at a time proves the order.
        let cwd = Path::new("/repo");
        assert_eq!(
            load(&fake.get(), Some(cwd), &fake.read()).unwrap().motion,
            Some(Motion::None)
        );
        *fake.files.get_mut(&project).unwrap() = Ok("[display]\nmotion = \"full\"".into());
        assert_eq!(
            load(&fake.get(), Some(cwd), &fake.read()).unwrap().motion,
            Some(Motion::Full),
            "project beats user and system"
        );
        fake.files.remove(&project);
        *fake.files.get_mut(&user).unwrap() = Ok("[display]\nmotion = \"reduced\"".into());
        assert_eq!(
            load(&fake.get(), Some(cwd), &fake.read()).unwrap().motion,
            Some(Motion::Reduced),
            "user beats system"
        );
    }

    #[test]
    fn the_system_tier_takes_the_first_readable_dir() {
        let fake = Fake {
            env: HashMap::from([(
                "XDG_CONFIG_DIRS".into(),
                OsString::from("/missing:/etc/xdg"),
            )]),
            files: HashMap::from([(
                PathBuf::from("/etc/xdg/cadmus/settings.toml"),
                Ok("[display]\nmotion = \"none\"".into()),
            )]),
        };
        assert_eq!(
            load(&fake.get(), None, &fake.read()).unwrap().motion,
            Some(Motion::None),
            "the second dir serves when the first has no file"
        );
    }

    #[test]
    fn an_empty_xdg_config_dirs_falls_back_to_the_default() {
        let fake = Fake {
            env: HashMap::from([("XDG_CONFIG_DIRS".into(), OsString::new())]),
            files: HashMap::new(),
        };
        assert_eq!(
            system_candidates(&fake.get()),
            vec![PathBuf::from("/etc/xdg/cadmus/settings.toml")]
        );
    }

    #[test]
    fn relative_xdg_dirs_are_rejected() {
        let fake = Fake {
            env: HashMap::from([("XDG_CONFIG_DIRS".into(), OsString::from("rel:/etc"))]),
            files: HashMap::new(),
        };
        assert_eq!(
            system_candidates(&fake.get()),
            vec![PathBuf::from("/etc/cadmus/settings.toml")]
        );
    }

    #[test]
    fn the_project_tier_walks_up_to_the_nearest_file() {
        let fake = Fake {
            env: HashMap::new(),
            files: HashMap::from([
                (
                    PathBuf::from("/repo/.cadmus/settings.toml"),
                    Ok("[display]\nmotion = \"none\"".into()),
                ),
                (
                    PathBuf::from("/repo/sub/.cadmus/settings.toml"),
                    Ok("[display]\nmotion = \"full\"".into()),
                ),
            ]),
        };
        assert_eq!(
            load(&fake.get(), Some(Path::new("/repo/sub/deep")), &fake.read())
                .unwrap()
                .motion,
            Some(Motion::Full),
            "the nearest project file wins, not the outer one"
        );
    }

    #[test]
    fn an_unreadable_project_file_fails_instead_of_skipping() {
        let fake = Fake {
            env: HashMap::new(),
            files: HashMap::from([(
                PathBuf::from("/repo/.cadmus/settings.toml"),
                Err(ErrorKind::PermissionDenied),
            )]),
        };
        assert!(load(&fake.get(), Some(Path::new("/repo")), &fake.read()).is_err());
    }

    #[test]
    fn missing_files_everywhere_resolve_to_the_terminal_default() {
        let fake = Fake {
            env: HashMap::from([("TERM".into(), OsString::from("xterm"))]),
            files: HashMap::new(),
        };
        let merged = load(&fake.get(), Some(Path::new("/repo")), &fake.read()).unwrap();
        assert_eq!(
            resolve(
                merged,
                fake.get()("TERM").as_deref().and_then(|v| v.to_str())
            ),
            Motion::Full
        );
    }
}
