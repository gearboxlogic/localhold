# Third-Party Notices

LocalHold depends on third-party Rust crates listed in `Cargo.lock`. Their
licenses remain their own and can be inspected with:

```sh
cargo deny list
```

The optional reranker can download the
`cross-encoder/ms-marco-MiniLM-L-6-v2` model and tokenizer. Those artifacts are
not part of this source repository and retain their upstream terms.

CUDA, cuDNN, NVIDIA drivers, ONNX Runtime shared libraries, embedding models,
and external model servers are not bundled in this source repository. Users
and distributors are responsible for the terms that apply to the components
they select. Binary distributions must ship a notice inventory matching the
components actually bundled in that artifact.
