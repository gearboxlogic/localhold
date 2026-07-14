# Installation

LocalHold is not published to crates.io. GitHub prereleases provide CPU binary
archives for Linux x86_64 and Windows x86_64 plus a self-contained CUDA 12
reranker archive for Linux x86_64. Building a locked checkout remains available
for other prefixes and custom CUDA installations.

## Release Archives

Download the archive for the release and its `SHA256SUMS` file from
[GitHub Releases](https://github.com/gearboxlogic/localhold/releases). Release
archives use these names:

- `localhold-vVERSION-x86_64-unknown-linux-gnu.tar.zst`
- `localhold-vVERSION-x86_64-unknown-linux-gnu-cuda12.tar.zst`
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

The `cuda12` archive additionally contains ONNX Runtime 1.23.2, CUDA 12.8
user-space libraries, and cuDNN 9.8 in its private `lib/` directory. It requires
Linux x86_64, glibc 2.35 or newer, `libstdc++`, `libz`, a compatible NVIDIA GPU,
and NVIDIA Linux driver 570.26 or newer. CUDA 12.8 adds the SM100, SM101, and
SM120 Blackwell architectures while retaining CUDA 12 compatibility for older
supported NVIDIA GPUs. The archive does not require Python, vLLM, a
CUDA toolkit installation, cuDNN installation, or `LD_LIBRARY_PATH`. The NVIDIA
kernel/driver library remains host-owned and is deliberately not bundled.

After verifying `SHA256SUMS`, inspect the resolved native manifest if desired:

```sh
tar --zstd -xf localhold-vVERSION-x86_64-unknown-linux-gnu-cuda12.tar.zst
root=localhold-vVERSION-x86_64-unknown-linux-gnu-cuda12
cat "$root/manifest/cuda-runtime.json"
"$root/bin/hold" doctor --json
```

The manifest records every upstream input checksum, extracted library checksum,
compatibility floor, system-owned dependency, and provider policy. Component
license and notice files are under `licenses/` and `notices/`.

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
- The NVIDIA driver is required only for CUDA reranking. A source-built CUDA
  profile also needs compatible CUDA, cuDNN, and CUDA-enabled ONNX Runtime
  libraries; the `cuda12` release archive already carries those user-space
  libraries.
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

### Custom CUDA Source Build

Build and install the CUDA reranker variant with:

```sh
./script/install.sh --profile cuda
```

This compiles ONNX Runtime's CUDA execution provider alongside CPU support. The
runtime `execution_provider` policy selects which provider is used; building
the CUDA profile alone does not claim that CUDA is active. The pinned `ort
2.0.0-rc.10` bindings request ONNX Runtime's stable v22 C API. The CUDA profile
loads ONNX Runtime 1.23, which retains that earlier API table; this keeps the
standard CPU build compatible with the Ubuntu 22.04/glibc 2.35 release floor
without weakening the CUDA runtime pin. LocalHold suppresses the binding's
conservative newer-version warning after that API table is obtained; loader,
provider, and ONNX Runtime session diagnostics remain visible. Install a
CUDA-enabled ONNX Runtime 1.23 build plus the CUDA and cuDNN versions required
by that build, then set
`ORT_DYLIB_PATH` to the absolute path of
`libonnxruntime.so` when it is outside the dynamic loader's normal search path.
`hold doctor` reports this as a failed reranker check with loader guidance when
the library is not discoverable; it must not terminate with an ONNX loader panic.

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
