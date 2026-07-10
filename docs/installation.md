# Installation

LocalHold is not published to crates.io. The beta installation path builds a
locked checkout and installs the `hold` binary plus its example configuration
and notices.

## Prerequisites

- Git
- Rust 1.97 with Cargo
- a C compiler and standard build tools

Clone a tagged release, review the tag and release notes, then install the CPU
build for the current user:

```sh
git clone --branch v0.1.0-beta.1 --depth 1 \
  https://github.com/gearboxlogic/localhold.git
cd localhold
./script/install.sh
export PATH="$HOME/.local/bin:$PATH"
```

The default prefix is `~/.local`. Override it with `--prefix`, for example:

```sh
./script/install.sh --prefix /usr/local
```

Packagers can set `DESTDIR`; it is prepended to the selected prefix without
changing paths embedded in the package staging tree.

## CUDA Reranker Preview

Build and install the CUDA reranker variant with:

```sh
./script/install.sh --profile cuda
```

This enables ONNX Runtime's CUDA execution provider for reranking. The current
`ort 2.0.0-rc.12` integration targets the ONNX Runtime 1.24 ABI. Install a
CUDA-enabled ONNX Runtime 1.24 build plus the CUDA and cuDNN versions required
by that build, then set `ORT_DYLIB_PATH` to the absolute path of
`libonnxruntime.so` when it is outside the dynamic loader's normal search path.
Embedding placement is independent: embeddings are produced by the configured
OpenAI-compatible endpoint.

## Configuration

The installer does not create or overwrite user configuration. The installed
example is under `PREFIX/share/localhold/localhold.example.toml`. Place a
reviewed copy at the platform path documented in [Operations](operations.md).

Ensure `PREFIX/bin` is on `PATH`, then confirm the installed binary with:

```sh
hold --help
```

Release archives and native packages are future distribution surfaces. Their
contents must use this same binary/profile split and include the license,
notice, third-party notice, example configuration, and checksums.
