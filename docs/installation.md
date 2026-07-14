# Installation

LocalHold is not published to crates.io. GitHub prereleases provide CPU binary
archives for Linux x86_64 and Windows x86_64. Building a locked checkout remains
available for other prefixes and the CUDA preview profile.

## Release Archives

Download the archive for the release and its `SHA256SUMS` file from
[GitHub Releases](https://github.com/gearboxlogic/localhold/releases). Release
archives use these names:

- `localhold-vVERSION-x86_64-unknown-linux-gnu.tar.zst`
- `localhold-vVERSION-x86_64-pc-windows-msvc.zip` (preview)

Verify and extract the Linux archive:

```sh
sha256sum --check --ignore-missing SHA256SUMS
tar --zstd -xf localhold-vVERSION-x86_64-unknown-linux-gnu.tar.zst
./localhold-vVERSION-x86_64-unknown-linux-gnu/bin/hold --help
```

Linux archive extraction requires `zstd` and a tar implementation with zstd
support. This is separate from the dependencies needed to build from source.

On Windows, compare the value from `Get-FileHash -Algorithm SHA256` with the
corresponding `SHA256SUMS` entry, then use `Expand-Archive`. Every archive has a
single versioned root containing `bin/hold` (or `hold.exe`),
`localhold.example.toml`, maintained documentation, the changelog, and license
notices.

Linux archives are built on Ubuntu 22.04 and require glibc 2.35 or newer plus
the normal C++ runtime library. Windows archives require a supported Windows
installation with the Microsoft Visual C++ Redistributable. Both archives
include CPU reranker support, which remains disabled until configured.

## Build From Source

### Standard CPU Build Requirements

- Git
- Rust 1.97 with Cargo
- C and C++ compilers
- CMake
- Make or Ninja
- network access to download Rust crates and the pinned ONNX Runtime artifact

The default installation includes the CPU reranker. Its dependency tree builds
bundled SQLite, Oniguruma, and AWS-LC code and downloads the pinned ONNX Runtime
binary. On Linux, the ONNX Runtime download client also requires `pkg-config`
and OpenSSL development headers.

Install the native prerequisites using the package manager for the host:

```sh
# Fedora
sudo dnf install gcc gcc-c++ cmake make pkgconf-pkg-config openssl-devel

# Debian/Ubuntu
sudo apt install build-essential cmake pkg-config libssl-dev

# macOS with Homebrew (after installing Xcode Command Line Tools)
brew install cmake pkg-config
```

The checked-in `rust-toolchain.toml` pins Rust 1.97 for rustup users. Project
contributors may instead install the complete pinned development toolset with
`mise install`; `mise` is not required by the release installer.

### Optional Dependencies

The following are not required for the standard CPU installation:

- `mise`, `just`, nextest, cargo-deny, gitleaks, and ShellCheck are development
  or CI tools. See [Contributing](../CONTRIBUTING.md) when working on the source.
- A PostgreSQL server with `pgvector` is required only when selecting the
  PostgreSQL backend. SQLite is bundled and remains the default.
- An OpenAI-compatible embedding endpoint is required only for semantic or
  hybrid vector search. The default `noop` provider supports local text search
  without a model server.
- The NVIDIA driver, CUDA, cuDNN, and CUDA-enabled ONNX Runtime are required
  only for the CUDA reranker profile described below. The CPU reranker does not
  require them.
- Docker and PostgreSQL client tools are used only by the PostgreSQL smoke-test
  workflow; they are not application dependencies.
- Python 3 is used only by maintainers and CI to create release archives; it is
  not required to build or run LocalHold.

Clone a tagged release, review the tag and release notes, then install the CPU
build for the current user:

```sh
git clone --branch v0.1.0-beta.3 --depth 1 \
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

### Windows Preview

Install Git, Rust 1.97 with Cargo, CMake, and Visual Studio 2022 Build Tools with
the **Desktop development with C++** workload. Build the preview binary from a
Developer PowerShell prompt:

```powershell
cargo build --release --locked --features reranker
.\target\release\hold.exe --help
```

The POSIX `script/install.sh` installer is not currently supported on Windows.
Windows compilation and native tests run in CI, but packaging and installer
integration remain preview work.

### CUDA Reranker Preview

Build and install the CUDA reranker variant with:

```sh
./script/install.sh --profile cuda
```

This compiles ONNX Runtime's CUDA execution provider alongside CPU support. The
runtime `execution_provider` policy selects which provider is used; building
the CUDA profile alone does not claim that CUDA is active. The current `ort
2.0.0-rc.10` integration targets the ONNX Runtime 1.22 ABI. Install a
CUDA-enabled ONNX Runtime 1.22 build plus the CUDA and cuDNN versions required
by that build, then set `ORT_DYLIB_PATH` to the absolute path of
`libonnxruntime.so` when it is outside the dynamic loader's normal search path.

The fused FP32 reranker artifact is the runtime default. The optional fused
FP16 artifact requires `execution_provider = "cuda"`; it cannot use `auto`
fallback. See [Operations](operations.md#reranker-model-precision) for the
speed, memory, and ranking-quality tradeoffs.

Keep every CUDA dependency for the reranker in one toolkit family. In
particular, do not place CUDA 13 directories ahead of a CUDA 12 runtime in
`LD_LIBRARY_PATH`: libraries with stable sonames such as `libcurand.so.10` can
otherwise resolve from the wrong toolkit even when the remaining dependencies
resolve from CUDA 12. Before starting LocalHold, run `ldd` on
`libonnxruntime_providers_cuda.so` and confirm that every CUDA and cuDNN library
is found in the intended runtime family.
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

Native operating-system packages remain a future distribution surface. They
must preserve the documented binary/profile split and include the same notices,
example configuration, and checksums as release archives.
