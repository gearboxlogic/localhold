//! Canonical `PostgreSQL` migration identities and ledger classification.

use std::collections::HashSet;

use sqlx_core::{query::query, query_scalar::query_scalar, row::Row as _};
use sqlx_postgres::PgPool;

use crate::error::StoreError;

/// One durable `PostgreSQL` migration identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MigrationIdentity {
    version: i64,
    name: &'static str,
}

impl MigrationIdentity {
    pub(crate) const fn version(self) -> i64 {
        self.version
    }

    pub(crate) const fn name(self) -> &'static str {
        self.name
    }
}

/// Ordered identities recognized by this binary.
pub(crate) const MIGRATIONS: &[MigrationIdentity] = &[
    MigrationIdentity {
        version: 1,
        name: "bootstrap_schema",
    },
    MigrationIdentity {
        version: 2,
        name: "audit_log_without_memory_fk",
    },
    MigrationIdentity {
        version: 3,
        name: "record_revision",
    },
    MigrationIdentity {
        version: 4,
        name: "published_v2_metadata",
    },
];

/// Latest `PostgreSQL` schema version recognized by this binary.
pub(crate) const CURRENT_SCHEMA_VERSION: i64 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// Compatibility of the database migration ledger with this binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrationMetadataState {
    Absent,
    Current,
    Repairable,
    Incompatible,
    Newer,
}

pub(crate) async fn read_migration_metadata_state(pool: &PgPool) -> Result<MigrationMetadataState, StoreError> {
    let table_exists: bool = query_scalar("SELECT to_regclass(format('%I.%I', current_schema(), 'localhold_migrations')) IS NOT NULL")
        .fetch_one(pool)
        .await?;
    if !table_exists {
        return Ok(MigrationMetadataState::Absent);
    }

    let rows = query("SELECT version, name FROM localhold_migrations ORDER BY version")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|row| Ok((row.try_get("version")?, row.try_get("name")?)))
        .collect::<Result<Vec<(i64, String)>, sqlx_core::Error>>()?;
    Ok(classify_migration_rows(&rows))
}

pub(crate) fn classify_migration_rows(rows: &[(i64, String)]) -> MigrationMetadataState {
    if rows.iter().any(|(version, _name)| *version > CURRENT_SCHEMA_VERSION) {
        return MigrationMetadataState::Newer;
    }

    let mut versions = HashSet::new();
    for (version, name) in rows {
        if !versions.insert(*version) {
            return MigrationMetadataState::Incompatible;
        }
        let Some(expected) = MIGRATIONS.iter().find(|migration| migration.version == *version) else {
            return MigrationMetadataState::Incompatible;
        };
        if expected.name != name {
            return MigrationMetadataState::Incompatible;
        }
    }

    if rows.len() == MIGRATIONS.len() {
        MigrationMetadataState::Current
    } else {
        MigrationMetadataState::Repairable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_has_unique_strictly_increasing_identities() {
        let mut previous = None;
        let mut names = HashSet::new();
        for migration in MIGRATIONS {
            assert!(previous.is_none_or(|version| migration.version > version));
            assert!(names.insert(migration.name));
            previous = Some(migration.version);
        }
        assert_eq!(previous, Some(CURRENT_SCHEMA_VERSION));
    }

    #[test]
    fn classifies_current_repairable_incompatible_and_newer_ledgers() {
        let current = MIGRATIONS.iter().map(|migration| (migration.version, migration.name.to_owned())).collect::<Vec<_>>();
        assert_eq!(classify_migration_rows(&current), MigrationMetadataState::Current);
        assert_eq!(classify_migration_rows(&current[..2]), MigrationMetadataState::Repairable);
        assert_eq!(classify_migration_rows(&[(1_i64, "wrong".into())]), MigrationMetadataState::Incompatible);
        assert_eq!(
            classify_migration_rows(&[(1_i64, "bootstrap_schema".into()), (1_i64, "bootstrap_schema".into())]),
            MigrationMetadataState::Incompatible
        );
        assert_eq!(classify_migration_rows(&[(CURRENT_SCHEMA_VERSION + 1_i64, "future".into())]), MigrationMetadataState::Newer);
    }
}
