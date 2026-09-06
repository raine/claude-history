//! The registered annotators, read together and written to one at a time.

use super::{
    Annotation, Annotator, CommandAnnotator, ConversationAnnotations, FileAnnotator, sidecar_counts,
};
use crate::config::{ConfigFile, DEFAULT_ANNOTATOR};
use crate::error::{AppError, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One registered annotator: its key, the label the viewer prints, and the
/// annotator itself.
pub struct RegisteredAnnotator {
    pub key: String,
    pub label: String,
    annotator: Box<dyn Annotator>,
}

/// Every registered annotator, plus the key writes are dispatched to.
pub struct AnnotatorSet {
    annotators: Vec<RegisteredAnnotator>,
    write_to: String,
    /// Root of the built-in file annotator, held for the startup count.
    file_root: Option<PathBuf>,
}

/// The key rendered with its first letter capitalised, which is the label a
/// registration without a `name` takes.
fn label_from_key(key: &str) -> String {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

impl AnnotatorSet {
    /// Build the set from config: every `[annotators.<key>]` carrying a
    /// command, plus the built-in file annotator whenever a root resolves.
    pub fn from_config(config: &ConfigFile) -> Self {
        let file_root = crate::config::annotations_root(config);
        let write_to = crate::config::annotation_write_target(config);
        let mut annotators: Vec<RegisteredAnnotator> = Vec::new();

        if let Some(root) = file_root.clone() {
            annotators.push(RegisteredAnnotator {
                key: DEFAULT_ANNOTATOR.to_string(),
                label: config
                    .annotators
                    .as_ref()
                    .and_then(|table| table.get(DEFAULT_ANNOTATOR))
                    .and_then(|entry| entry.name.clone())
                    .unwrap_or_else(|| label_from_key(DEFAULT_ANNOTATOR)),
                annotator: Box::new(FileAnnotator::new(root)),
            });
        }

        if let Some(table) = config.annotators.as_ref() {
            for (key, entry) in table {
                let Some(command) = entry.command.clone() else {
                    continue;
                };
                annotators.push(RegisteredAnnotator {
                    key: key.clone(),
                    label: entry.name.clone().unwrap_or_else(|| label_from_key(key)),
                    annotator: Box::new(CommandAnnotator::new(command)),
                });
            }
        }

        Self {
            annotators,
            write_to,
            file_root,
        }
    }

    /// The set built from the config file, empty when the file fails to load.
    pub fn from_current_config() -> Self {
        match crate::config::load_config() {
            Ok(config) => Self::from_config(&config),
            Err(_) => Self {
                annotators: Vec::new(),
                write_to: DEFAULT_ANNOTATOR.to_string(),
                file_root: None,
            },
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.annotators.iter().map(|entry| entry.key.as_str())
    }

    pub fn label(&self, key: &str) -> Option<&str> {
        self.annotators
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.label.as_str())
    }

    /// Label per annotator key, for the viewer's name column.
    pub fn labels(&self) -> HashMap<String, String> {
        self.annotators
            .iter()
            .map(|entry| (entry.key.clone(), entry.label.clone()))
            .collect()
    }

    pub fn write_target(&self) -> &str {
        &self.write_to
    }

    pub fn file_root(&self) -> Option<&Path> {
        self.file_root.as_deref()
    }

    /// Read every annotator and merge the results by conversation.
    ///
    /// An annotator that errors contributes nothing and the rest still return:
    /// annotations are additive, and a store that is unreachable leaves the
    /// transcript readable.
    pub fn read_all(&self, conversations: &[&Path]) -> Vec<(PathBuf, ConversationAnnotations)> {
        let mut merged: Vec<(PathBuf, Vec<Annotation>)> = Vec::new();

        for entry in &self.annotators {
            let Ok(read) = entry.annotator.read(conversations) else {
                continue;
            };
            for (conversation, annotations) in read {
                let mut flat = annotations
                    .session
                    .into_iter()
                    .chain(annotations.positioned)
                    .collect::<Vec<_>>();
                for annotation in &mut flat {
                    annotation.annotator = entry.key.clone();
                }
                match merged.iter_mut().find(|(path, _)| *path == conversation) {
                    Some((_, existing)) => existing.extend(flat),
                    None => merged.push((conversation, flat)),
                }
            }
        }

        merged
            .into_iter()
            .map(|(path, annotations)| (path, ConversationAnnotations::from_flat(annotations)))
            .collect()
    }

    /// Annotations for one conversation, merged across annotators.
    pub fn read_one(&self, conversation: &Path) -> ConversationAnnotations {
        self.read_all(&[conversation])
            .into_iter()
            .next()
            .map(|(_, annotations)| annotations)
            .unwrap_or_default()
    }

    /// Note count per conversation, merged across annotators.
    ///
    /// The file annotator counts through one walk of its root rather than one
    /// read per conversation, so a corpus with no command annotator registered
    /// costs a directory walk at startup.
    pub fn counts(&self, conversations: &[&Path]) -> HashMap<PathBuf, usize> {
        let mut counts: HashMap<PathBuf, usize> = HashMap::new();

        if let Some(root) = self.file_root.as_deref() {
            let by_sidecar = sidecar_counts(root);
            for conversation in conversations {
                let Some(sidecar) = super::sidecar_path(root, conversation) else {
                    continue;
                };
                if let Some(count) = by_sidecar.get(&sidecar) {
                    *counts.entry(conversation.to_path_buf()).or_default() += count;
                }
            }
        }

        for entry in &self.annotators {
            if entry.key == DEFAULT_ANNOTATOR {
                continue;
            }
            let Ok(read) = entry.annotator.read(conversations) else {
                continue;
            };
            for (conversation, annotations) in read {
                *counts.entry(conversation).or_default() += annotations.len();
            }
        }

        counts
    }

    /// Write one annotation to the configured target.
    ///
    /// A target naming no registered annotator is an error rather than a
    /// fallback: a write silently landing somewhere other than where it was
    /// addressed is the state INV-3 forbids.
    pub fn write(&self, conversation: &Path, annotation: &Annotation) -> Result<String> {
        let Some(entry) = self
            .annotators
            .iter()
            .find(|entry| entry.key == self.write_to)
        else {
            return Err(AppError::ConfigError(format!(
                "annotations.write_to names `{}`, which is not a registered annotator",
                self.write_to
            )));
        };
        entry.annotator.write(conversation, annotation)
    }

    /// Delete one annotation from the annotator holding it.
    ///
    /// `annotator` names that store, carried on the annotation since the read.
    /// An empty name falls to the write target, which is where an annotation
    /// written in this process sits.
    pub fn delete(&self, conversation: &Path, id: &str, annotator: &str) -> Result<bool> {
        let key = if annotator.is_empty() {
            self.write_to.as_str()
        } else {
            annotator
        };
        let Some(entry) = self.annotators.iter().find(|entry| entry.key == key) else {
            return Ok(false);
        };
        entry.annotator.delete(conversation, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AnnotatorConfig;
    use std::collections::BTreeMap;

    fn config_with(
        root: PathBuf,
        entries: Vec<(&str, AnnotatorConfig)>,
        write_to: &str,
    ) -> ConfigFile {
        let mut table = BTreeMap::new();
        table.insert(
            DEFAULT_ANNOTATOR.to_string(),
            AnnotatorConfig {
                root: Some(root),
                ..AnnotatorConfig::default()
            },
        );
        for (key, entry) in entries {
            table.insert(key.to_string(), entry);
        }
        ConfigFile {
            annotations: Some(crate::config::AnnotationsConfig {
                root: None,
                write_to: Some(write_to.to_string()),
            }),
            annotators: Some(table),
            ..ConfigFile::default()
        }
    }

    #[test]
    fn a_registration_without_a_name_is_labelled_from_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_with(
            dir.path().to_path_buf(),
            vec![(
                "chsum",
                AnnotatorConfig {
                    command: Some("true".to_string()),
                    ..AnnotatorConfig::default()
                },
            )],
            DEFAULT_ANNOTATOR,
        );

        let set = AnnotatorSet::from_config(&config);

        assert_eq!(set.label("chsum"), Some("Chsum"));
        assert_eq!(set.label(DEFAULT_ANNOTATOR), Some("File"));
    }

    #[test]
    fn a_registration_without_a_command_registers_no_annotator() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_with(
            dir.path().to_path_buf(),
            vec![("empty", AnnotatorConfig::default())],
            DEFAULT_ANNOTATOR,
        );

        let set = AnnotatorSet::from_config(&config);

        assert_eq!(set.keys().collect::<Vec<_>>(), vec![DEFAULT_ANNOTATOR]);
    }

    #[test]
    fn a_write_to_naming_nothing_registered_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_with(dir.path().to_path_buf(), Vec::new(), "absent");
        let set = AnnotatorSet::from_config(&config);

        let written = set.write(Path::new("/tmp/x/a.jsonl"), &Annotation::default());

        assert!(written.is_err());
    }

    #[test]
    fn a_read_tags_each_annotation_with_the_annotator_holding_it() {
        let dir = tempfile::tempdir().unwrap();
        let conversation = PathBuf::from("/tmp/-tmp-x/a.jsonl");
        let config = config_with(dir.path().to_path_buf(), Vec::new(), DEFAULT_ANNOTATOR);
        let set = AnnotatorSet::from_config(&config);
        set.write(
            &conversation,
            &Annotation {
                id: "n1".to_string(),
                kind: "note".to_string(),
                text: "hello".to_string(),
                ..Annotation::default()
            },
        )
        .unwrap();

        let read = set.read_one(&conversation);

        assert_eq!(read.session[0].annotator, DEFAULT_ANNOTATOR);
    }

    #[test]
    fn a_delete_names_the_annotator_the_annotation_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let conversation = PathBuf::from("/tmp/-tmp-x/a.jsonl");
        let config = config_with(dir.path().to_path_buf(), Vec::new(), DEFAULT_ANNOTATOR);
        let set = AnnotatorSet::from_config(&config);
        set.write(
            &conversation,
            &Annotation {
                id: "n1".to_string(),
                text: "hello".to_string(),
                ..Annotation::default()
            },
        )
        .unwrap();

        assert!(set.delete(&conversation, "n1", DEFAULT_ANNOTATOR).unwrap());
        assert!(set.read_one(&conversation).is_empty());
    }

    #[test]
    fn a_delete_naming_an_unregistered_annotator_finds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_with(dir.path().to_path_buf(), Vec::new(), DEFAULT_ANNOTATOR);
        let set = AnnotatorSet::from_config(&config);

        assert!(
            !set.delete(Path::new("/tmp/x/a.jsonl"), "n1", "gone")
                .unwrap()
        );
    }
}
