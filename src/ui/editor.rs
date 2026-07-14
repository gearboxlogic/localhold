//! In-app memory edit draft and terminal-safe text input primitives.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::types::{Importance, Memory, MemoryMetadata, MemoryUpdate, MetadataPatch};

/// Editable fields in focus order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditField {
    Content,
    Tags,
    Importance,
    Expiry,
    Metadata,
}

impl EditField {
    pub(crate) const ALL: [Self; 5] = [Self::Content, Self::Tags, Self::Importance, Self::Expiry, Self::Metadata];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Content => "CONTENT",
            Self::Tags => "TAGS JSON",
            Self::Importance => "IMPORTANCE",
            Self::Expiry => "EXPIRY",
            Self::Metadata => "METADATA JSON",
        }
    }

    pub(crate) fn next(self, backwards: bool) -> Self {
        let index = Self::ALL.iter().position(|field| *field == self).unwrap_or(0);
        let next = if backwards {
            index.checked_sub(1).unwrap_or_else(|| Self::ALL.len().saturating_sub(1))
        } else {
            let candidate = index.saturating_add(1);
            if candidate == Self::ALL.len() { 0 } else { candidate }
        };
        Self::ALL[next]
    }

    pub(crate) const fn multiline(self) -> bool {
        matches!(self, Self::Content | Self::Metadata)
    }
}

/// A UTF-8-safe text buffer with a byte-index cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextInput {
    pub value: String,
    pub cursor: usize,
}

#[expect(clippy::string_slice, reason = "cursor offsets are maintained at UTF-8 character boundaries")]
impl TextInput {
    const fn new(value: String) -> Self {
        let cursor = value.len();
        Self { value, cursor }
    }

    pub(crate) fn insert(&mut self, character: char) {
        self.value.insert(self.cursor, character);
        self.cursor = self.cursor.saturating_add(character.len_utf8());
    }

    pub(crate) fn backspace(&mut self) {
        let previous = self.previous_boundary();
        if previous < self.cursor {
            drop(self.value.drain(previous..self.cursor));
            self.cursor = previous;
        }
    }

    pub(crate) fn delete(&mut self) {
        let next = self.next_boundary();
        if next > self.cursor {
            drop(self.value.drain(self.cursor..next));
        }
    }

    pub(crate) fn left(&mut self) {
        self.cursor = self.previous_boundary();
    }

    pub(crate) fn right(&mut self) {
        self.cursor = self.next_boundary();
    }

    pub(crate) fn home(&mut self) {
        self.cursor = self.value[..self.cursor].rfind('\n').map_or(0, |index| index.saturating_add(1));
    }

    pub(crate) fn end(&mut self) {
        self.cursor = self.value[self.cursor..].find('\n').map_or(self.value.len(), |offset| self.cursor.saturating_add(offset));
    }

    pub(crate) fn up(&mut self) {
        let line_start = self.value[..self.cursor].rfind('\n').map_or(0, |index| index.saturating_add(1));
        if line_start == 0 {
            return;
        }
        let column = self.value[line_start..self.cursor].chars().count();
        let previous_end = line_start.saturating_sub(1);
        let previous_start = self.value[..previous_end].rfind('\n').map_or(0, |index| index.saturating_add(1));
        self.cursor = byte_at_char_column(&self.value, previous_start, previous_end, column);
    }

    pub(crate) fn down(&mut self) {
        let line_start = self.value[..self.cursor].rfind('\n').map_or(0, |index| index.saturating_add(1));
        let column = self.value[line_start..self.cursor].chars().count();
        let Some(newline_offset) = self.value[self.cursor..].find('\n') else {
            return;
        };
        let next_start = self.cursor.saturating_add(newline_offset).saturating_add(1);
        let next_end = self.value[next_start..].find('\n').map_or(self.value.len(), |offset| next_start.saturating_add(offset));
        self.cursor = byte_at_char_column(&self.value, next_start, next_end, column);
    }

    fn previous_boundary(&self) -> usize {
        self.value[..self.cursor].char_indices().next_back().map_or(0, |(index, _)| index)
    }

    fn next_boundary(&self) -> usize {
        self.value[self.cursor..]
            .char_indices()
            .nth(1)
            .map_or(self.value.len(), |(offset, _)| self.cursor.saturating_add(offset))
    }
}

#[expect(clippy::string_slice, reason = "line offsets come from newline and character boundaries")]
fn byte_at_char_column(value: &str, start: usize, end: usize, column: usize) -> usize {
    value[start..end].char_indices().nth(column).map_or(end, |(offset, _)| start.saturating_add(offset))
}

/// Editable copy of one memory and its card metadata.
#[derive(Debug, Clone)]
#[expect(clippy::partial_pub_fields, reason = "rendering reads draft inputs while baseline snapshots remain editor-private")]
pub(crate) struct EditDraft {
    pub field: EditField,
    /// Vertical document scroll for long edit forms.
    pub scroll: u16,
    pub content: TextInput,
    pub tags: TextInput,
    pub importance: TextInput,
    pub expiry: TextInput,
    pub metadata: TextInput,
    original: DraftValues,
    original_metadata: Option<MemoryMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DraftValues {
    content: String,
    tags: String,
    importance: String,
    expiry: String,
    metadata: String,
}

impl EditDraft {
    pub(crate) fn new(memory: &Memory, metadata: Option<&MemoryMetadata>) -> Self {
        let values = DraftValues {
            content: memory.content.clone(),
            tags: serde_json::to_string(&memory.tags).unwrap_or_else(|_| "[]".into()),
            importance: format!("{:.2}", memory.importance.value()),
            expiry: memory.expires_at.map_or_else(String::new, |value| value.to_rfc3339()),
            metadata: editable_metadata_json(metadata),
        };
        Self {
            field: EditField::Content,
            scroll: 0_u16,
            content: TextInput::new(values.content.clone()),
            tags: TextInput::new(values.tags.clone()),
            importance: TextInput::new(values.importance.clone()),
            expiry: TextInput::new(values.expiry.clone()),
            metadata: TextInput::new(values.metadata.clone()),
            original: values,
            original_metadata: metadata.cloned(),
        }
    }

    pub(crate) const fn active(&self) -> &TextInput {
        match self.field {
            EditField::Content => &self.content,
            EditField::Tags => &self.tags,
            EditField::Importance => &self.importance,
            EditField::Expiry => &self.expiry,
            EditField::Metadata => &self.metadata,
        }
    }

    pub(crate) const fn active_mut(&mut self) -> &mut TextInput {
        match self.field {
            EditField::Content => &mut self.content,
            EditField::Tags => &mut self.tags,
            EditField::Importance => &mut self.importance,
            EditField::Expiry => &mut self.expiry,
            EditField::Metadata => &mut self.metadata,
        }
    }

    pub(crate) fn ensure_cursor_visible(&mut self) {
        const VIEWPORT_LINES: usize = 12;
        let cursor_line = self.cursor_document_line();
        let scroll = usize::from(self.scroll);
        if cursor_line < scroll {
            self.scroll = u16::try_from(cursor_line).unwrap_or(u16::MAX);
        } else if cursor_line >= scroll.saturating_add(VIEWPORT_LINES) {
            let target = cursor_line.saturating_sub(VIEWPORT_LINES.saturating_sub(1));
            self.scroll = u16::try_from(target).unwrap_or(u16::MAX);
        }
    }

    #[expect(clippy::string_slice, reason = "input cursors are maintained at UTF-8 character boundaries")]
    pub(crate) fn cursor_document_line(&self) -> usize {
        let fields = [
            (EditField::Content, &self.content),
            (EditField::Tags, &self.tags),
            (EditField::Importance, &self.importance),
            (EditField::Expiry, &self.expiry),
            (EditField::Metadata, &self.metadata),
        ];
        let mut line = 3_usize;
        for (field, input) in fields {
            line = line.saturating_add(1);
            if field == self.field {
                return line.saturating_add(input.value[..input.cursor].chars().filter(|character| *character == '\n').count());
            }
            line = line.saturating_add(input.value.lines().count().max(1)).saturating_add(1);
        }
        line
    }

    pub(crate) fn dirty(&self) -> bool {
        self.current_values() != self.original
    }

    pub(crate) fn parse(&self) -> Result<ParsedEdit, DraftError> {
        let content = (self.content.value != self.original.content).then(|| self.content.value.clone());
        let tags = if self.tags.value == self.original.tags {
            None
        } else {
            Some(serde_json::from_str::<Vec<String>>(&self.tags.value).map_err(|_error| DraftError::new(EditField::Tags, "tags must be a JSON array of strings"))?)
        };

        let importance = if self.importance.value == self.original.importance {
            None
        } else {
            let value = self
                .importance
                .value
                .trim()
                .parse::<f64>()
                .map_err(|_error| DraftError::new(EditField::Importance, "enter a number from 0.0 to 1.0"))?;
            if !(0.0_f64..=1.0_f64).contains(&value) {
                return Err(DraftError::new(EditField::Importance, "importance must be from 0.0 to 1.0"));
            }
            Some(Importance::new(value))
        };

        let expires_at = if self.expiry.value == self.original.expiry {
            None
        } else {
            let trimmed = self.expiry.value.trim();
            let replacement = if trimmed.is_empty() {
                None
            } else {
                Some(
                    DateTime::parse_from_rfc3339(trimmed)
                        .map_err(|_error| DraftError::new(EditField::Expiry, "use RFC 3339, for example 2026-08-01T12:00:00Z"))?
                        .with_timezone(&Utc),
                )
            };
            Some(replacement)
        };

        let metadata_patch = if self.metadata.value == self.original.metadata {
            None
        } else {
            Some(parse_metadata_patch(&self.metadata.value, self.original_metadata.as_ref())?)
        };
        let metadata_patch = metadata_patch.filter(|patch| !patch.is_empty());

        Ok(ParsedEdit {
            update: MemoryUpdate {
                content,
                tags,
                access_policy: None,
                importance,
                expires_at,
                confidence: None,
                source_conversation: None,
                entities: None,
            },
            metadata_patch,
        })
    }

    fn current_values(&self) -> DraftValues {
        DraftValues {
            content: self.content.value.clone(),
            tags: self.tags.value.clone(),
            importance: self.importance.value.clone(),
            expiry: self.expiry.value.clone(),
            metadata: self.metadata.value.clone(),
        }
    }
}

/// Validated edit ready for the engine.
#[derive(Debug)]
pub(crate) struct ParsedEdit {
    pub update: MemoryUpdate,
    pub metadata_patch: Option<MetadataPatch>,
}

impl ParsedEdit {
    pub(crate) const fn is_empty(&self) -> bool {
        self.update.content.is_none() && self.update.tags.is_none() && self.update.importance.is_none() && self.update.expires_at.is_none() && self.metadata_patch.is_none()
    }
}

/// Field-specific draft validation error.
#[derive(Debug)]
pub(crate) struct DraftError {
    pub field: EditField,
    pub message: String,
}

impl DraftError {
    fn new(field: EditField, message: impl Into<String>) -> Self {
        Self { field, message: message.into() }
    }
}

fn editable_metadata_json(metadata: Option<&MemoryMetadata>) -> String {
    let value = serde_json::json!({
        "summary": metadata.and_then(|item| item.summary.clone()),
        "agent_label": metadata.and_then(|item| item.agent_label.clone()),
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
}

fn parse_metadata_patch(value: &str, existing: Option<&MemoryMetadata>) -> Result<MetadataPatch, DraftError> {
    let value: Value = serde_json::from_str(value).map_err(|error| DraftError::new(EditField::Metadata, format!("invalid JSON: {error}")))?;
    let object = value.as_object().ok_or_else(|| DraftError::new(EditField::Metadata, "metadata must be a JSON object"))?;
    if let Some(key) = object.keys().find(|key| !matches!(key.as_str(), "summary" | "agent_label")) {
        return Err(DraftError::new(EditField::Metadata, format!("unknown metadata field: {key}")));
    }

    let old_summary = existing.and_then(|item| item.summary.as_deref());
    let old_agent_label = existing.and_then(|item| item.agent_label.as_deref());
    let summary = metadata_field_patch(object.get("summary"), "summary", old_summary)?;
    let agent_label = metadata_field_patch(object.get("agent_label"), "agent_label", old_agent_label)?;

    Ok(MetadataPatch {
        scope_key: None,
        summary: summary.replacement,
        clear_summary: summary.clear,
        agent_label: agent_label.replacement,
        clear_agent_label: agent_label.clear,
    })
}

struct MetadataFieldPatch {
    replacement: Option<String>,
    clear: bool,
}

fn metadata_field_patch(value: Option<&Value>, field: &str, existing: Option<&str>) -> Result<MetadataFieldPatch, DraftError> {
    let Some(value) = value else {
        return Ok(MetadataFieldPatch { replacement: None, clear: false });
    };
    let replacement = optional_json_string(Some(value), field)?;
    if replacement.as_deref() == existing {
        return Ok(MetadataFieldPatch { replacement: None, clear: false });
    }
    let clear = replacement.is_none() && existing.is_some();
    Ok(MetadataFieldPatch { replacement, clear })
}

fn optional_json_string(value: Option<&Value>, field: &str) -> Result<Option<String>, DraftError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
        }
        Some(_) => Err(DraftError::new(EditField::Metadata, format!("{field} must be a string or null"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AccessPolicy, Provenance};

    fn memory() -> Memory {
        let mut memory = Memory::new_for_test("first line\nsecond line".into(), vec!["alpha".into()], Provenance::default(), AccessPolicy::Public);
        memory.importance = Importance::new(0.5);
        memory
    }

    #[test]
    fn text_input_edits_unicode_at_boundaries() {
        let mut input = TextInput::new("a\u{e9}".into());
        input.left();
        input.backspace();
        input.insert('\u{3b2}');
        assert_eq!(input.value, "\u{3b2}\u{e9}");
    }

    #[test]
    fn draft_parses_typed_fields_and_metadata_clears() {
        let memory = memory();
        let metadata = MemoryMetadata {
            memory_id: memory.id,
            scope_key: Some("scope".into()),
            summary: Some("old".into()),
            agent_label: Some("agent".into()),
            created_by_principal: Some("owner".into()),
            quality_flags: Vec::new(),
            schema_version: 1,
        };
        let mut draft = EditDraft::new(&memory, Some(&metadata));
        draft.tags.value = r#"["alpha","beta"]"#.into();
        draft.expiry.value = "2026-08-01T12:00:00Z".into();
        draft.metadata.value = r#"{"summary": null, "agent_label": "new"}"#.into();

        let parsed = draft.parse().unwrap();
        assert_eq!(parsed.update.tags.unwrap(), vec!["alpha", "beta"]);
        assert!(parsed.update.expires_at.unwrap().is_some());
        let patch = parsed.metadata_patch.unwrap();
        assert!(patch.clear_summary);
        assert_eq!(patch.agent_label.as_deref(), Some("new"));
    }

    #[test]
    fn tags_json_preserves_commas_inside_tags() {
        let mut memory = memory();
        memory.tags = vec!["client,west".into()];
        let mut draft = EditDraft::new(&memory, None);
        assert_eq!(draft.tags.value, r#"["client,west"]"#);

        draft.tags.value = r#"["client,west","urgent"]"#.into();
        let parsed = draft.parse().unwrap();

        assert_eq!(parsed.update.tags.unwrap(), vec!["client,west", "urgent"]);
    }

    #[test]
    fn tags_reject_ambiguous_non_json_input() {
        let memory = memory();
        let mut draft = EditDraft::new(&memory, None);
        draft.tags.value = "alpha, beta".into();

        let error = draft.parse().unwrap_err();

        assert_eq!(error.field, EditField::Tags);
        assert!(error.message.contains("JSON array"));
    }

    #[test]
    fn omitted_metadata_keys_preserve_existing_values() {
        let memory = memory();
        let metadata = MemoryMetadata {
            memory_id: memory.id,
            scope_key: Some("scope".into()),
            summary: Some("old".into()),
            agent_label: Some("agent".into()),
            created_by_principal: Some("owner".into()),
            quality_flags: Vec::new(),
            schema_version: 1,
        };
        let patch = parse_metadata_patch(r#"{"summary":"new"}"#, Some(&metadata)).unwrap();

        assert_eq!(patch.summary.as_deref(), Some("new"));
        assert!(!patch.clear_summary);
        assert!(patch.agent_label.is_none());
        assert!(!patch.clear_agent_label);
    }

    #[test]
    fn explicit_metadata_null_clears_only_that_field() {
        let memory = memory();
        let metadata = MemoryMetadata {
            memory_id: memory.id,
            scope_key: Some("scope".into()),
            summary: Some("old".into()),
            agent_label: Some("agent".into()),
            created_by_principal: Some("owner".into()),
            quality_flags: Vec::new(),
            schema_version: 1,
        };
        let patch = parse_metadata_patch(r#"{"agent_label":null}"#, Some(&metadata)).unwrap();

        assert!(patch.summary.is_none());
        assert!(!patch.clear_summary);
        assert!(patch.agent_label.is_none());
        assert!(patch.clear_agent_label);
    }

    #[test]
    fn metadata_rejects_unknown_fields() {
        let memory = memory();
        let mut draft = EditDraft::new(&memory, None);
        draft.metadata.value = r#"{"scope":"forbidden"}"#.into();
        let error = draft.parse().unwrap_err();
        assert_eq!(error.field, EditField::Metadata);
        assert!(error.message.contains("unknown metadata field"));
    }
}
