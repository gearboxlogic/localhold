//! Input validation and normalization utilities.
//!
//! These are pure functions operating on primitives — they have no dependency
//! on [`RecallEngine`](crate::engine::RecallEngine) or any store.

use crate::{
    error::ValidationError,
    types::{Entity, EntityType},
};

/// Convert a TTL in seconds to an absolute expiry timestamp.
///
/// # Errors
///
/// Returns `ValidationError` if the value is too large or causes overflow.
pub(crate) fn ttl_seconds_to_expiry(ttl_seconds: u64, now: chrono::DateTime<chrono::Utc>) -> Result<chrono::DateTime<chrono::Utc>, ValidationError> {
    let ttl_i64 = i64::try_from(ttl_seconds).map_err(|e| ValidationError::new("ttl_seconds", format!("value is too large: {e}")))?;
    let duration = chrono::TimeDelta::try_seconds(ttl_i64).ok_or_else(|| ValidationError::new("ttl_seconds", "value is out of range"))?;
    now.checked_add_signed(duration)
        .ok_or_else(|| ValidationError::new("ttl_seconds", "value causes timestamp overflow"))
}

/// Validate that an optional string field is not blank when provided.
pub(crate) fn validate_optional_non_empty(field_name: &str, value: Option<&str>) -> Result<(), ValidationError> {
    if value.is_some_and(|s| s.trim().is_empty()) {
        return Err(ValidationError::new(field_name, "cannot be empty when provided"));
    }
    Ok(())
}

/// Validate that an optional string array contains no empty values.
pub(crate) fn validate_optional_string_array(field_name: &str, values: Option<&[String]>) -> Result<(), ValidationError> {
    if values.is_some_and(|items| items.iter().any(|item| item.trim().is_empty())) {
        return Err(ValidationError::new(field_name, "cannot contain empty values"));
    }
    Ok(())
}

/// Validate that a string array contains no empty values.
pub(crate) fn validate_string_array(field_name: &str, values: &[String]) -> Result<(), ValidationError> {
    validate_optional_string_array(field_name, Some(values))
}

/// Normalize an optional string field: validate non-empty and trim whitespace.
///
/// Avoids allocating a new `String` when the value is already trimmed.
pub(crate) fn normalize_optional_non_empty(field_name: &str, value: Option<String>) -> Result<Option<String>, ValidationError> {
    validate_optional_non_empty(field_name, value.as_deref())?;
    Ok(value.map(|v| if v.trim().len() == v.len() { v } else { v.trim().to_owned() }))
}

/// Normalize a required string field: validate non-empty and trim whitespace.
///
/// Avoids allocating a new `String` when the value is already trimmed.
pub(crate) fn normalize_non_empty(field_name: &str, value: &str) -> Result<String, ValidationError> {
    validate_optional_non_empty(field_name, Some(value))?;
    let trimmed = value.trim();
    if trimmed.len() == value.len() { Ok(value.to_owned()) } else { Ok(trimmed.to_owned()) }
}

/// Validate content length against a maximum.
pub(crate) fn validate_content_length(content: &str, max_content_length: usize) -> Result<(), ValidationError> {
    if content.len() > max_content_length {
        return Err(ValidationError::new(
            "content",
            format!("exceeds maximum length of {max_content_length} bytes (got {})", content.len()),
        ));
    }
    Ok(())
}

/// Validate that a batch field is non-empty and within its configured cap.
pub(crate) fn validate_batch_len(field_name: &str, len: usize, max_batch_size: usize) -> Result<(), ValidationError> {
    if len == 0 {
        return Err(ValidationError::new(field_name, "cannot be empty"));
    }
    if len > max_batch_size {
        return Err(ValidationError::new(field_name, format!("exceeds maximum batch size of {max_batch_size}")));
    }
    Ok(())
}

/// Validate tags count and individual tag lengths.
pub(crate) fn validate_tags(field_name: &str, tags: &[String], max_tags: usize, max_tag_length: usize) -> Result<(), ValidationError> {
    if tags.len() > max_tags {
        return Err(ValidationError::new(field_name, format!("exceeds maximum of {max_tags} tags (got {})", tags.len())));
    }
    tags.iter().try_for_each(|tag| {
        if tag.len() > max_tag_length {
            Err(ValidationError::new(
                field_name,
                format!("contains a tag exceeding maximum length of {max_tag_length} bytes (got {})", tag.len()),
            ))
        } else {
            Ok(())
        }
    })
}

/// Normalize an optional string array: validate non-empty and trim all items.
pub(crate) fn normalize_optional_string_array(field_name: &str, values: Option<Vec<String>>) -> Result<Option<Vec<String>>, ValidationError> {
    validate_optional_string_array(field_name, values.as_deref())?;
    Ok(values.map(|items| items.into_iter().map(|item| item.trim().to_owned()).collect()))
}

/// Parse a string as a [`MemoryId`].
///
/// # Errors
///
/// Returns `ValidationError` if the string is not a valid ULID.
#[cfg(test)]
pub(crate) fn parse_memory_id(s: &str) -> Result<crate::types::MemoryId, ValidationError> {
    s.parse().map_err(|e: ulid::DecodeError| ValidationError::new("id", format!("invalid memory id: {e}")))
}

/// Clamp an importance value to the valid `[0.0, 1.0]` range.
///
/// Non-finite values (`NaN`, `Infinity`) are replaced with the default of 0.5
/// to prevent panics from `f64::clamp` (which panics on `NaN`).
pub(crate) const fn clamp_importance(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.5;
    }
    value.clamp(0.0, 1.0)
}

/// Default maximum number of entities per memory (parity with `max_tags_per_memory`).
#[cfg(test)]
const DEFAULT_MAX_ENTITIES_PER_MEMORY: usize = 50;

/// Default maximum length of an entity name or type in bytes (parity with `max_tag_length`).
#[cfg(test)]
const DEFAULT_MAX_ENTITY_FIELD_LENGTH: usize = 256;

/// Validate and normalize raw entity fields.
///
/// Returns the trimmed entity name and validated [`EntityType`] when both
/// fields are non-empty and free of ASCII control characters.
pub(crate) fn normalize_entity_parts(name: &str, entity_type: &str) -> Result<(String, EntityType), ValidationError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ValidationError::new("entities", "entity name cannot be empty"));
    }

    let entity_type = entity_type.trim();
    if entity_type.is_empty() {
        return Err(ValidationError::new("entities", "entity type cannot be empty"));
    }

    if name.bytes().any(|byte| byte <= 0x1F_u8) {
        return Err(ValidationError::new("entities", "entity name cannot contain ASCII control characters"));
    }

    if entity_type.bytes().any(|byte| byte <= 0x1F_u8) {
        return Err(ValidationError::new("entities", "entity type cannot contain ASCII control characters"));
    }

    let Some(entity_type) = EntityType::new(entity_type) else {
        return Err(ValidationError::new("entities", "entity type cannot be empty"));
    };
    Ok((name.to_owned(), entity_type))
}

/// Validate a list of entities with configurable limits.
///
/// Names and types must be non-empty after trimming, and the total count and
/// field lengths must be within the provided limits.
pub(crate) fn validate_entities_with_limits(entities: &[Entity], max_entities: usize, max_entity_field_length: usize) -> Result<(), ValidationError> {
    if entities.len() > max_entities {
        return Err(ValidationError::new(
            "entities",
            format!("exceeds maximum of {max_entities} entities (got {})", entities.len()),
        ));
    }
    entities.iter().try_for_each(|entity| {
        let _normalized = normalize_entity_parts(&entity.name, entity.entity_type.as_str())?;
        if entity.name.len() > max_entity_field_length {
            return Err(ValidationError::new(
                "entities",
                format!("entity name exceeds maximum length of {max_entity_field_length} bytes (got {})", entity.name.len()),
            ));
        }
        if entity.entity_type.as_str().len() > max_entity_field_length {
            return Err(ValidationError::new(
                "entities",
                format!(
                    "entity type exceeds maximum length of {max_entity_field_length} bytes (got {})",
                    entity.entity_type.as_str().len()
                ),
            ));
        }
        Ok(())
    })
}

/// Validate entities with default limits (convenience for tests).
#[cfg(test)]
fn validate_entities(entities: &[Entity]) -> Result<(), ValidationError> {
    validate_entities_with_limits(entities, DEFAULT_MAX_ENTITIES_PER_MEMORY, DEFAULT_MAX_ENTITY_FIELD_LENGTH)
}

/// Normalize entities by trimming whitespace from names.
///
/// [`EntityType`] values are already trimmed by construction, so only
/// names need explicit trimming here.
pub(crate) fn normalize_entities(entities: Vec<Entity>) -> Vec<Entity> {
    entities
        .into_iter()
        .map(|e| Entity {
            name: e.name.trim().to_owned(),
            entity_type: e.entity_type,
        })
        .collect()
}

/// Validate that a required string is not blank (whitespace-only).
pub(crate) fn validate_non_blank(field_name: &str, value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new(field_name, "cannot be blank"));
    }
    Ok(())
}

/// Upper bound for `max_distance` filter values.
///
/// L2 distance for normalized vectors (unit length) ranges from 0.0 (identical)
/// to 2.0 (diametrically opposed). A cap of 10.0 is generous enough for any
/// practical query while rejecting clearly nonsensical values.
const MAX_DISTANCE_UPPER_BOUND: f64 = 10.0;

/// Validate that a `max_distance` filter value is finite, non-negative, and within bounds.
pub(crate) fn validate_max_distance(max_distance: Option<f64>) -> Result<(), ValidationError> {
    if let Some(d) = max_distance {
        if !d.is_finite() || d < 0.0_f64 {
            return Err(ValidationError::new("max_distance", "must be a finite non-negative number"));
        }
        if d > MAX_DISTANCE_UPPER_BOUND {
            return Err(ValidationError::new(
                "max_distance",
                format!("exceeds maximum of {MAX_DISTANCE_UPPER_BOUND} (typical L2 distance range for normalized vectors is 0.0–2.0)"),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Entity, EntityType, MemoryId};

    // -- RR-009: clamp_importance ---------------------------------------------

    #[test]
    fn clamp_importance_nan_returns_default() {
        assert!((clamp_importance(f64::NAN) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_importance_positive_infinity_returns_default() {
        assert!((clamp_importance(f64::INFINITY) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_importance_negative_infinity_returns_default() {
        assert!((clamp_importance(f64::NEG_INFINITY) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_importance_below_zero_returns_zero() {
        assert!((clamp_importance(-0.5) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_importance_above_one_returns_one() {
        assert!((clamp_importance(1.5) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_importance_passthrough() {
        assert!((clamp_importance(0.7) - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_importance_boundary_zero() {
        assert!((clamp_importance(0.0) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_importance_boundary_one() {
        assert!((clamp_importance(1.0) - 1.0).abs() < f64::EPSILON);
    }

    // -- RR-010: validate_entities -------------------------------------------

    #[test]
    fn validate_entities_exceeds_max_count() {
        let entities: Vec<Entity> = (0_i32..51_i32).map(|i| Entity::new(format!("entity-{i}"), "type").unwrap()).collect();
        let err = validate_entities(&entities).unwrap_err();
        assert!(err.to_string().contains("exceeds maximum"), "error should mention exceeding max count");
    }

    #[test]
    fn validate_entities_empty_name() {
        let entities = vec![Entity {
            name: String::new(),
            entity_type: EntityType::new("type").unwrap(),
        }];
        let err = validate_entities(&entities).unwrap_err();
        assert!(err.to_string().contains("name"), "error should mention entity name");
    }

    #[test]
    fn validate_entities_empty_entity_type_impossible() {
        // With EntityType newtype, empty entity types are rejected at construction time.
        assert!(EntityType::new("").is_none(), "EntityType::new rejects empty strings");
        assert!(EntityType::new("   ").is_none(), "EntityType::new rejects blank strings");
    }

    #[test]
    fn validate_entities_name_exceeds_max_length() {
        let long_name = "a".repeat(257);
        let entities = vec![Entity {
            name: long_name,
            entity_type: EntityType::new("type").unwrap(),
        }];
        let err = validate_entities(&entities).unwrap_err();
        assert!(err.to_string().contains("name"), "error should mention entity name");
        assert!(err.to_string().contains("length"), "error should mention length");
    }

    #[test]
    fn validate_entities_type_exceeds_max_length() {
        let long_type = "t".repeat(257);
        let entities = vec![Entity {
            name: "Alice".into(),
            entity_type: EntityType::new(long_type).unwrap(),
        }];
        let err = validate_entities(&entities).unwrap_err();
        assert!(err.to_string().contains("type"), "error should mention entity type");
        assert!(err.to_string().contains("length"), "error should mention length");
    }

    #[test]
    fn validate_entities_valid_input() {
        let entities = vec![Entity::new("Alice", "person").unwrap(), Entity::new("example-agent", "project").unwrap()];
        validate_entities(&entities).unwrap();
    }

    // -- RR-116: unit tests for validation functions -------------------------

    // -- ttl_seconds_to_expiry -----------------------------------------------

    #[test]
    fn ttl_seconds_to_expiry_valid_duration() {
        let now = chrono::Utc::now();
        let result = ttl_seconds_to_expiry(3600, now).unwrap();
        let diff = result.signed_duration_since(now);
        assert_eq!(diff.num_seconds(), 3600);
    }

    #[test]
    fn ttl_seconds_to_expiry_zero_is_valid() {
        let now = chrono::Utc::now();
        let result = ttl_seconds_to_expiry(0, now).unwrap();
        assert_eq!(result, now);
    }

    #[test]
    fn ttl_seconds_to_expiry_u64_max_is_too_large() {
        let now = chrono::Utc::now();
        let err = ttl_seconds_to_expiry(u64::MAX, now).unwrap_err();
        assert!(err.to_string().contains("too large"), "error: {err}");
    }

    // -- validate_optional_non_empty -----------------------------------------

    #[test]
    fn validate_optional_non_empty_none_is_ok() {
        validate_optional_non_empty("field", None).unwrap();
    }

    #[test]
    fn validate_optional_non_empty_non_blank_is_ok() {
        validate_optional_non_empty("field", Some("hello")).unwrap();
    }

    #[test]
    fn validate_optional_non_empty_empty_string_rejected() {
        let err = validate_optional_non_empty("field", Some("")).unwrap_err();
        assert!(err.to_string().contains("cannot be empty"), "error: {err}");
    }

    #[test]
    fn validate_optional_non_empty_whitespace_only_rejected() {
        let err = validate_optional_non_empty("field", Some("   ")).unwrap_err();
        assert!(err.to_string().contains("cannot be empty"), "error: {err}");
    }

    // -- validate_optional_string_array --------------------------------------

    #[test]
    fn validate_optional_string_array_none_is_ok() {
        validate_optional_string_array("tags", None).unwrap();
    }

    #[test]
    fn validate_optional_string_array_valid_items_ok() {
        let items = vec!["a".into(), "b".into()];
        validate_optional_string_array("tags", Some(&items)).unwrap();
    }

    #[test]
    fn validate_optional_string_array_empty_vec_ok() {
        let items: Vec<String> = vec![];
        validate_optional_string_array("tags", Some(&items)).unwrap();
    }

    #[test]
    fn validate_optional_string_array_blank_item_rejected() {
        let items = vec!["a".into(), "  ".into()];
        let err = validate_optional_string_array("tags", Some(&items)).unwrap_err();
        assert!(err.to_string().contains("empty values"), "error: {err}");
    }

    // -- validate_string_array -----------------------------------------------

    #[test]
    fn validate_string_array_valid() {
        let items = vec!["a".into(), "b".into()];
        validate_string_array("tags", &items).unwrap();
    }

    #[test]
    fn validate_string_array_blank_item_rejected() {
        let items = vec!["good".into(), String::new()];
        let err = validate_string_array("tags", &items).unwrap_err();
        assert!(err.to_string().contains("empty values"), "error: {err}");
    }

    // -- normalize_optional_non_empty ----------------------------------------

    #[test]
    fn normalize_optional_non_empty_trims_whitespace() {
        let result = normalize_optional_non_empty("f", Some("  hello  ".into())).unwrap();
        assert_eq!(result.as_deref(), Some("hello"));
    }

    #[test]
    fn normalize_optional_non_empty_none_passthrough() {
        let result = normalize_optional_non_empty("f", None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn normalize_optional_non_empty_blank_rejected() {
        let err = normalize_optional_non_empty("f", Some("   ".into())).unwrap_err();
        assert!(err.to_string().contains("cannot be empty"), "error: {err}");
    }

    // -- normalize_non_empty -------------------------------------------------

    #[test]
    fn normalize_non_empty_trims_whitespace() {
        let result = normalize_non_empty("f", "  hello  ").unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn normalize_non_empty_blank_rejected() {
        let err = normalize_non_empty("f", "   ").unwrap_err();
        assert!(err.to_string().contains("cannot be empty"), "error: {err}");
    }

    // -- validate_content_length ---------------------------------------------

    #[test]
    fn validate_content_length_within_limit() {
        validate_content_length("hello", 100).unwrap();
    }

    #[test]
    fn validate_content_length_at_exact_limit() {
        let content = "a".repeat(100);
        validate_content_length(&content, 100).unwrap();
    }

    #[test]
    fn validate_content_length_exceeds_limit() {
        let content = "a".repeat(101);
        let err = validate_content_length(&content, 100).unwrap_err();
        assert!(err.to_string().contains("exceeds maximum"), "error: {err}");
    }

    // -- validate_tags -------------------------------------------------------

    #[test]
    fn validate_tags_within_limits() {
        let tags = vec!["a".into(), "b".into()];
        validate_tags("tags", &tags, 10, 256).unwrap();
    }

    #[test]
    fn validate_tags_exceeds_max_count() {
        let tags = vec!["a".into(), "b".into(), "c".into()];
        let err = validate_tags("tags", &tags, 2, 256).unwrap_err();
        assert!(err.to_string().contains("exceeds maximum of 2 tags"), "error: {err}");
    }

    #[test]
    fn validate_tags_individual_tag_too_long() {
        let tags = vec!["a".repeat(300)];
        let err = validate_tags("tags", &tags, 10, 256).unwrap_err();
        assert!(err.to_string().contains("exceeding maximum length"), "error: {err}");
    }

    #[test]
    fn validate_tags_empty_list_ok() {
        validate_tags("tags", &[], 10, 256).unwrap();
    }

    // -- normalize_optional_string_array -------------------------------------

    #[test]
    fn normalize_optional_string_array_trims_items() {
        let items = vec!["  a  ".into(), "  b  ".into()];
        let result = normalize_optional_string_array("tags", Some(items)).unwrap();
        assert_eq!(result.as_deref(), Some(&["a".to_owned(), "b".to_owned()][..]));
    }

    #[test]
    fn normalize_optional_string_array_none_passthrough() {
        let result = normalize_optional_string_array("tags", None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn normalize_optional_string_array_blank_rejected() {
        let items = vec!["valid".into(), "   ".into()];
        let err = normalize_optional_string_array("tags", Some(items)).unwrap_err();
        assert!(err.to_string().contains("cannot contain empty values"), "error: {err}");
    }

    // -- parse_memory_id -----------------------------------------------------

    #[test]
    fn parse_memory_id_valid_ulid() {
        let id = MemoryId::new();
        let parsed = parse_memory_id(&id.to_string()).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn parse_memory_id_invalid_string() {
        let err = parse_memory_id("not-a-ulid").unwrap_err();
        assert!(err.to_string().contains("invalid memory id"), "error: {err}");
    }

    #[test]
    fn parse_memory_id_empty_string() {
        let err = parse_memory_id("").unwrap_err();
        assert!(err.to_string().contains("invalid memory id"), "error: {err}");
    }

    // -- normalize_entities --------------------------------------------------

    #[test]
    fn normalize_entities_trims_whitespace() {
        let entities = vec![Entity::new("  Alice  ", "  person  ").unwrap()];
        let normalized = normalize_entities(entities);
        assert_eq!(normalized[0].name, "Alice");
        assert_eq!(normalized[0].entity_type.as_str(), "person");
    }

    #[test]
    fn normalize_entities_empty_list() {
        let result = normalize_entities(vec![]);
        assert!(result.is_empty());
    }

    // -- validate_non_blank --------------------------------------------------

    #[test]
    fn validate_non_blank_valid() {
        validate_non_blank("field", "hello").unwrap();
    }

    #[test]
    fn validate_non_blank_empty_rejected() {
        let err = validate_non_blank("field", "").unwrap_err();
        assert!(err.to_string().contains("cannot be blank"), "error: {err}");
    }

    #[test]
    fn validate_non_blank_whitespace_only_rejected() {
        let err = validate_non_blank("field", "   ").unwrap_err();
        assert!(err.to_string().contains("cannot be blank"), "error: {err}");
    }

    // -- validate_max_distance -----------------------------------------------

    #[test]
    fn validate_max_distance_none_ok() {
        validate_max_distance(None).unwrap();
    }

    #[test]
    fn validate_max_distance_valid_value() {
        validate_max_distance(Some(1.5_f64)).unwrap();
    }

    #[test]
    fn validate_max_distance_zero_ok() {
        validate_max_distance(Some(0.0_f64)).unwrap();
    }

    #[test]
    fn validate_max_distance_negative_rejected() {
        let err = validate_max_distance(Some(-1.0_f64)).unwrap_err();
        assert!(err.to_string().contains("finite non-negative"), "error: {err}");
    }

    #[test]
    fn validate_max_distance_nan_rejected() {
        let err = validate_max_distance(Some(f64::NAN)).unwrap_err();
        assert!(err.to_string().contains("finite non-negative"), "error: {err}");
    }

    #[test]
    fn validate_max_distance_infinity_rejected() {
        let err = validate_max_distance(Some(f64::INFINITY)).unwrap_err();
        assert!(err.to_string().contains("finite non-negative"), "error: {err}");
    }

    #[test]
    fn validate_max_distance_exceeds_upper_bound() {
        let err = validate_max_distance(Some(11.0_f64)).unwrap_err();
        assert!(err.to_string().contains("exceeds maximum"), "error: {err}");
    }

    #[test]
    fn validate_max_distance_at_upper_bound_ok() {
        validate_max_distance(Some(10.0_f64)).unwrap();
    }

    // -- validate_entities: whitespace-only name/type treated as empty -------

    #[test]
    fn validate_entities_whitespace_name_rejected_at_construction() {
        // With Entity::new, blank names are rejected at construction (returns None).
        assert!(Entity::new("   ", "type").is_none(), "Entity::new rejects blank names");
    }

    #[test]
    fn validate_entities_whitespace_type_rejected_at_construction() {
        // With EntityType newtype, blank types are rejected at construction (returns None).
        assert!(Entity::new("Alice", "   ").is_none(), "Entity::new rejects blank types");
    }

    #[test]
    fn validate_entities_at_max_count_ok() {
        let entities: Vec<Entity> = (0_i32..50_i32).map(|i| Entity::new(format!("entity-{i}"), "type").unwrap()).collect();
        validate_entities(&entities).unwrap();
    }

    #[test]
    fn validate_entities_name_at_max_length_ok() {
        let name = "a".repeat(256);
        let entities = vec![Entity {
            name,
            entity_type: EntityType::new("type").unwrap(),
        }];
        validate_entities(&entities).unwrap();
    }

    #[test]
    fn validate_entities_empty_list_ok() {
        validate_entities(&[]).unwrap();
    }
}
