use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use sha2::{Digest, Sha256};
use syn::ext::IdentExt;
use syn::visit::{self, Visit};

use super::model::SOURCE_ROOT;

struct ModuleParents {
    module: String,
    parents: Vec<PathBuf>,
}

struct TargetRoots {
    production: BTreeSet<String>,
    testing: BTreeSet<String>,
}

const LEGACY_ANALYZER_MANIFEST_SHA256: &str = "cca207767614bd2c1d46bc06092b69e90157aeb450797fcc7cad4e1ed67c89b9";
const LEGACY_AUTO_TARGET_PATHS: &[&str] = &[
    "tools/maintainability/src/lib.rs",
    "tools/maintainability/src/bin/",
    "tools/maintainability/examples/",
    "tools/maintainability/tests/",
    "tools/maintainability/benches/",
];

pub(super) fn validate_legacy_auto_target_inventory(workspace: &Path, manifest: &str, repository_paths: &BTreeSet<String>) -> Result<()> {
    if !legacy_analyzer_manifest_is_exact(manifest) {
        return Ok(());
    }
    if let Some(path) = repository_paths.iter().find(|path| {
        let normalized = path.to_ascii_lowercase();
        LEGACY_AUTO_TARGET_PATHS.iter().any(|candidate| {
            let root = candidate.strip_suffix('/').unwrap_or(candidate);
            normalized == root || normalized.starts_with(candidate)
        })
    }) {
        bail!("legacy maintainability analyzer manifest cannot authenticate auto-discovered Cargo target {path:?}");
    }
    for candidate in LEGACY_AUTO_TARGET_PATHS {
        let candidate = candidate.strip_suffix('/').unwrap_or(candidate);
        match std::fs::symlink_metadata(workspace.join(candidate)) {
            Ok(_) => bail!("legacy maintainability analyzer manifest cannot authenticate physical auto-discovered Cargo target {candidate:?}"),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("inspect legacy Cargo auto-target path {candidate:?}")),
        }
    }
    Ok(())
}

pub(super) fn test_only_paths(sources: &BTreeMap<String, String>, manifest: &str) -> Result<BTreeSet<String>> {
    let target_roots = target_roots(manifest, sources)?;
    let mut ordered = sources.keys().cloned().collect::<Vec<_>>();
    ordered.sort_by_key(|path| Path::new(path).components().count());
    let mut test_only = target_roots.testing.difference(&target_roots.production).cloned().collect::<BTreeSet<_>>();
    loop {
        let before = test_only.len();
        for path in &ordered {
            if path_is_test_only(path, sources, &target_roots, &test_only)? {
                test_only.insert(path.clone());
            }
        }
        if test_only.len() == before {
            break;
        }
    }
    Ok(test_only)
}

fn path_is_test_only(path: &str, sources: &BTreeMap<String, String>, target_roots: &TargetRoots, test_only: &BTreeSet<String>) -> Result<bool> {
    if target_roots.production.contains(path) || test_only.contains(path) {
        return Ok(false);
    }
    let Some(module_parents) = module_parents(path, target_roots) else {
        return Ok(false);
    };
    let mut saw_test_edge = false;
    let mut saw_production_edge = false;
    for parent in module_parents.parents {
        let parent = normalized_internal_path(&parent);
        let Some(source) = sources.get(&parent) else {
            continue;
        };
        match (test_only.contains(&parent), module_edge_is_test_only(source, &parent, &module_parents.module)?) {
            (true, Some(_)) | (false, Some(true)) => saw_test_edge = true,
            (false, Some(false)) => saw_production_edge = true,
            (_, None) => {}
        }
    }
    Ok(saw_test_edge && !saw_production_edge)
}

pub(super) fn validate_compiler_inputs(sources: &BTreeMap<String, String>, manifest: &str) -> Result<()> {
    let parsed = manifest
        .parse::<toml::Table>()
        .context("parse maintainability analyzer Cargo manifest for compiler inputs")?;
    let package = parsed
        .get("package")
        .and_then(toml::Value::as_table)
        .context("maintainability analyzer Cargo manifest requires a package table")?;
    if !legacy_analyzer_manifest_is_exact(manifest) {
        for field in ["build", "autobins", "autoexamples", "autotests", "autobenches"] {
            if package.get(field) != Some(&toml::Value::Boolean(false)) {
                bail!("maintainability analyzer Cargo package.{field} must be false to close the authenticated compiler-input inventory");
            }
        }
    }
    validate_dependency_sources(&parsed, package)?;
    target_roots(manifest, sources)?;
    for (path, source) in sources {
        let syntax = syn::parse_file(source).with_context(|| format!("parse maintainability analyzer compiler input {path:?}"))?;
        let mut visitor = CompilerInputVisitor {
            path,
            inline_module_depth: 0,
            error: None,
        };
        visitor.visit_file(&syntax);
        if let Some(error) = visitor.error {
            return Err(error);
        }
    }
    Ok(())
}

fn legacy_analyzer_manifest_is_exact(manifest: &str) -> bool {
    format!("{:x}", Sha256::digest(manifest.as_bytes())) == LEGACY_ANALYZER_MANIFEST_SHA256
}

fn validate_dependency_sources(manifest: &toml::Table, package: &toml::Table) -> Result<()> {
    if package.contains_key("workspace") {
        bail!("maintainability analyzer Cargo package.workspace is outside the closed compiler-input inventory");
    }
    if manifest.get("workspace").and_then(toml::Value::as_table).is_none_or(|workspace| !workspace.is_empty()) {
        bail!("maintainability analyzer Cargo workspace must remain an empty standalone workspace");
    }
    for unsupported in ["target", "patch", "replace"] {
        if manifest.contains_key(unsupported) {
            bail!("maintainability analyzer Cargo {unsupported} tables are outside the closed compiler-input inventory");
        }
    }
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(dependencies) = manifest.get(kind) else {
            continue;
        };
        let dependencies = dependencies
            .as_table()
            .with_context(|| format!("maintainability analyzer Cargo [{kind}] must be a table"))?;
        for (name, dependency) in dependencies {
            match dependency {
                toml::Value::String(_) => {}
                toml::Value::Table(properties)
                    if properties
                        .keys()
                        .all(|key| matches!(key.as_str(), "version" | "features" | "default-features" | "optional" | "package")) => {}
                toml::Value::Table(_) => {
                    bail!("maintainability analyzer Cargo dependency {name:?} in [{kind}] uses an unsupported external or inherited source");
                }
                _ => bail!("maintainability analyzer Cargo dependency {name:?} in [{kind}] must use a registry version specification"),
            }
        }
    }
    Ok(())
}

fn module_edge_is_test_only(source: &str, parent: &str, module: &str) -> Result<Option<bool>> {
    let syntax = syn::parse_file(source).with_context(|| format!("parse maintainability analyzer module parent {parent:?}"))?;
    let mut matched = false;
    let mut all_test_only = true;
    for item in syntax.items {
        let syn::Item::Mod(item) = item else {
            continue;
        };
        if item.ident.unraw() == module && item.content.is_none() {
            matched = true;
            all_test_only &= item.attrs.iter().any(exact_test_cfg);
        }
    }
    Ok(matched.then_some(all_test_only))
}

fn target_roots(source: &str, sources: &BTreeMap<String, String>) -> Result<TargetRoots> {
    let manifest = source.parse::<toml::Table>().context("parse maintainability analyzer Cargo manifest for target roots")?;
    let production = [format!("{SOURCE_ROOT}/main.rs"), format!("{SOURCE_ROOT}/lib.rs")]
        .into_iter()
        .filter(|path| sources.contains_key(path))
        .collect::<BTreeSet<_>>();
    let mut roots = TargetRoots {
        production,
        testing: BTreeSet::new(),
    };
    if let Some(library) = manifest.get("lib") {
        add_declared_path(library, "lib", true, sources, &mut roots)?;
    }
    for (kind, production) in [("bin", true), ("example", true), ("test", false), ("bench", false)] {
        let Some(targets) = manifest.get(kind) else {
            continue;
        };
        let targets = targets
            .as_array()
            .with_context(|| format!("maintainability analyzer Cargo [[{kind}]] declarations must be an array"))?;
        for target in targets {
            add_declared_path(target, kind, production, sources, &mut roots)?;
        }
    }
    Ok(roots)
}

fn add_declared_path(target: &toml::Value, kind: &str, production: bool, sources: &BTreeMap<String, String>, roots: &mut TargetRoots) -> Result<()> {
    let table = target.as_table().with_context(|| format!("maintainability analyzer Cargo {kind} target must be a table"))?;
    let Some(path) = table.get("path") else {
        if kind != "lib" {
            bail!("maintainability analyzer Cargo {kind} targets require explicit profiled paths");
        }
        return Ok(());
    };
    let path = path.as_str().with_context(|| format!("maintainability analyzer Cargo {kind}.path must be a string"))?;
    let path = target_path(path)?;
    if !sources.contains_key(&path) {
        bail!("maintainability analyzer Cargo {kind} target {path:?} must be a profiled Rust source under {SOURCE_ROOT:?}");
    }
    if production {
        roots.production.insert(path);
    } else {
        roots.testing.insert(path);
    }
    Ok(())
}

fn target_path(path: &str) -> Result<String> {
    if path.contains('\\') {
        bail!("maintainability analyzer Cargo target paths must use forward slashes on every platform");
    }
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_)))
    {
        bail!("maintainability analyzer Cargo target path must remain relative and cannot traverse parents");
    }
    let resolved = normalized_internal_path(&Path::new("tools/maintainability").join(path));
    if !super::model::is_source(&resolved) {
        bail!("maintainability analyzer Cargo target path must be a profiled Rust source under {SOURCE_ROOT:?}");
    }
    Ok(resolved)
}

fn normalized_internal_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

struct CompilerInputVisitor<'a> {
    path: &'a str,
    inline_module_depth: usize,
    error: Option<anyhow::Error>,
}

impl<'ast> Visit<'ast> for CompilerInputVisitor<'_> {
    fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
        if self.error.is_some() {
            return;
        }
        let tokens = attribute.meta.to_token_stream();
        if attribute.path().segments.last().is_some_and(|segment| segment.ident.unraw() == "path") || tokens_contain_path_attribute(tokens.clone()) {
            self.error = Some(anyhow::anyhow!(
                "maintainability analyzer source {:?} uses a path attribute outside the closed compiler-input inventory",
                self.path
            ));
            return;
        }
        if tokens_contain_include_identifier(tokens) {
            self.error = Some(anyhow::anyhow!(
                "maintainability analyzer source {:?} uses an attribute compiler input outside the closed compiler-input inventory",
                self.path
            ));
            return;
        }
        visit::visit_attribute(self, attribute);
    }

    fn visit_macro(&mut self, macro_: &'ast syn::Macro) {
        if self.error.is_none()
            && (macro_.path.segments.last().is_some_and(|segment| is_compiler_input_macro_ident(&segment.ident)) || tokens_contain_external_input(macro_.tokens.clone()))
        {
            self.error = Some(anyhow::anyhow!(
                "maintainability analyzer source {:?} uses macro syntax outside the closed compiler-input inventory",
                self.path
            ));
            return;
        }
        visit::visit_macro(self, macro_);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        if self.error.is_none() && tokens_contain_compiler_input_macro_identifier(item.to_token_stream()) {
            self.error = Some(anyhow::anyhow!(
                "maintainability analyzer source {:?} imports a compiler-input macro outside the closed compiler-input inventory",
                self.path
            ));
            return;
        }
        visit::visit_item_use(self, item);
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if self.error.is_some() {
            return;
        }
        if item.content.is_none() && self.inline_module_depth > 0 {
            self.error = Some(anyhow::anyhow!(
                "maintainability analyzer source {:?} declares an external module inside an inline module outside the closed compiler-input inventory",
                self.path
            ));
            return;
        }
        let enters_inline = item.content.is_some();
        self.inline_module_depth += usize::from(enters_inline);
        visit::visit_item_mod(self, item);
        self.inline_module_depth -= usize::from(enters_inline);
    }
}

fn tokens_contain_external_input(tokens: TokenStream) -> bool {
    let trees = tokens.into_iter().collect::<Vec<_>>();
    trees.iter().any(|tree| match tree {
        TokenTree::Group(group) => (group.delimiter() == Delimiter::Bracket && tokens_contain_path_attribute(group.stream())) || tokens_contain_external_input(group.stream()),
        TokenTree::Ident(ident) => is_compiler_input_macro_ident(ident) || ident.unraw() == "mod",
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn tokens_contain_include_identifier(tokens: TokenStream) -> bool {
    tokens.into_iter().any(|tree| match tree {
        TokenTree::Group(group) => tokens_contain_include_identifier(group.stream()),
        TokenTree::Ident(ident) => matches!(ident.unraw().to_string().as_str(), "include" | "include_str" | "include_bytes"),
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn tokens_contain_compiler_input_macro_identifier(tokens: TokenStream) -> bool {
    tokens.into_iter().any(|tree| match tree {
        TokenTree::Group(group) => tokens_contain_compiler_input_macro_identifier(group.stream()),
        TokenTree::Ident(ident) => is_compiler_input_macro_ident(&ident),
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn is_compiler_input_macro_ident(ident: &proc_macro2::Ident) -> bool {
    matches!(
        ident.unraw().to_string().as_str(),
        "include" | "include_str" | "include_bytes" | "asm" | "global_asm" | "naked_asm" | "llvm_asm"
    )
}

fn tokens_contain_path_attribute(tokens: TokenStream) -> bool {
    let trees = tokens.into_iter().collect::<Vec<_>>();
    trees
        .windows(2)
        .any(|pair| matches!(&pair[0], TokenTree::Ident(ident) if ident.unraw() == "path") && matches!(&pair[1], TokenTree::Punct(punct) if punct.as_char() == '='))
        || trees.iter().any(|tree| match tree {
            TokenTree::Group(group) => tokens_contain_path_attribute(group.stream()),
            TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => false,
        })
}

fn module_parents(path: &str, target_roots: &TargetRoots) -> Option<ModuleParents> {
    let relative = Path::new(path).strip_prefix(SOURCE_ROOT).ok()?;
    let stem = relative.file_stem()?.to_str()?;
    if matches!(stem, "main" | "lib") && relative.parent().is_some_and(|parent| parent.as_os_str().is_empty()) {
        return None;
    }
    let (module, parent_module) = if stem == "mod" {
        let module_directory = relative.parent()?;
        (
            module_directory.file_name()?.to_str()?.to_owned(),
            module_directory.parent().unwrap_or_else(|| Path::new("")),
        )
    } else {
        (stem.to_owned(), relative.parent().unwrap_or_else(|| Path::new("")))
    };
    let root = Path::new(SOURCE_ROOT);
    let mut parents = if parent_module.as_os_str().is_empty() {
        BTreeSet::from([root.join("main.rs"), root.join("lib.rs")])
    } else {
        BTreeSet::from([root.join(parent_module).with_extension("rs"), root.join(parent_module).join("mod.rs")])
    };
    let module_directory = root.join(parent_module);
    for target in target_roots.production.iter().chain(&target_roots.testing) {
        if target != path && Path::new(target).parent() == Some(module_directory.as_path()) {
            parents.insert(PathBuf::from(target));
        }
    }
    Some(ModuleParents {
        module,
        parents: parents.into_iter().collect(),
    })
}

fn exact_test_cfg(attribute: &syn::Attribute) -> bool {
    attribute.path().is_ident("cfg") && attribute.parse_args::<syn::Path>().is_ok_and(|path| path.is_ident("test"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed_manifest(extra: &str) -> String {
        format!("workspace = {{}}\n[package]\nname = 'maintainability'\nbuild = false\nautobins = false\nautoexamples = false\nautotests = false\nautobenches = false\n{extra}")
    }

    #[test]
    fn only_the_exact_legacy_analyzer_manifest_can_defer_auto_target_hardening() {
        let manifest = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).expect("read legacy analyzer manifest");
        let sources = BTreeMap::from([(format!("{SOURCE_ROOT}/main.rs"), "fn main() {}\n".to_owned())]);
        assert!(legacy_analyzer_manifest_is_exact(&manifest));
        validate_compiler_inputs(&sources, &manifest).expect("exact legacy manifest");

        let changed = format!("{manifest}\n# unreviewed change\n");
        assert!(!legacy_analyzer_manifest_is_exact(&changed));
        let error = validate_compiler_inputs(&sources, &changed).expect_err("changed legacy manifest");
        assert!(error.to_string().contains("package.autobins must be false"), "{error:#}");
    }

    #[test]
    fn legacy_analyzer_manifest_rejects_every_conventional_auto_target_root() {
        let manifest = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).expect("read legacy analyzer manifest");
        let workspace = tempfile::tempdir().expect("temporary workspace");
        for path in [
            "tools/maintainability/src/lib.rs",
            "tools/maintainability/src/bin",
            "tools/maintainability/src/bin/escape.rs",
            "tools/maintainability/examples",
            "tools/maintainability/examples/escape.rs",
            "tools/maintainability/tests",
            "tools/maintainability/tests/escape.rs",
            "tools/maintainability/benches",
            "tools/maintainability/benches/escape.rs",
            "TOOLS/MAINTAINABILITY/SRC/LIB.RS",
            "tools/maintainability/SRC/bin",
            "tools/maintainability/SRC/bin/escape.rs",
            "tools/maintainability/Examples",
            "tools/maintainability/Examples/escape.rs",
            "tools/maintainability/Tests",
            "tools/maintainability/Tests/escape.rs",
            "tools/maintainability/Benches",
            "tools/maintainability/Benches/escape.rs",
        ] {
            let error = validate_legacy_auto_target_inventory(workspace.path(), &manifest, &BTreeSet::from([path.to_owned()])).expect_err("legacy auto target");
            assert!(error.to_string().contains(path), "{error:#}");
        }
        validate_legacy_auto_target_inventory(workspace.path(), &manifest, &BTreeSet::from([format!("{SOURCE_ROOT}/main.rs")])).expect("reviewed main target");
    }

    #[test]
    fn legacy_analyzer_manifest_rejects_physical_auto_target_roots_outside_git_inventory() {
        let manifest = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).expect("read legacy analyzer manifest");
        for path in [
            "tools/maintainability/src/lib.rs",
            "tools/maintainability/src/bin",
            "tools/maintainability/examples",
            "tools/maintainability/tests",
            "tools/maintainability/benches",
        ] {
            let workspace = tempfile::tempdir().expect("temporary workspace");
            let physical = workspace.path().join(path);
            if Path::new(path).extension().is_some() {
                std::fs::create_dir_all(physical.parent().expect("auto-target parent")).expect("auto-target parent");
                std::fs::write(&physical, "fn main() {}\n").expect("physical auto target");
            } else {
                std::fs::create_dir_all(&physical).expect("physical auto-target root");
            }
            let error = validate_legacy_auto_target_inventory(workspace.path(), &manifest, &BTreeSet::new()).expect_err("physical legacy auto target");
            assert!(error.to_string().contains(path), "{error:#}");
        }
    }

    #[test]
    fn only_cfg_test_module_reachability_earns_the_test_limit() {
        let sources = BTreeMap::from([
            (
                format!("{SOURCE_ROOT}/main.rs"),
                "#[cfg(test)] mod reviewed;\nmod production;\n#[cfg(any(test, unix))] mod conditional;\n".to_owned(),
            ),
            (format!("{SOURCE_ROOT}/reviewed.rs"), "mod nested;\n".to_owned()),
            (format!("{SOURCE_ROOT}/reviewed/nested.rs"), "fn helper() {}\n".to_owned()),
            (format!("{SOURCE_ROOT}/production.rs"), "fn production() {}\n".to_owned()),
            (format!("{SOURCE_ROOT}/conditional.rs"), "fn conditional() {}\n".to_owned()),
        ]);
        let test_only = test_only_paths(&sources, "[package]\nname = 'maintainability'\n").expect("classify modules");
        assert!(test_only.contains(&format!("{SOURCE_ROOT}/reviewed.rs")));
        assert!(test_only.contains(&format!("{SOURCE_ROOT}/reviewed/nested.rs")));
        assert!(!test_only.contains(&format!("{SOURCE_ROOT}/production.rs")));
        assert!(!test_only.contains(&format!("{SOURCE_ROOT}/conditional.rs")));
    }

    #[test]
    fn nested_mod_rs_test_reachability_reaches_a_fixed_point() {
        let nested = format!("{SOURCE_ROOT}/foo/bar.rs");
        let sources = BTreeMap::from([
            (format!("{SOURCE_ROOT}/main.rs"), "#[cfg(test)] mod foo;\n".to_owned()),
            (format!("{SOURCE_ROOT}/foo/mod.rs"), "mod bar;\n".to_owned()),
            (nested.clone(), "fn bar() {}\n".to_owned()),
        ]);
        let classified = test_only_paths(&sources, "[package]\nname = 'maintainability'\n").expect("classify nested mod.rs");
        assert!(classified.contains(&format!("{SOURCE_ROOT}/foo/mod.rs")));
        assert!(classified.contains(&nested));
    }

    #[test]
    fn internally_computed_paths_use_profile_separators() {
        assert_eq!(
            normalized_internal_path(Path::new(r"tools\maintainability\src\main.rs")),
            "tools/maintainability/src/main.rs"
        );
    }

    #[test]
    fn a_production_reachable_tests_named_file_keeps_the_production_limit() {
        let sources = BTreeMap::from([
            (format!("{SOURCE_ROOT}/main.rs"), "mod tests;\n".to_owned()),
            (format!("{SOURCE_ROOT}/tests.rs"), "fn production() {}\n".to_owned()),
        ]);
        assert!(test_only_paths(&sources, "[package]\nname = 'maintainability'\n").expect("classify modules").is_empty());
    }

    #[test]
    fn every_matching_module_declaration_participates_in_classification() {
        for declarations in [
            "#[cfg(test)] mod shared;\n#[cfg(not(test))] mod shared;\n",
            "#[cfg(not(test))] mod shared;\n#[cfg(test)] mod shared;\n",
        ] {
            let sources = BTreeMap::from([
                (format!("{SOURCE_ROOT}/main.rs"), declarations.to_owned()),
                (format!("{SOURCE_ROOT}/shared.rs"), "fn shared() {}\n".to_owned()),
            ]);
            let classified = test_only_paths(&sources, "[package]\nname = 'maintainability'\n").expect("classify duplicate declarations");
            assert!(!classified.contains(&format!("{SOURCE_ROOT}/shared.rs")), "{declarations}");
        }
    }

    #[test]
    fn production_parent_or_cargo_target_wins_over_test_only_reachability() {
        let shared = format!("{SOURCE_ROOT}/shared.rs");
        let sources = BTreeMap::from([
            (format!("{SOURCE_ROOT}/main.rs"), "#[cfg(test)] mod shared;\n".to_owned()),
            (format!("{SOURCE_ROOT}/lib.rs"), "mod shared;\n".to_owned()),
            (shared.clone(), "fn shared() {}\n".to_owned()),
        ]);
        let classified = test_only_paths(&sources, "[package]\nname = 'maintainability'\n").expect("classify multiple parents");
        assert!(!classified.contains(&shared));

        let only_test_parent = BTreeMap::from([
            (format!("{SOURCE_ROOT}/main.rs"), "#[cfg(test)] mod shared;\n".to_owned()),
            (shared.clone(), "fn shared() {}\n".to_owned()),
        ]);
        let manifest = "[package]\nname = 'maintainability'\n[[bin]]\nname = 'shared'\npath = 'src/shared.rs'\n";
        let classified = test_only_paths(&only_test_parent, manifest).expect("classify explicit target");
        assert!(!classified.contains(&shared));
    }

    #[test]
    fn explicit_test_and_bench_targets_do_not_gain_production_classification() {
        let shared = format!("{SOURCE_ROOT}/shared.rs");
        let sources = BTreeMap::from([
            (format!("{SOURCE_ROOT}/main.rs"), "fn main() {}\n".to_owned()),
            (shared.clone(), "fn shared() {}\n".to_owned()),
        ]);
        for kind in ["test", "bench"] {
            let manifest = format!("[package]\nname = 'maintainability'\n[[{kind}]]\nname = 'shared'\npath = 'src/shared.rs'\n");
            let classified = test_only_paths(&sources, &manifest).expect("classify explicit test target");
            assert!(classified.contains(&shared), "{kind}");
        }
    }

    #[test]
    fn custom_target_roots_propagate_their_reachability_to_child_modules() {
        let helper = format!("{SOURCE_ROOT}/helper.rs");
        let sources = BTreeMap::from([
            (format!("{SOURCE_ROOT}/main.rs"), "#[cfg(test)] mod helper;\n".to_owned()),
            (format!("{SOURCE_ROOT}/cli.rs"), "mod helper;\n".to_owned()),
            (helper.clone(), "fn helper() {}\n".to_owned()),
        ]);
        let production = "[package]\nname = 'maintainability'\n[[bin]]\nname = 'cli'\npath = 'src/cli.rs'\n";
        let classified = test_only_paths(&sources, production).expect("classify custom production root child");
        assert!(!classified.contains(&helper));

        let testing_sources = BTreeMap::from([
            (format!("{SOURCE_ROOT}/main.rs"), "fn main() {}\n".to_owned()),
            (format!("{SOURCE_ROOT}/cli.rs"), "mod helper;\n".to_owned()),
            (helper.clone(), "fn helper() {}\n".to_owned()),
        ]);
        let testing = "[package]\nname = 'maintainability'\n[[test]]\nname = 'cli'\npath = 'src/cli.rs'\n";
        let classified = test_only_paths(&testing_sources, testing).expect("classify custom test root child");
        assert!(classified.contains(&helper));
    }

    #[test]
    fn test_roots_do_not_claim_undeclared_sibling_sources() {
        let unrelated = format!("{SOURCE_ROOT}/unrelated.rs");
        let dead_nested = format!("{SOURCE_ROOT}/tests/dead.rs");
        let sources = BTreeMap::from([
            (format!("{SOURCE_ROOT}/main.rs"), "#[cfg(test)] mod tests;\n".to_owned()),
            (format!("{SOURCE_ROOT}/test_cli.rs"), "fn test_entry() {}\n".to_owned()),
            (format!("{SOURCE_ROOT}/tests.rs"), "fn tests() {}\n".to_owned()),
            (unrelated.clone(), "fn unrelated() {}\n".to_owned()),
            (dead_nested.clone(), "fn dead() {}\n".to_owned()),
        ]);
        let manifest = "[package]\nname = 'maintainability'\n[[test]]\nname = 'test-cli'\npath = 'src/test_cli.rs'\n";
        let classified = test_only_paths(&sources, manifest).expect("classify unrelated siblings");
        assert!(!classified.contains(&unrelated));
        assert!(!classified.contains(&dead_nested));
    }

    #[test]
    fn compiler_inventory_rejects_external_targets_path_overrides_and_includes() {
        let main = format!("{SOURCE_ROOT}/main.rs");
        let ordinary = BTreeMap::from([(main.clone(), "fn main() {}\n".to_owned())]);
        for declaration in [
            "[[bin]]\nname = 'outside'\npath = 'quality/helper.rs'\n",
            "[[bin]]\nname = 'backslash'\npath = 'src\\main.rs'\n",
            "[[test]]\nname = 'outside'\npath = '../quality/helper.rs'\n",
            "[[example]]\nname = 'outside'\npath = 'examples/helper.txt'\n",
            "[[bench]]\nname = 'implicit-outside'\n",
        ] {
            let error = validate_compiler_inputs(&ordinary, &closed_manifest(declaration)).unwrap_err();
            assert!(
                error.to_string().contains("profiled Rust source")
                    || error.to_string().contains("cannot traverse parents")
                    || error.to_string().contains("must use forward slashes")
                    || error.to_string().contains("require explicit profiled paths"),
                "{declaration}: {error:#}"
            );
        }

        for source in [
            "#[path = \"../quality/helper.rs\"] mod helper;\nfn main() {}\n",
            "include!(\"../quality/helper.rs\");\nfn main() {}\n",
            "const POLICY: &str = include_str!(\"../policy.json\");\nfn main() {}\n",
            "const POLICY: &str = r#include_str!(\"../policy.json\");\nfn main() {}\n",
            "#![cfg_attr(not(test), doc = include_str!(\"../../outside.md\"))]\nfn main() {}\n",
            "#![r#cfg_attr(not(test), r#doc = r#include_bytes!(\"../../outside.bin\"))]\nfn main() {}\n",
            "use std::include as payload; payload!(\"../quality/helper.rs\");\nfn main() {}\n",
            "macro_rules! helper { () => { #[path = \"../quality/helper.rs\"] mod external; } }\nfn main() {}\n",
            "macro_rules! helper { () => { mod external; } }\nfn main() {}\n",
            "core::arch::global_asm!(r#\".incbin \\\"../../outside.bin\\\"\"#);\nfn main() {}\n",
            "fn main() { core::arch::asm!(r#\".include \\\"../../outside.s\\\"\"#); }\n",
            "use core::arch::global_asm as payload; payload!(\"nop\");\nfn main() {}\n",
            "macro_rules! helper { () => { naked_asm!(\"nop\"); } }\nfn main() {}\n",
            "fn main() { r#llvm_asm!(\"nop\"); }\n",
        ] {
            let sources = BTreeMap::from([(main.clone(), source.to_owned())]);
            let error = validate_compiler_inputs(&sources, &closed_manifest("")).unwrap_err();
            assert!(error.to_string().contains("compiler-input inventory"), "{source}: {error:#}");
        }
    }

    #[test]
    fn compiler_inventory_disables_implicit_cargo_target_discovery() {
        let sources = BTreeMap::from([(format!("{SOURCE_ROOT}/main.rs"), "fn main() {}\n".to_owned())]);
        for field in ["build", "autobins", "autoexamples", "autotests", "autobenches"] {
            let manifest = closed_manifest("").replace(&format!("{field} = false\n"), "");
            let error = validate_compiler_inputs(&sources, &manifest).unwrap_err();
            assert!(error.to_string().contains(&format!("package.{field}")), "{error:#}");
        }
    }

    #[test]
    fn compiler_inventory_rejects_external_and_inherited_dependency_sources() {
        let sources = BTreeMap::from([(format!("{SOURCE_ROOT}/main.rs"), "fn main() {}\n".to_owned())]);
        for declaration in [
            "[dependencies.helper]\npath = '../helper'\n",
            "[dev-dependencies.helper]\ngit = 'https://example.invalid/helper'\n",
            "[build-dependencies.helper]\nworkspace = true\n",
            "[target.'cfg(unix)'.dependencies]\nhelper = { path = '../helper' }\n",
            "[patch.crates-io]\nhelper = { path = '../helper' }\n",
            "[replace]\n'helper:1.0.0' = { path = '../helper' }\n",
        ] {
            let error = validate_compiler_inputs(&sources, &closed_manifest(declaration)).unwrap_err();
            assert!(
                error.to_string().contains("compiler-input inventory") || error.to_string().contains("external or inherited source"),
                "{declaration}: {error:#}"
            );
        }
        let inherited = closed_manifest("").replace("workspace = {}", "[workspace.dependencies]\nhelper = { path = '../helper' }");
        let error = validate_compiler_inputs(&sources, &inherited).unwrap_err();
        assert!(error.to_string().contains("workspace must remain an empty standalone workspace"), "{error:#}");
    }

    #[test]
    fn compiler_inventory_rejects_external_modules_nested_in_inline_modules() {
        let sources = BTreeMap::from([
            (format!("{SOURCE_ROOT}/main.rs"), "mod container { mod helper; }\nfn main() {}\n".to_owned()),
            (format!("{SOURCE_ROOT}/container.rs"), "#[cfg(test)] mod helper;\n".to_owned()),
            (format!("{SOURCE_ROOT}/container/helper.rs"), "fn helper() {}\n".to_owned()),
        ]);
        let error = validate_compiler_inputs(&sources, &closed_manifest("")).unwrap_err();
        assert!(error.to_string().contains("external module inside an inline module"), "{error:#}");
    }
}
