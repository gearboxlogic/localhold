#!/usr/bin/env bash
set -euo pipefail

failed=0

while IFS= read -r path; do
  [[ -e "$path" || -L "$path" ]] || continue
  case "$path" in
    .agents/*|.claude/*|.codex/*|tasks/*|docs/plans/*|docs/review/*|CLAUDE.md|recall.example.toml|script/promote-mcp-local.sh)
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
  'gizmo[-_ ]recall'
  'Gearbox-Logic'
  'github-gbl'
  '/home/[^/]+/(dev|src|projects)/'
  'C:\\Users\\'
)

for marker in "${markers[@]}"; do
  if matches="$(git grep -n -I -i -E "$marker" -- . ':(exclude)script/check-publication-hygiene.sh' || true)" && [[ -n "$matches" ]]; then
    printf 'forbidden publication marker %q:\n%s\n' "$marker" "$matches" >&2
    failed=1
  fi
done

if (( failed != 0 )); then
  exit 1
fi

printf 'publication hygiene checks passed\n'
