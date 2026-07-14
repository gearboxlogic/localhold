#!/usr/bin/env bash
# Preview of the LocalHold CLI output design (assets/brand/cli.md).
# Uses truecolor; run in any modern terminal.
set -euo pipefail

if [[ ! -t 1 || -n "${NO_COLOR:-}" ]]; then
  echo "cli-preview: needs a color TTY (this is the point of the preview)"; exit 1
fi

OR=$'\e[38;2;200;155;60m'
AZ=$'\e[38;2;127;163;212m'
VERT=$'\e[38;2;107;163;131m'
GULES=$'\e[38;2;200;106;97m'
DIM=$'\e[2m'
BOLD=$'\e[1m'
R=$'\e[0m'

rule="${DIM}${OR}▀▀▀▀▀▀▀▀    ▀▀▀▀▀▀▀▀    ▀▀▀▀▀▀▀▀    ▀▀▀▀▀▀▀▀${R}"

echo
echo "${BOLD}localhold${R} ${DIM}· CLI output preview${R}"
echo
echo "$rule"
echo
echo "${DIM}message shapes${R}"
echo
echo "${VERT}✓ held${R}      Memory saved to scope ${AZ}project:localhold${R}."
echo "${GULES}✗ not held${R}  Scope ${AZ}team:atlas${R} isn't registered."
echo "            Register it with: hold admin scope register team:atlas"
echo "${OR}! watch${R}     Embedding endpoint answered in 4.2s; searches will feel slow."
echo "${DIM}· note      Storage is local. This command sent nothing anywhere.${R}"
echo
echo "$rule"
echo
echo "${DIM}ledger table${R}"
echo
echo "${DIM}SCOPE                 MEMORIES  VECTORS  LAST WRITE${R}"
echo "${AZ}project:localhold${R}          412      412  2026-07-12"
echo "${AZ}user:jeff${R}                  118      118  2026-07-13"
echo "${AZ}team:atlas${R}                    7        0  2026-06-30"
echo
echo "$rule"
echo
echo "${DIM}progress${R}"
echo
echo "reindexing vectors  ${OR}412${R}/1024"
echo "${VERT}✓ held${R}      1024 vectors rebuilt in 2m 14s."
echo
