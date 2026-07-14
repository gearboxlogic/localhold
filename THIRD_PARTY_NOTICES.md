# Third-Party Notices

LocalHold depends on third-party Rust crates listed in `Cargo.lock`. Their
licenses remain their own and can be inspected with:

```sh
cargo deny list
```

The optional reranker can download the Apache-2.0-licensed
[`cross-encoder/ms-marco-MiniLM-L6-v2`](https://huggingface.co/cross-encoder/ms-marco-MiniLM-L6-v2)
model and tokenizer at the immutable revision recorded in `src/config.rs`.
Those artifacts are fetched from the upstream model repository at runtime and
are not part of this source repository.

The standard CPU build statically incorporates Microsoft ONNX Runtime through
`ort 2.0.0-rc.10`. ONNX Runtime is licensed under the MIT License:

> Copyright (c) Microsoft Corporation. All rights reserved.
>
> Permission is hereby granted, free of charge, to any person obtaining a copy
> of this software and associated documentation files (the "Software"), to deal
> in the Software without restriction, including without limitation the rights
> to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
> copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in
> all copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
> FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
> AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
> LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
> SOFTWARE.

CUDA, cuDNN, NVIDIA drivers, CUDA-profile ONNX Runtime shared libraries,
embedding models, and external model servers are not bundled in this source
repository. The Linux x86_64 CUDA 12 release archive does redistribute the
manifested ONNX Runtime, CUDA 12.8, and cuDNN 9.8 user-space libraries. That
archive includes the upstream ONNX Runtime license and third-party notices plus
the license supplied with every NVIDIA component under `licenses/` and
`notices/`. The NVIDIA kernel/driver library is never bundled. Users and other
distributors remain responsible for reviewing the terms that apply to their
distribution and use. Every binary distribution must ship a notice inventory
matching the components actually bundled in that artifact.
