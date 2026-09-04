//! `claude-history annotators`: the registrations annotations are read from and
//! written to.
//!
//! Config is edited rather than regenerated, so comments and every section
//! outside `[annotators]` survive the write.

use crate::cli::{AnnotatorAddArgs, AnnotatorsCommand};
use crate::config::{self, DEFAULT_ANNOTATOR};
use crate::error::{AppError, Result};
use std::path::PathBuf;
use toml_edit::{DocumentMut, Item, Table, value};

/// Read the config file into an editable document, an empty one when the file
/// is absent.
fn load_document(path: &PathBuf) -> Result<DocumentMut> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(DocumentMut::new());
    };
    text.parse::<DocumentMut>()
        .map_err(|error| AppError::ConfigError(format!("config at {}: {error}", path.display())))
}

fn save_document(path: &PathBuf, document: &DocumentMut) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, document.to_string())?;
    Ok(())
}

fn config_path() -> Result<PathBuf> {
    config::get_config_path().ok_or_else(|| {
        AppError::ConfigError("no home directory, so no config file path".to_string())
    })
}

/// The `[annotators]` table, created when absent.
fn annotators_table(document: &mut DocumentMut) -> Result<&mut Table> {
    let entry = document
        .entry("annotators")
        .or_insert_with(|| Item::Table(Table::new()));
    let table = entry.as_table_mut().ok_or_else(|| {
        AppError::ConfigError("config key `annotators` is not a table".to_string())
    })?;
    // Implicit, so the file carries `[annotators.chsum]` alone rather than an
    // empty `[annotators]` header above it.
    table.set_implicit(true);
    Ok(table)
}

pub fn run(command: AnnotatorsCommand) -> Result<String> {
    match command {
        AnnotatorsCommand::List => list(),
        AnnotatorsCommand::Add(args) => add(args),
        AnnotatorsCommand::Remove { key } => remove(&key),
        AnnotatorsCommand::WriteTo { key } => write_to(&key),
    }
}

fn list() -> Result<String> {
    let config = config::load_config()?;
    let set = super::AnnotatorSet::from_config(&config);
    let write_target = set.write_target().to_string();

    let mut output = String::new();
    for key in set.keys() {
        let label = set.label(key).unwrap_or(key);
        let source = config
            .annotators
            .as_ref()
            .and_then(|table| table.get(key))
            .and_then(|entry| entry.command.clone())
            .unwrap_or_else(|| match set.file_root() {
                Some(root) => root.display().to_string(),
                None => String::new(),
            });
        let marker = if key == write_target {
            "  <- writes"
        } else {
            ""
        };
        output.push_str(&format!("  {key:<10} {label:<10} {source}{marker}\n"));
    }
    if output.is_empty() {
        output.push_str("  no annotators registered\n");
    }
    Ok(output)
}

fn add(args: AnnotatorAddArgs) -> Result<String> {
    if args.key == DEFAULT_ANNOTATOR {
        return Err(AppError::ConfigError(format!(
            "`{DEFAULT_ANNOTATOR}` is the built-in file annotator and takes no command"
        )));
    }
    let path = config_path()?;
    let mut document = load_document(&path)?;

    let table = annotators_table(&mut document)?;
    let entry = table
        .entry(&args.key)
        .or_insert_with(|| Item::Table(Table::new()));
    let entry = entry.as_table_mut().ok_or_else(|| {
        AppError::ConfigError(format!(
            "config key `annotators.{}` is not a table",
            args.key
        ))
    })?;
    entry["command"] = value(&args.command);
    match args.name {
        Some(name) => entry["name"] = value(&name),
        None => {
            entry.remove("name");
        }
    }

    save_document(&path, &document)?;
    Ok(format!("registered {} as `{}`\n", args.key, args.command))
}

fn remove(key: &str) -> Result<String> {
    let path = config_path()?;
    let mut document = load_document(&path)?;

    let table = annotators_table(&mut document)?;
    if table.remove(key).is_none() {
        return Err(AppError::ConfigError(format!(
            "no annotator registered as {key}"
        )));
    }

    // Writes addressed to a dropped annotator would fail at the next keystroke,
    // so the target returns to the built-in file annotator with the
    // registration.
    let mut reset_write_target = false;
    if super::AnnotatorSet::from_current_config().write_target() == key {
        set_write_target(&mut document, DEFAULT_ANNOTATOR)?;
        reset_write_target = true;
    }

    save_document(&path, &document)?;
    if reset_write_target {
        return Ok(format!(
            "removed {key}; writes go to `{DEFAULT_ANNOTATOR}`\n"
        ));
    }
    Ok(format!("removed {key}\n"))
}

fn set_write_target(document: &mut DocumentMut, key: &str) -> Result<()> {
    let entry = document
        .entry("annotations")
        .or_insert_with(|| Item::Table(Table::new()));
    let table = entry.as_table_mut().ok_or_else(|| {
        AppError::ConfigError("config key `annotations` is not a table".to_string())
    })?;
    table["write_to"] = value(key);
    Ok(())
}

fn write_to(key: &str) -> Result<String> {
    let config = config::load_config()?;
    let registered = key == DEFAULT_ANNOTATOR
        || config
            .annotators
            .as_ref()
            .is_some_and(|table| table.contains_key(key));
    if !registered {
        return Err(AppError::ConfigError(format!(
            "no annotator registered as {key}"
        )));
    }

    let path = config_path()?;
    let mut document = load_document(&path)?;
    set_write_target(&mut document, key)?;
    save_document(&path, &document)?;
    Ok(format!("writes go to {key}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_an_annotator_leaves_other_sections_and_comments_intact() {
        let mut document = "# kept\n[display]\nwidth = 80\n"
            .parse::<DocumentMut>()
            .unwrap();

        let table = annotators_table(&mut document).unwrap();
        let entry = table
            .entry("chsum")
            .or_insert_with(|| Item::Table(Table::new()));
        entry.as_table_mut().unwrap()["command"] = value("chsum annotations");

        let rendered = document.to_string();
        assert!(rendered.contains("# kept"), "{rendered}");
        assert!(rendered.contains("width = 80"), "{rendered}");
        assert!(
            rendered.contains("command = \"chsum annotations\""),
            "{rendered}"
        );
    }

    #[test]
    fn the_write_target_is_one_line_in_the_annotations_table() {
        let mut document = "[annotations]\nroot = \"/tmp/notes\"\n"
            .parse::<DocumentMut>()
            .unwrap();

        set_write_target(&mut document, "chsum").unwrap();

        let rendered = document.to_string();
        assert!(rendered.contains("root = \"/tmp/notes\""), "{rendered}");
        assert!(rendered.contains("write_to = \"chsum\""), "{rendered}");
    }

    #[test]
    fn a_missing_config_file_parses_as_an_empty_document() {
        let dir = tempfile::tempdir().unwrap();
        let document = load_document(&dir.path().join("absent.toml")).unwrap();

        assert_eq!(document.to_string(), "");
    }
}
