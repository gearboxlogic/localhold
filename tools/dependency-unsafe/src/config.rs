use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const REQUIRED_REVIEW_TRIGGERS: [&str; 4] = ["version-or-checksum-change", "enabled-feature-change", "new-dependency-route", "exposure-signal-change"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditConfig {
    pub schema_version: u32,
    pub cargo_version: String,
    pub rustc_version: String,
    pub platforms: Vec<PlatformConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformConfig {
    pub name: String,
    pub target: String,
    pub baseline: String,
    pub configurations: Vec<GraphConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphConfig {
    pub id: String,
    pub profile: String,
    #[serde(default)]
    pub all_features: bool,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub include_dev: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Classification {
    GeneratedBindings,
    MatureFfi,
    PureRustUnchecked,
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassificationFragment {
    schema_version: u32,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    review_triggers: Option<Vec<String>>,
    #[serde(default)]
    packages: BTreeMap<String, ClassificationEntry>,
}

#[derive(Debug)]
pub struct ClassificationPolicy {
    pub schema_version: u32,
    pub owner: String,
    pub review_triggers: Vec<String>,
    pub packages: BTreeMap<String, ClassificationEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassificationEntry {
    pub classification: Classification,
    pub rationale: String,
    pub review_issue: u64,
}

impl AuditConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read audit matrix {}", path.display()))?;
        let config: Self = serde_json::from_slice(&bytes).with_context(|| format!("parse audit matrix {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn platform(&self, name: &str) -> Result<&PlatformConfig> {
        self.platforms
            .iter()
            .find(|platform| platform.name == name)
            .with_context(|| format!("audit platform {name:?} is not configured"))
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!("unsupported audit matrix schema {}", self.schema_version);
        }
        if self.platforms.is_empty() {
            bail!("audit matrix has no platforms");
        }
        let mut platform_names = BTreeSet::new();
        let mut baselines = BTreeSet::new();
        for platform in &self.platforms {
            validate_platform(platform, &mut platform_names, &mut baselines)?;
        }
        let expected = BTreeSet::from(["linux".to_owned(), "windows".to_owned()]);
        let actual: BTreeSet<_> = self.platforms.iter().map(|platform| platform.name.clone()).collect();
        if actual != expected {
            bail!("audit matrix must define exactly the linux and windows platforms");
        }
        Ok(())
    }
}

fn validate_platform<'a>(platform: &'a PlatformConfig, names: &mut BTreeSet<&'a String>, baselines: &mut BTreeSet<&'a String>) -> Result<()> {
    let expected_target = match platform.name.as_str() {
        "linux" => "x86_64-unknown-linux-gnu",
        "windows" => "x86_64-pc-windows-msvc",
        _ => bail!("unsupported audit platform {:?}", platform.name),
    };
    if platform.target != expected_target {
        bail!("audit platform {:?} must use target {expected_target:?}", platform.name);
    }
    let expected_baseline = format!("policy/dependency-unsafe/baseline/{}", platform.name);
    if platform.baseline != expected_baseline {
        bail!("audit platform {:?} must use baseline {expected_baseline:?}", platform.name);
    }
    if !names.insert(&platform.name) {
        bail!("duplicate audit platform {:?}", platform.name);
    }
    if !baselines.insert(&platform.baseline) {
        bail!("duplicate audit baseline {:?}", platform.baseline);
    }
    if platform.target.trim().is_empty() || platform.configurations.is_empty() {
        bail!("platform {:?} is incomplete", platform.name);
    }
    let mut ids = BTreeSet::new();
    for configuration in &platform.configurations {
        validate_graph(configuration, &platform.name, &mut ids)?;
    }
    Ok(())
}

fn validate_graph<'a>(configuration: &'a GraphConfig, platform: &str, ids: &mut BTreeSet<&'a String>) -> Result<()> {
    if !valid_slug(&configuration.id) || !configuration.id.starts_with(&format!("{platform}-")) {
        bail!("configuration ID {:?} must be a platform-prefixed lowercase slug", configuration.id);
    }
    if !ids.insert(&configuration.id) {
        bail!("duplicate configuration {:?} for {platform}", configuration.id);
    }
    if !matches!(configuration.profile.as_str(), "dev" | "test" | "release") {
        bail!("unsupported Cargo profile {:?} in {}", configuration.profile, configuration.id);
    }
    if configuration.all_features && !configuration.features.is_empty() {
        bail!("{} cannot combine all_features with explicit features", configuration.id);
    }
    Ok(())
}

fn valid_slug(value: &str) -> bool {
    !value.is_empty() && value.len() <= 80 && value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

impl ClassificationPolicy {
    pub fn load(path: &Path) -> Result<Self> {
        let mut entries: Vec<_> = fs::read_dir(path)
            .with_context(|| format!("read classification policy directory {}", path.display()))?
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        if entries.is_empty() {
            bail!("classification policy directory {} has no JSON files", path.display());
        }
        for entry in &entries {
            if !entry.is_file() || entry.extension().and_then(|extension| extension.to_str()) != Some("json") {
                bail!("classification policy directory contains unsupported entry {}", entry.display());
            }
        }
        let mut owner = None;
        let mut review_triggers = None;
        let mut packages = BTreeMap::new();
        for file in entries {
            let bytes = fs::read(&file).with_context(|| format!("read classification policy {}", file.display()))?;
            let fragment: ClassificationFragment = serde_json::from_slice(&bytes).with_context(|| format!("parse classification policy {}", file.display()))?;
            if fragment.schema_version != 1 {
                bail!("unsupported classification schema {} in {}", fragment.schema_version, file.display());
            }
            validate_fragment_placement(&file, fragment.packages.keys())?;
            merge_setting(&mut owner, fragment.owner, "owner", &file)?;
            merge_setting(&mut review_triggers, fragment.review_triggers, "review_triggers", &file)?;
            merge_classifications(&mut packages, fragment.packages)?;
        }
        let policy = Self {
            schema_version: 1,
            owner: owner.context("classification policy has no owner")?,
            review_triggers: review_triggers.context("classification policy has no review trigger set")?,
            packages,
        };
        policy.validate()?;
        Ok(policy)
    }

    pub fn classification(&self, source_id: &str) -> Option<Classification> {
        self.packages.get(source_id).map(|entry| entry.classification)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!("unsupported classification schema {}", self.schema_version);
        }
        if self.owner.trim().is_empty() {
            bail!("classification policy owner is empty");
        }
        if !self.review_triggers.iter().map(String::as_str).eq(REQUIRED_REVIEW_TRIGGERS) {
            bail!("classification policy must use the complete ordered review trigger set");
        }
        for (source_id, entry) in &self.packages {
            if !valid_source_id(source_id) {
                bail!("classification key {source_id:?} is not a normalized crates.io identity");
            }
            if entry.rationale.trim().is_empty() || entry.review_issue == 0 {
                bail!("classification for {source_id} lacks review metadata");
            }
        }
        Ok(())
    }

    pub fn source_ids(&self) -> BTreeSet<String> {
        self.packages.keys().cloned().collect()
    }
}

fn validate_fragment_placement<'a>(file: &Path, source_ids: impl Iterator<Item = &'a String>) -> Result<()> {
    let stem = file.file_stem().and_then(|stem| stem.to_str()).context("classification fragment name is not UTF-8")?;
    if stem == "policy" {
        if source_ids.count() != 0 {
            bail!("classification policy settings file must not contain package entries");
        }
        return Ok(());
    }
    let (start, end) = match stem {
        "a-d" => ('a', 'd'),
        "e-h" => ('e', 'h'),
        "i-l" => ('i', 'l'),
        "m-p" => ('m', 'p'),
        "q-t" => ('q', 't'),
        "u-z" => ('u', 'z'),
        _ => bail!("unsupported classification fragment name {stem:?}"),
    };
    for source_id in source_ids {
        let initial = source_id
            .strip_prefix("crates.io:")
            .and_then(|package| package.chars().next())
            .map(|initial| initial.to_ascii_lowercase())
            .with_context(|| format!("classification key {source_id:?} has no package-name initial"))?;
        if !(start..=end).contains(&initial) {
            bail!("classification key {source_id:?} is misplaced in {stem}.json");
        }
    }
    Ok(())
}

fn valid_source_id(source_id: &str) -> bool {
    let Some(package) = source_id.strip_prefix("crates.io:") else {
        return false;
    };
    let Some((name_version, checksum)) = package.rsplit_once('#') else {
        return false;
    };
    let Some((name, version)) = name_version.rsplit_once('@') else {
        return false;
    };
    !name.is_empty()
        && !version.is_empty()
        && !name.bytes().any(|byte| byte.is_ascii_whitespace())
        && !version.bytes().any(|byte| byte.is_ascii_whitespace())
        && checksum.len() == 64
        && checksum.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn merge_setting<T>(current: &mut Option<T>, incoming: Option<T>, name: &str, file: &Path) -> Result<()> {
    if let Some(value) = incoming
        && current.replace(value).is_some()
    {
        bail!("classification policy setting {name:?} is defined more than once, including {}", file.display());
    }
    Ok(())
}

fn merge_classifications(policy: &mut BTreeMap<String, ClassificationEntry>, fragment: BTreeMap<String, ClassificationEntry>) -> Result<()> {
    for (source_id, entry) in fragment {
        if policy.insert(source_id.clone(), entry).is_some() {
            bail!("duplicate classification for {source_id}");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;
