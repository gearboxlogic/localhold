#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
  printf 'publication hygiene must run inside a Git repository\n' >&2
  exit 1
}
cd "$repo_root"

if ! command -v gitleaks >/dev/null 2>&1; then
  printf 'gitleaks is required; install pinned tools with `mise install`\n' >&2
  exit 1
fi

if [[ "$(git rev-parse --is-shallow-repository)" == true ]]; then
  printf 'full Git history is required for secret scanning; fetch with `git fetch --unshallow --tags`\n' >&2
  exit 1
fi

failed=0

while IFS= read -r path; do
  [[ -e "$path" || -L "$path" ]] || continue
  case "$path" in
    .agents/*|.cache/*|.cargo/*|.claude/*|.codex/*|.rustup/*|target/*|tasks/*|docs/plans/*|docs/review/*|CLAUDE.md|script/promote-mcp-local.sh)
      printf 'forbidden publication path: %s\n' "$path" >&2
      failed=1
      ;;
  esac
done < <(git ls-files)

while IFS= read -r entry; do
  if [[ "$entry" == 120000* ]]; then
    printf 'tracked symlink requires explicit publication review: %s\n' "${entry##*$'\t'}" >&2
    failed=1
  fi
done < <(git ls-files --stage)

markers=(
  'giz''mo'
  'Gearbox-Logic'
  'github-gbl'
  '/home/[^/]+/(dev|src|projects)/'
  'C:\\Users\\'
)

for marker in "${markers[@]}"; do
  pathspecs=(. ':(exclude)script/check-publication-hygiene.sh')
  if matches="$(git grep -n -I -i -E "$marker" -- "${pathspecs[@]}" || true)" && [[ -n "$matches" ]]; then
    printf 'forbidden publication marker %q:\n%s\n' "$marker" "$matches" >&2
    failed=1
  fi
done

retired_names=(
  'Recall''Engine'
  'Recall''Server'
  'RECALL''_'
  'recall_''migrations'
  'recall[.]toml'
  'recall[.]db'
)

for marker in "${retired_names[@]}"; do
  if matches="$(git grep -n -I -E "$marker" -- . ':(exclude)script/check-publication-hygiene.sh' || true)" && [[ -n "$matches" ]]; then
    printf 'retired LocalHold name %q:\n%s\n' "$marker" "$matches" >&2
    failed=1
  fi
done

printf '%s\n' 'scanning all reachable Git history for secrets'
if ! gitleaks git \
  --config .github/gitleaks.toml \
  --redact \
  --no-banner \
  --verbose \
  --log-opts='--all --full-history --no-color' \
  .; then
  failed=1
fi

printf '%s\n' 'scanning the current working tree for secrets'
if ! gitleaks dir \
  --config .github/gitleaks.toml \
  --redact \
  --no-banner \
  --verbose \
  .; then
  failed=1
fi

if (( failed != 0 )); then
  exit 1
fi

printf 'publication hygiene checks passed\n'
