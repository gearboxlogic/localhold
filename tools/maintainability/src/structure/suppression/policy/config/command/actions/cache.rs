use anyhow::{Context, Result, bail};

use super::action_inputs;

pub(super) const ACTION: &str = "actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9";
const CARGO_PATHS: &[&str] = &[".cache/localhold/cargo/registry", ".cache/localhold/cargo/git", "target"];
const CUDA_PATH: &str = "${{ runner.temp }}/localhold-cuda-source-cache";

pub(super) fn validate_inputs(path: &str, lines: &[&str], uses_index: usize) -> Result<()> {
    let mut cache_paths = None;
    let mut key = None;
    let mut restore_keys = None;
    for input in action_inputs(path, lines, uses_index)? {
        let slot = match input.key {
            "path" => &mut cache_paths,
            "key" => &mut key,
            "restore-keys" => &mut restore_keys,
            _ => bail!("cache input {:?} in {path:?} is not reviewed", input.key),
        };
        if slot.replace(input.lines(path)?).is_some() {
            bail!("cache input {:?} in {path:?} must not be repeated", input.key);
        }
    }
    let cache_paths = cache_paths.with_context(|| format!("cache in {path:?} must declare one reviewed path set"))?;
    let key = key.with_context(|| format!("cache in {path:?} must declare one reviewed key"))?;
    if key.len() != 1 {
        bail!("cache key in {path:?} must be one literal line");
    }
    let restore_keys = restore_keys.unwrap_or_default();
    if !reviewed_profile(&cache_paths, &key[0], &restore_keys) {
        bail!("cache in {path:?} must use one exact reviewed path, key, and restore-key profile");
    }
    Ok(())
}

fn reviewed_profile(paths: &[String], key: &str, restore_keys: &[String]) -> bool {
    let cargo_paths = paths.iter().map(String::as_str).eq(CARGO_PATHS.iter().copied());
    let cargo_profile = cargo_paths
        && (key == "ubuntu-22.04-rust-${{ hashFiles('Cargo.lock', 'mise.lock') }}" && restore_keys == ["ubuntu-22.04-rust-"]
            || key == "${{ runner.os }}-rust-${{ hashFiles('Cargo.lock', 'mise.lock') }}" && restore_keys == ["${{ runner.os }}-rust-"]
            || key == "${{ runner.os }}-rust-outdated-${{ hashFiles('Cargo.lock', 'mise.lock') }}"
                && restore_keys == ["${{ runner.os }}-rust-outdated-", "${{ runner.os }}-rust-"]);
    cargo_profile || paths == [CUDA_PATH] && key == "localhold-cuda12-${{ hashFiles('release/cuda-linux-x86_64.json') }}" && restore_keys.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(inputs: &str) -> Result<()> {
        let source = format!("steps:\n  - uses: {ACTION}\n    with:\n      {inputs}\n");
        let lines = source.lines().collect::<Vec<_>>();
        validate_inputs(".github/workflows/test.yml", &lines, 1)
    }

    #[test]
    fn caches_require_exact_confined_profiles() {
        let cargo = "path: |\n        .cache/localhold/cargo/registry\n        .cache/localhold/cargo/git\n        target\n      key: ${{ runner.os }}-rust-${{ hashFiles('Cargo.lock', 'mise.lock') }}\n      restore-keys: ${{ runner.os }}-rust-";
        let outdated = "path: |\n        .cache/localhold/cargo/registry\n        .cache/localhold/cargo/git\n        target\n      key: ${{ runner.os }}-rust-outdated-${{ hashFiles('Cargo.lock', 'mise.lock') }}\n      restore-keys: |\n        ${{ runner.os }}-rust-outdated-\n        ${{ runner.os }}-rust-";
        let cuda = "path: ${{ runner.temp }}/localhold-cuda-source-cache\n      key: localhold-cuda12-${{ hashFiles('release/cuda-linux-x86_64.json') }}";
        for inputs in [cargo, outdated, cuda] {
            validate(inputs).expect("reviewed cache profile");
        }

        for inputs in [
            cargo.replace("        target", "        Justfile"),
            cargo.replace("${{ runner.os }}-rust-${{ hashFiles('Cargo.lock', 'mise.lock') }}", "attacker-controlled"),
            cargo.replace("path: |", "path: Justfile\n      path: |"),
            cuda.replace("${{ runner.temp }}/localhold-cuda-source-cache", "${{ github.workspace }}"),
        ] {
            assert!(validate(&inputs).is_err(), "accepted {inputs:?}");
        }
    }
}
