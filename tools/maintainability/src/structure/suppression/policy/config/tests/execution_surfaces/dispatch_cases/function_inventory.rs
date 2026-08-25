pub(in super::super) const FUNCTION_INVENTORY_CASES: &[(&str, &str)] = &[
    (
        "unset() { :; }\ndeclare -i value=0\npayload='a[$(sh quality/lint.txt)0]'\nunset value\nvalue=payload\n",
        "opaque interpreter program",
    ),
    ("declare() { :; }\ndeclare -i value=0\n", "opaque interpreter program"),
    ("read() { :; }\nread value\n", "opaque interpreter program"),
    ("cargo() \\\n{ :; }\ncargo check\n", "opaque interpreter program"),
    ("cargo()\n# declaration interruption\n{ :; }\ncargo check\n", "opaque interpreter program"),
    ("cargo() # declaration interruption\n{ :; }\ncargo check\n", "opaque interpreter program"),
    (
        "caller() {\n local -i value=0\n local payload='a[$(sh quality/lint.txt)0]'\n printf '%s' \"$(helper() { value=payload; }; helper)\"\n}\ncaller\n",
        "opaque interpreter program",
    ),
    (
        "caller() {\n local -i value=0\n local payload='a[$(sh quality/lint.txt)0]'\n printf '%s' `helper() { value=payload; }; helper`\n}\ncaller\n",
        "opaque interpreter program",
    ),
    (
        "caller() {\n local -i value=0\n local payload='a[$(sh quality/lint.txt)0]'\n cat <(helper() { value=payload; }; helper)\n}\ncaller\n",
        "opaque interpreter program",
    ),
    ("if true\nthen\nsh() { :; }\nfi\nsh quality/lint.txt\n", "opaque interpreter program"),
    ("outer() {\n sh() { :; }\n}\nouter\nsh quality/lint.txt\n", "opaque interpreter program"),
    (
        "caller() {\n local -i value=0\n local payload='a[$(sh quality/lint.txt)0]'\n printf '%s' \"${ helper() { value=payload; }; helper; }\"\n}\ncaller\n",
        "opaque interpreter program",
    ),
    (
        "caller() {\n local -i value=0\n local payload='a[$(sh quality/lint.txt)0]'\n printf '%s' \"${| helper() { value=payload; }; helper; }\"\n}\ncaller\n",
        "opaque interpreter program",
    ),
    (
        "caller() {\n local -i value=0\n local payload='a[$(sh quality/lint.txt)0]'\n printf '%s' \"${\\\n helper() { value=payload; }; helper; }\"\n}\ncaller\n",
        "opaque interpreter program",
    ),
    ("true && helper() { sh quality/lint.txt; }\nhelper\n", "opaque interpreter program"),
    ("false || helper() { sh quality/lint.txt; }\nhelper\n", "opaque interpreter program"),
    ("helper() { sh quality/lint.txt; } && helper\n", "opaque interpreter program"),
    ("helper() { sh quality/lint.txt; } | cat\n", "opaque interpreter program"),
    ("helper() { sh quality/lint.txt; } &\nwait\n", "opaque interpreter program"),
    ("let() { :; }\nlet value=1\n", "opaque interpreter program"),
    ("test() { :; }\ntest -n safe\n", "opaque interpreter program"),
    ("function . { :; }\n. quality/lint.txt\n", "unsupported sourced-file indirection"),
    ("perl() { :; }\nperl quality/lint.txt\n", "opaque interpreter program"),
    ("python3.13() { :; }\npython3.13 quality/lint.txt\n", "opaque interpreter program"),
    ("ksh() { :; }\nksh quality/lint.txt\n", "opaque interpreter program"),
    (
        "caller() {\n local -i value=0\n local payload='a[$(sh quality/lint.txt)0]'\n cat <<DOC\n${ helper() { value=payload; }; helper; }\nDOC\n}\ncaller\n",
        "opaque interpreter program",
    ),
    (
        "caller() {\n local -i value=0\n local payload='a[$(sh quality/lint.txt)0]'\n values=(\"${ helper() { value=payload; }; helper; }\")\n}\ncaller\n",
        "opaque interpreter program",
    ),
    (
        "caller() {\n local -i value=0\n local payload='a[$(sh quality/lint.txt)0]'\n case x in\n ${ helper() { value=payload; }; helper; }x) : ;;\n esac\n}\ncaller\n",
        "opaque interpreter program",
    ),
    (
        "printf '%s' $'x\\'' >/dev/null\ncaller() { local -i value=0; local payload='a[$(sh quality/lint.txt)0]'; printf '%s' \"${ helper() { value=payload; }; helper; }\"; }\ncaller\n",
        "opaque interpreter program",
    ),
    ("set -euo pipefail\nhelper() { :; } 2>/dev/null && helper\n", "opaque interpreter program"),
    ("set -euo pipefail\nhelper() { :; } <<<safe && helper\n", "opaque interpreter program"),
    (
        "printf '%s' \"$(printf '%s' '\"')\" >/dev/null\ncaller() { local -i value=0; local payload='a[$(sh quality/lint.txt)0]'; printf '%s' \"${ helper() { value=payload; }; helper; }\"; }\ncaller\n",
        "opaque interpreter program",
    ),
    (
        "printf '%s' \"`printf '%s' '\"'`\" >/dev/null\ncaller() { local -i value=0; local payload='a[$(sh quality/lint.txt)0]'; printf '%s' \"${ helper() { value=payload; }; helper; }\"; }\ncaller\n",
        "opaque interpreter program",
    ),
    (
        "printf '%s' \"$(printf '%s' \"$(printf '%s' '\"')\")\" >/dev/null\ncaller() { local -i value=0; local payload='a[$(sh quality/lint.txt)0]'; printf '%s' \"${ helper() { value=payload; }; helper; }\"; }\ncaller\n",
        "opaque interpreter program",
    ),
    (
        "printf '%s' \"$(printf '%s' '\"')\" >/dev/null\nvalues=(<\\\n(cargo() { :; }; cargo check))\n",
        "opaque interpreter program",
    ),
    (
        "printf '%s' \"$\\\n(printf '%s' '\"')\" >/dev/null\nvalues=(<\\\n(pkg-config() { sh quality/lint.txt; }; pkg-config))\n",
        "opaque interpreter program",
    ),
    (
        "printf '%s' \"$\\\r\n(printf '%s' '\"')\" >/dev/null\r\nvalues=(>\\\r\n(pkg-config() { sh quality/lint.txt; }; pkg-config))\r\n",
        "opaque interpreter program",
    ),
    (
        "printf '%s' \"$\\\n\\\n(printf '%s' '\"')\" >/dev/null\nvalues=(<\\\n(pkg-config() { sh quality/lint.txt; }; pkg-config))\n",
        "opaque interpreter program",
    ),
    (
        "cat <\\\n<'DOC' >/dev/null\n'\nDOC\nvalues=(<\\\n(pkg-config() { sh quality/lint.txt; }; pkg-config))\n",
        "opaque interpreter program",
    ),
    (
        "printf '%s' \"$(printf '%s' \"$(printf '%s' '\"')\")\" >/dev/null\nvalues=(<\\\n(cargo() { :; }; cargo check))\n",
        "opaque interpreter program",
    ),
    (
        "printf '%s' \"`printf '%s' '\"'`\" >/dev/null\nvalues=(<\\\n(cargo() { :; }; cargo check))\n",
        "opaque interpreter program",
    ),
    ("values=(<\\\n(cargo() { :; }; cargo check))\n", "opaque interpreter program"),
    ("values=(\n  >\\\r\n(just() { :; }; just check-quality)\n)\n", "opaque interpreter program"),
    (
        "printf '%s' $'x\\'' >/dev/null\nvalues=(<\\\n(cargo() { :; }; cargo check))\n",
        "opaque interpreter program",
    ),
    (
        "printf '%s' $'x\\'' >/dev/null\nvalues=(\n  >\\\n(just() { :; }; just check-quality)\n)\n",
        "opaque interpreter program",
    ),
    (
        "printf '%s' \"$(ca\\\nse x in x) printf '%s' '\"' ;; esac)\" >/dev/null\ncargo check\n",
        "opaque interpreter program",
    ),
    ("values=(<(cargo() { :; }; cargo check))\n", "opaque interpreter program"),
    ("values=(\n  >(just() { :; }; just check-quality)\n)\n", "opaque interpreter program"),
    (
        "printf '%s' \"$(case x in x) printf '%s' '\"' ;; esac)\" >/dev/null\nprintf '%s' \"${ helper() { :; }; helper; }\"\n",
        "opaque interpreter program",
    ),
    ("printf '%s' \"$(cat <<'DOC'\n)\n\"\nDOC\n)\" >/dev/null\nprintf '%s' safe\n", "opaque interpreter program"),
];
