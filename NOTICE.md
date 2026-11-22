# NOTICE

**aden** — A Dense Referential Context Compiler.

© 2026 RioPlay <rioplay@rioplay.dev>.

Licensed under AGPL-3.0-or-later (see [LICENSE](LICENSE)).

This NOTICE file additionally lists the licenses of the third-party
dependencies aden builds against, for attribution and license-compatibility
purposes.

Maintainers refresh the dependency table with `python scripts/update_notice.py`;
this preserves the legal preamble and additional attribution below. CI verifies
the result with `python scripts/update_notice.py --check`.

## License compatibility

aden is distributed under AGPL-3.0-or-later. The overwhelming majority of its
dependencies are permissively licensed (MIT / Apache-2.0 and similar), which are
GPL/AGPL-compatible. The three non-permissive dependencies are used under
AGPL-compatible terms:

- **`self_cell`** — `Apache-2.0 OR GPL-2.0-only`. Used under the **Apache-2.0**
  branch, which is compatible with AGPL-3.0.
- **`option-ext`** — `MPL-2.0`. MPL is a file-level (weak) copyleft and is
  compatible with (A)GPL-licensed projects; it is consumed unmodified as an
  upstream crate.
- **`dyn-eq`** — `MPL-2.0`. Same weak-copyleft terms as `option-ext`; pulled
  (unmodified) only by the optional `dense` feature's `tract` ONNX stack.

The optional **`dense`** feature (local hybrid-retrieval embeddings, off by
default) additionally builds against `tract` / `tract-onnx` (`Apache-2.0 OR
MIT`), `kitoken` (`BSD-2-Clause`), and `fancy-regex` (`MIT`) — all
permissive and AGPL-compatible. The embedding model for this feature, the
MIT-licensed BAAI/bge-small-en-v1.5, is **fetched on demand** by the user
(`scripts/fetch-bge-model.sh`) rather than bundled; it is used under its own MIT
terms, with the license recorded alongside the downloaded files.

The optional **`model-fetch`** feature (off by default) adds the `aden model
fetch` command, which downloads that same model with checksum verification. It
compiles in `ureq` (`MIT OR Apache-2.0`); `sha2` (`MIT OR Apache-2.0`) is already
a default dependency. No model bytes enter aden's source tree.

The **`view`** feature (browser graph viewer, **on by default**) embeds one
vendored frontend asset: **`force-graph`** (vasturiano), **MIT**, pinned at
v1.52.0 and recorded with a sha256 in
[`crates/aden-cli/assets/CHECKSUMS`](crates/aden-cli/assets/CHECKSUMS). It is the
pre-built minified UMD bundle, inlined into the generated HTML so the page is fully
offline (no CDN, no runtime network). aden's build never runs npm. Because the
bundle is redistributed in the binary, its MIT notice is reproduced in full:

> Copyright (c) 2018 Vasco Asturiano
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of
> this software and associated documentation files (the "Software"), to deal in the
> Software without restriction, including without limitation the rights to use, copy,
> modify, merge, publish, distribute, sublicense, and/or sell copies of the Software,
> and to permit persons to whom the Software is furnished to do so, subject to the
> following conditions:
>
> The above copyright notice and this permission notice shall be included in all
> copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
> INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
> PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
> HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
> CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE
> OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

The same feature also embeds **`3d-force-graph`** (vasturiano), **MIT**, pinned
at v1.80.2 (sha256 in the same `CHECKSUMS` file) for the `aden view --3d`
orbital view. Its pre-built UMD bundle includes **three.js** (© 2010–2026
three.js authors, **MIT**). The three.js copyright notice is reproduced below:

> Copyright (c) 2010-present three.js authors
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
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
> THE SOFTWARE.

## Bundled reference material

The repository also contains third-party *reference material* used to inform
aden's secure-coding checks. See [`research/secure-coding/SOURCES.md`](research/secure-coding/SOURCES.md)
for full attribution. This material — which includes the OWASP Secure Coding
Practices (CC BY-SA 4.0, a ShareAlike license), the OWASP Top 10 (CC BY 3.0),
and the MITRE CWE corpus (MITRE CWE Terms of Use — not a Creative Commons license; see https://cwe.mitre.org/about/termsofuse.html) — is **not** part of aden's AGPL-licensed source and is
**not** compiled into the binary. It is retained for research/derivation only
and carries its own upstream licenses.

---

## Additional grammar attribution

**tree-sitter** (MIT): Copyright (c) 2018-2024 Max Brunsfeld.
Repository: https://github.com/tree-sitter/tree-sitter

**tree-sitter-language-pack** bundles compiled grammars from numerous upstream
repositories. Each grammar carries its own copyright holder and license. Full
per-grammar attribution is maintained by the pack at
https://github.com/kreuzberg-dev/tree-sitter-language-pack.

## Additional compression attribution

The `zstd`, `zstd-safe`, and `zstd-sys` crates carry the following identical
BSD-3-Clause notice in their `LICENSE` files, reproduced once for all three:

> BSD 3-Clause License
>
> Copyright (c) 2026, Alexandre Bury
>
> Redistribution and use in source and binary forms, with or without
> modification, are permitted provided that the following conditions are met:
>
> 1. Redistributions of source code must retain the above copyright notice, this
>    list of conditions and the following disclaimer.
>
> 2. Redistributions in binary form must reproduce the above copyright notice,
>    this list of conditions and the following disclaimer in the documentation
>    and/or other materials provided with the distribution.
>
> 3. Neither the name of the copyright holder nor the names of its
>    contributors may be used to endorse or promote products derived from
>    this software without specific prior written permission.
>
> THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
> AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
> IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
> DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
> FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
> DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
> SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
> CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
> OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
> OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

The `zstd-sys` crate additionally includes this notice in
`LICENSE.BSD-3-Clause` for its generated bindings:

> The auto-generated bindings are under the 3-clause BSD license:
>
> BSD License
>
> For Zstandard software
>
> Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
>
> Redistribution and use in source and binary forms, with or without modification,
> are permitted provided that the following conditions are met:
>
>  * Redistributions of source code must retain the above copyright notice, this
>    list of conditions and the following disclaimer.
>
>  * Redistributions in binary form must reproduce the above copyright notice,
>    this list of conditions and the following disclaimer in the documentation
>    and/or other materials provided with the distribution.
>
>  * Neither the name Facebook, nor Meta, nor the names of its contributors may
>    be used to endorse or promote products derived from this software without
>    specific prior written permission.
>
> THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
> ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
> WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
> DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
> ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
> (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
> LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON
> ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
> (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
> SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

# Third-Party Dependencies

Generated by `python scripts/update_notice.py` from locked Cargo metadata
with all workspace features enabled. Includes optional, development and
platform-specific dependencies; not every entry ships in every binary.

## Dependencies with Licenses

### adler2 v2.0.1

- **License**: 0BSD OR MIT OR Apache-2.0
- **Repository**: https://github.com/oyvindln/adler2

### ahash v0.8.12

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/tkaitchuck/ahash

### aho-corasick v1.1.5

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/aho-corasick

### alloca v0.4.0

- **License**: MIT
- **Repository**: https://github.com/playXE/alloca-rs

### allocator-api2 v0.2.21

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/zakarumych/allocator-api2

### anes v0.1.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/zrzka/anes-rs

### anstyle v1.0.14

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-cli/anstyle.git

### anyhow v1.0.104

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/anyhow

### anymap3 v1.1.0

- **License**: BlueOak-1.0.0 OR MIT OR Apache-2.0
- **Repository**: https://github.com/reivilibre/anymap3

### ar_archive_writer v0.5.5

- **License**: Apache-2.0 WITH LLVM-exception
- **Repository**: https://github.com/rust-lang/ar_archive_writer

### async-trait v0.1.92

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/async-trait

### auto_impl v1.4.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/auto-impl-rs/auto_impl/

### autocfg v1.5.1

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/cuviper/autocfg

### base64 v0.22.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/marshallpierce/rust-base64

### base64 v0.23.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/marshallpierce/rust-base64

### bit-set v0.5.3

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/contain-rs/bit-set

### bit-set v0.8.0

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/contain-rs/bit-set

### bit-vec v0.6.3

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/contain-rs/bit-vec

### bit-vec v0.8.0

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/contain-rs/bit-vec

### bitflags v1.3.2

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/bitflags/bitflags

### bitflags v2.13.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/bitflags/bitflags

### block-buffer v0.12.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RustCrypto/utils

### block2 v0.6.2

- **License**: MIT
- **Repository**: https://github.com/madsmtm/objc2

### bstr v1.14.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/BurntSushi/bstr

### bumpalo v3.20.4

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/fitzgen/bumpalo

### byteorder v1.5.0

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/byteorder

### bytes v1.12.2

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/bytes

### cast v0.3.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/japaric/cast.rs

### cc v1.11.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/cc-rs

### cfg-if v1.0.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/cfg-if

### cfg_aliases v0.2.2

- **License**: MIT
- **Repository**: https://github.com/katharostech/cfg_aliases

### ciborium v0.2.2

- **License**: Apache-2.0
- **Repository**: https://github.com/enarx/ciborium

### ciborium-io v0.2.2

- **License**: Apache-2.0
- **Repository**: https://github.com/enarx/ciborium

### ciborium-ll v0.2.2

- **License**: Apache-2.0
- **Repository**: https://github.com/enarx/ciborium

### clap v4.6.9

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/clap-rs/clap

### clap_builder v4.6.7

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/clap-rs/clap

### clap_derive v4.6.9

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/clap-rs/clap

### clap_lex v1.1.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/clap-rs/clap

### cobs v0.3.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/jamesmunns/cobs.rs

### combine v4.6.9

- **License**: MIT
- **Repository**: https://github.com/Marwes/combine

### const-oid v0.10.3

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/RustCrypto/formats

### const-random v0.1.18

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/tkaitchuck/constrandom

### const-random-macro v0.1.16

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/tkaitchuck/constrandom

### core-foundation v0.10.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/servo/core-foundation-rs

### core-foundation-sys v0.8.7

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/servo/core-foundation-rs

### cpufeatures v0.3.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RustCrypto/utils

### crc32fast v1.5.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/srijs/rust-crc32fast

### criterion v0.8.2

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/criterion-rs/criterion.rs

### criterion-plot v0.8.2

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/criterion-rs/criterion.rs

### crossbeam-deque v0.8.8

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/crossbeam-rs/crossbeam

### crossbeam-epoch v0.9.21

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/crossbeam-rs/crossbeam

### crossbeam-utils v0.8.23

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/crossbeam-rs/crossbeam

### crunchy v0.2.4

- **License**: MIT
- **Repository**: https://github.com/eira-fransham/crunchy

### crypto-common v0.2.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RustCrypto/traits

### ctrlc v3.5.3

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/Detegr/rust-ctrlc.git

### darling v0.24.1

- **License**: MIT
- **Repository**: https://github.com/TedDriggs/darling

### darling_core v0.24.1

- **License**: MIT
- **Repository**: https://github.com/TedDriggs/darling

### darling_macro v0.24.1

- **License**: MIT
- **Repository**: https://github.com/TedDriggs/darling

### dashmap v5.5.3

- **License**: MIT
- **Repository**: https://github.com/xacrimon/dashmap

### deranged v0.5.9

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/jhpratt/deranged

### derive-new v0.5.9

- **License**: MIT
- **Repository**: https://github.com/nrc/derive-new

### derive_more v2.1.1

- **License**: MIT
- **Repository**: https://github.com/JelteF/derive_more

### derive_more-impl v2.1.1

- **License**: MIT
- **Repository**: https://github.com/JelteF/derive_more

### digest v0.11.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RustCrypto/traits

### dirs v7.0.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://codeberg.org/dirs/dirs-rs

### dirs-sys v0.5.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dirs-dev/dirs-sys-rs

### dispatch2 v0.3.1

- **License**: Zlib OR Apache-2.0 OR MIT
- **Repository**: https://github.com/madsmtm/objc2

### displaydoc v0.2.7

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/yaahc/displaydoc

### downcast-rs v1.2.1

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/marcianx/downcast-rs

### dyn-clone v1.0.20

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/dyn-clone

### dyn-hash v0.2.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/dyn-hash

### either v1.19.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rayon-rs/either

### embedded-io v0.4.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/embassy-rs/embedded-io

### embedded-io v0.6.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-embedded/embedded-hal

### equivalent v1.0.3

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/indexmap-rs/equivalent

### errno v0.3.15

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/lambda-fairy/rust-errno

### fallible-iterator v0.3.0

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/sfackler/rust-fallible-iterator

### fallible-streaming-iterator v0.1.9

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/sfackler/fallible-streaming-iterator

### fancy-regex v0.18.0

- **License**: MIT
- **Repository**: https://github.com/fancy-regex/fancy-regex

### fastrand v2.5.0

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/smol-rs/fastrand

### fd-lock v4.0.4

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/yoshuawuyts/fd-lock

### filetime v0.2.30

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/alexcrichton/filetime

### find-msvc-tools v0.1.15

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/cc-rs

### fixedbitset v0.5.9

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/petgraph/fixedbitset

### flate2 v1.1.11

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/flate2-rs

### fnv v1.0.7

- **License**: Apache-2.0 / MIT
- **Repository**: https://github.com/servo/rust-fnv

### foldhash v0.1.5

- **License**: Zlib
- **Repository**: https://github.com/orlp/foldhash

### foldhash v0.2.0

- **License**: Zlib
- **Repository**: https://github.com/orlp/foldhash

### form_urlencoded v1.2.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/servo/rust-url

### futures v0.3.34

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-channel v0.3.34

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-core v0.3.34

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-executor v0.3.34

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-io v0.3.34

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-macro v0.3.34

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-sink v0.3.34

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-task v0.3.34

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-util v0.3.34

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### getrandom v0.2.17

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-random/getrandom

### getrandom v0.4.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-random/getrandom

### half v2.7.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/VoidStarKat/half-rs

### hashbrown v0.14.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/hashbrown

### hashbrown v0.15.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/hashbrown

### hashbrown v0.17.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/hashbrown

### hashlink v0.12.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/djc/hashlink

### heck v0.5.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/withoutboats/heck

### http v1.5.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/hyperium/http

### httparse v1.10.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/seanmonstar/httparse

### hybrid-array v0.4.15

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RustCrypto/hybrid-array

### icu_collections v2.4.0

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### icu_locale_core v2.4.0

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### icu_normalizer v2.4.0

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### icu_normalizer_data v2.4.0

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### icu_properties v2.4.0

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### icu_properties_data v2.4.0

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### icu_provider v2.4.0

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### ident_case v1.0.1

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/TedDriggs/ident_case

### idna v1.1.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/servo/rust-url/

### idna_adapter v1.2.2

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/hsivonen/idna_adapter

### indexmap v2.14.2

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/indexmap-rs/indexmap

### inotify v0.11.5

- **License**: ISC
- **Repository**: https://github.com/hannobraun/inotify-rs

### inotify-sys v0.1.8

- **License**: ISC
- **Repository**: https://github.com/hannobraun/inotify-sys

### itertools v0.10.5

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/rust-itertools/itertools

### itertools v0.12.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-itertools/itertools

### itertools v0.13.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-itertools/itertools

### itertools v0.14.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-itertools/itertools

### itoa v1.0.19

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/itoa

### jiff v1.0.2

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/jiff

### jiff-core v1.0.1

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/jiff

### jiff-static v1.0.2

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/jiff

### jni v0.22.4

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/jni-rs/jni-rs

### jni-macros v0.22.4

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/jni-rs/jni-rs

### jni-sys v0.4.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/jni-rs/jni-sys

### jni-sys-macros v0.4.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/jni-rs/jni-sys

### jobserver v0.1.35

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/jobserver-rs

### js-sys v0.3.107

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys

### kitoken v0.11.0

- **License**: BSD-2-Clause
- **Repository**: https://github.com/Systemcluster/kitoken

### kqueue v1.2.1

- **License**: MIT
- **Repository**: https://gitlab.com/rust-kqueue/rust-kqueue

### kqueue-sys v1.1.2

- **License**: MIT
- **Repository**: https://gitlab.com/rust-kqueue/rust-kqueue-sys

### kstring v2.0.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/cobalt-org/kstring

### lazy_static v1.5.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang-nursery/lazy-static.rs

### libc v0.2.191

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/libc

### libloading v0.9.1

- **License**: ISC
- **Repository**: https://github.com/nagisa/rust_libloading/

### libm v0.2.16

- **License**: MIT
- **Repository**: https://github.com/rust-lang/compiler-builtins

### libredox v0.1.25

- **License**: MIT
- **Repository**: https://gitlab.redox-os.org/redox-os/libredox.git

### libsqlite3-sys v0.40.0

- **License**: MIT
- **Repository**: https://github.com/rusqlite/rusqlite

### linux-raw-sys v0.12.1

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/sunfishcode/linux-raw-sys

### liquid v0.26.11

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/cobalt-org/liquid-rust

### liquid-core v0.26.11

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/cobalt-org/liquid-rust

### liquid-derive v0.26.10

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/cobalt-org/liquid-rust

### liquid-lib v0.26.11

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/cobalt-org/liquid-rust

### litemap v0.8.3

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### lock_api v0.4.14

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/Amanieu/parking_lot

### log v0.4.35

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/log

### lsp-types v0.94.1

- **License**: MIT
- **Repository**: https://github.com/gluon-lang/lsp-types

### lz4_flex v0.14.2

- **License**: MIT
- **Repository**: https://github.com/pseitz/lz4_flex

### maplit v1.0.2

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/bluss/maplit

### matrixmultiply v0.3.11

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/bluss/matrixmultiply/

### memchr v2.8.3

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/memchr

### memmap2 v0.9.12

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RazrFalcon/memmap2-rs

### miniz_oxide v0.9.1

- **License**: MIT OR Zlib OR Apache-2.0
- **Repository**: https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide

### mio v1.2.5

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/mio

### multiversion v0.8.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/calebzulawski/multiversion

### multiversion-macros v0.8.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/calebzulawski/multiversion

### ndarray v0.16.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-ndarray/ndarray

### nix v0.32.0

- **License**: MIT
- **Repository**: https://github.com/nix-rust/nix

### nom v8.0.0

- **License**: MIT
- **Repository**: https://github.com/rust-bakery/nom

### nom-language v0.1.0

- **License**: MIT
- **Repository**: https://github.com/rust-bakery/nom

### notify v8.2.2

- **License**: CC0-1.0
- **Repository**: https://github.com/notify-rs/notify.git

### notify-types v2.2.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/notify-rs/notify.git

### num-complex v0.4.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-num/num-complex

### num-conv v0.2.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/jhpratt/num-conv

### num-integer v0.1.47

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-num/num-integer

### num-traits v0.2.19

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-num/num-traits

### objc2 v0.6.5

- **License**: MIT
- **Repository**: https://github.com/madsmtm/objc2

### objc2-core-foundation v0.3.2

- **License**: Zlib OR Apache-2.0 OR MIT
- **Repository**: https://github.com/madsmtm/objc2

### objc2-core-services v0.3.2

- **License**: Zlib OR Apache-2.0 OR MIT
- **Repository**: https://github.com/madsmtm/objc2

### objc2-encode v4.1.0

- **License**: MIT
- **Repository**: https://github.com/madsmtm/objc2

### object v0.39.1

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/gimli-rs/object

### once_cell v1.21.4

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/matklad/once_cell

### oorandom v11.1.5

- **License**: MIT
- **Repository**: https://hg.sr.ht/~icefox/oorandom

### openssl-probe v0.2.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rustls/openssl-probe

### option-ext v0.2.0

- **License**: MPL-2.0
- **Repository**: https://github.com/soc/option-ext.git

### orx-priority-queue v1.9.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/orxfun/orx-priority-queue/

### page_size v0.6.0

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/Elzair/page_size_rs

### parking_lot v0.12.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/Amanieu/parking_lot

### parking_lot_core v0.9.12

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/Amanieu/parking_lot

### pastey v0.1.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/as1100k/pastey

### pastey v0.2.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/as1100k/pastey

### percent-encoding v2.3.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/servo/rust-url/

### pest v2.9.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/pest-parser/pest

### pest_derive v2.9.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/pest-parser/pest

### pest_generator v2.9.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/pest-parser/pest

### pest_meta v2.9.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/pest-parser/pest

### pest_stack_guard v1.0.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/pest-parser/pest_stack_guard

### petgraph v0.8.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/petgraph/petgraph

### pin-project v1.2.0

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/taiki-e/pin-project

### pin-project-internal v1.2.0

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/taiki-e/pin-project

### pin-project-lite v0.2.18

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/taiki-e/pin-project-lite

### pkg-config v0.3.34

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/pkg-config-rs

### plotters v0.3.7

- **License**: MIT
- **Repository**: https://github.com/plotters-rs/plotters

### plotters-backend v0.3.7

- **License**: MIT
- **Repository**: https://github.com/plotters-rs/plotters

### plotters-svg v0.3.7

- **License**: MIT
- **Repository**: https://github.com/plotters-rs/plotters.git

### portable-atomic v1.15.0

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/taiki-e/portable-atomic

### portable-atomic-util v0.2.8

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/taiki-e/portable-atomic-util

### postcard v1.2.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/jamesmunns/postcard

### potential_utf v0.1.6

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### powerfmt v0.2.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/jhpratt/powerfmt

### ppv-lite86 v0.2.21

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/cryptocorrosion/cryptocorrosion

### primal-check v0.3.4

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/huonw/primal

### proc-macro2 v1.0.107

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/proc-macro2

### prost v0.11.9

- **License**: Apache-2.0
- **Repository**: https://github.com/tokio-rs/prost

### prost-derive v0.11.9

- **License**: Apache-2.0
- **Repository**: https://github.com/tokio-rs/prost

### psm v0.1.32

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/stacker/

### quote v1.0.47

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/quote

### r-efi v6.0.0

- **License**: MIT OR Apache-2.0 OR LGPL-2.1-or-later
- **Repository**: https://github.com/r-efi/r-efi

### rand v0.8.8

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-random/rand

### rand_chacha v0.3.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-random/rand

### rand_core v0.6.4

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-random/rand

### rand_distr v0.4.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-random/rand

### rawpointer v0.2.1

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/bluss/rawpointer/

### rayon v1.12.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rayon-rs/rayon

### rayon-core v1.13.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rayon-rs/rayon

### redox_syscall v0.5.18

- **License**: MIT
- **Repository**: https://gitlab.redox-os.org/redox-os/syscall

### redox_users v0.5.3

- **License**: MIT
- **Repository**: https://gitlab.redox-os.org/redox-os/users

### ref-cast v1.0.27

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/ref-cast

### ref-cast-impl v1.0.27

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/ref-cast

### regex v1.13.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/regex

### regex-automata v0.4.19

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/regex

### regex-syntax v0.8.12

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/regex

### ring v0.17.14

- **License**: Apache-2.0 AND ISC
- **Repository**: https://github.com/briansmith/ring

### rmcp v3.11.0

- **License**: Apache-2.0
- **Repository**: https://github.com/modelcontextprotocol/rust-sdk/

### rmcp-macros v3.11.0

- **License**: Apache-2.0
- **Repository**: https://github.com/modelcontextprotocol/rust-sdk/

### rusqlite v0.43.0

- **License**: MIT
- **Repository**: https://github.com/rusqlite/rusqlite

### rust-stemmers v1.2.0

- **License**: MIT/BSD-3-Clause
- **Repository**: https://github.com/CurrySoftware/rust-stemmers

### rustc_version v0.4.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/djc/rustc-version-rs

### rustfft v6.4.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/ejmahler/RustFFT

### rustix v1.1.6

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/rustix

### rustls v0.23.46

- **License**: Apache-2.0 OR ISC OR MIT
- **Repository**: https://github.com/rustls/rustls

### rustls-native-certs v0.8.5

- **License**: Apache-2.0 OR ISC OR MIT
- **Repository**: https://github.com/rustls/rustls-native-certs

### rustls-pki-types v1.16.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rustls/pki-types

### rustls-platform-verifier v0.7.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rustls/rustls-platform-verifier

### rustls-platform-verifier-android v0.2.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rustls/rustls-platform-verifier

### rustls-webpki v0.103.17

- **License**: ISC
- **Repository**: https://github.com/rustls/webpki

### rustversion v1.0.23

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/rustversion

### safetensors v0.6.2

- **License**: Apache-2.0
- **Repository**: https://github.com/huggingface/safetensors

### same-file v1.0.6

- **License**: Unlicense/MIT
- **Repository**: https://github.com/BurntSushi/same-file

### scan_fmt v0.2.6

- **License**: MIT
- **Repository**: https://github.com/wlentz/scan_fmt

### schannel v0.1.30

- **License**: MIT
- **Repository**: https://github.com/steffengy/schannel-rs

### schemars v1.2.3

- **License**: MIT
- **Repository**: https://github.com/GREsau/schemars

### schemars_derive v1.2.3

- **License**: MIT
- **Repository**: https://github.com/GREsau/schemars

### scopeguard v1.2.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/bluss/scopeguard

### security-framework v3.7.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/kornelski/rust-security-framework

### security-framework-sys v2.17.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/kornelski/rust-security-framework

### semver v1.0.28

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/semver

### serde v1.0.229

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/serde-rs/serde

### serde_core v1.0.229

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/serde-rs/serde

### serde_derive v1.0.229

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/serde-rs/serde

### serde_derive_internals v0.30.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/serde-rs/serde

### serde_json v1.0.154

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/serde-rs/json

### serde_repr v0.1.21

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/serde-repr

### serde_spanned v1.1.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/toml-rs/toml

### sha2 v0.11.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RustCrypto/hashes

### shlex v2.0.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/comex/rust-shlex

### signal-hook-registry v1.4.8

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/vorner/signal-hook

### simd-adler32 v0.3.10

- **License**: MIT
- **Repository**: https://github.com/mcountryman/simd-adler32

### simd_cesu8 v1.2.0

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/seancroach/simd_cesu8

### simdutf8 v0.1.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rusticstuff/simdutf8

### slab v0.4.12

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/slab

### smallvec v1.16.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/smallvec/smallvec

### socket2 v0.6.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/socket2

### stable_deref_trait v1.2.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/storyyeller/stable_deref_trait

### stacker v0.1.25

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/stacker

### static_assertions v1.1.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/nvzqz/static-assertions-rs

### streaming-iterator v0.1.9

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/sfackler/streaming-iterator

### strength_reduce v0.2.4

- **License**: MIT OR Apache-2.0
- **Repository**: http://github.com/ejmahler/strength_reduce

### string-interner v0.15.0

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/robbepop/string-interner

### strsim v0.11.1

- **License**: MIT
- **Repository**: https://github.com/rapidfuzz/strsim-rs

### subtle v2.6.1

- **License**: BSD-3-Clause
- **Repository**: https://github.com/dalek-cryptography/subtle

### syn v1.0.109

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/syn

### syn v2.0.119

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/syn

### syn v3.0.10

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/syn

### synstructure v0.14.0

- **License**: MIT
- **Repository**: https://github.com/mystor/synstructure

### tar v0.4.46

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/composefs/tar-rs

### target-features v0.1.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/calebzulawski/target-features

### tempfile v3.27.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/Stebalien/tempfile

### thiserror v2.0.21

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/thiserror

### thiserror-impl v2.0.21

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/thiserror

### time v0.3.55

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/time-rs/time

### time-core v0.1.9

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/time-rs/time

### time-macros v0.2.32

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/time-rs/time

### tiny-keccak v2.0.2

- **License**: CC0-1.0
- **Repository**: Not specified upstream

### tinystr v0.8.5

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### tinytemplate v1.2.1

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/bheisler/TinyTemplate

### tinyvec v1.13.3

- **License**: Zlib OR Apache-2.0 OR MIT
- **Repository**: https://github.com/Lokathor/tinyvec

### tokio v1.54.1

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/tokio

### tokio-macros v2.7.2

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/tokio

### tokio-stream v0.1.19

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/tokio

### tokio-util v0.7.20

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/tokio

### toml v1.2.2+spec-1.1.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/toml-rs/toml

### toml_datetime v1.1.3+spec-1.1.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/toml-rs/toml

### toml_edit v0.25.20+spec-1.1.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/toml-rs/toml

### toml_parser v1.1.6+spec-1.1.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/toml-rs/toml

### toml_writer v1.2.0+spec-1.1.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/toml-rs/toml

### tower v0.4.13

- **License**: MIT
- **Repository**: https://github.com/tower-rs/tower

### tower-layer v0.3.3

- **License**: MIT
- **Repository**: https://github.com/tower-rs/tower

### tower-lsp v0.20.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/ebkalderon/tower-lsp

### tower-lsp-macros v0.9.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/ebkalderon/tower-lsp

### tower-service v0.3.3

- **License**: MIT
- **Repository**: https://github.com/tower-rs/tower

### tracing v0.1.44

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/tracing

### tracing-attributes v0.1.31

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/tracing

### tracing-core v0.1.36

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/tracing

### tract-core v0.22.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/snipsco/tract

### tract-data v0.22.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/snipsco/tract

### tract-hir v0.22.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/snipsco/tract

### tract-linalg v0.22.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/snipsco/tract

### tract-nnef v0.22.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/snipsco/tract

### tract-onnx v0.22.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/snipsco/tract

### tract-onnx-opl v0.22.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/snipsco/tract

### transpose v0.2.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/ejmahler/transpose

### tree-sitter v0.27.2

- **License**: MIT
- **Repository**: https://github.com/tree-sitter/tree-sitter

### tree-sitter-language v0.1.9

- **License**: MIT
- **Repository**: https://github.com/tree-sitter/tree-sitter

### tree-sitter-language-pack v1.25.0

- **License**: MIT
- **Repository**: https://github.com/xberg-io/tree-sitter-language-pack

### typenum v1.20.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/paholg/typenum

### ucd-trie v0.1.8

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/BurntSushi/ucd-generate

### unicode-general-category v1.1.0

- **License**: Apache-2.0
- **Repository**: https://github.com/yeslogic/unicode-general-category

### unicode-ident v1.0.27

- **License**: (MIT OR Apache-2.0) AND Unicode-3.0
- **Repository**: https://github.com/dtolnay/unicode-ident

### unicode-normalization v0.1.26

- **License**: (MIT OR Apache-2.0) AND Unicode-3.0
- **Repository**: https://github.com/unicode-rs/unicode-normalization

### unicode-segmentation v1.13.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/unicode-rs/unicode-segmentation

### untrusted v0.9.0

- **License**: ISC
- **Repository**: https://github.com/briansmith/untrusted

### ureq v3.5.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/algesten/ureq

### ureq-proto v0.7.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/algesten/ureq-proto

### url v2.5.9

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/servo/rust-url

### utf8-zero v0.8.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/algesten/utf8-zero

### utf8_iter v1.1.0

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/hsivonen/utf8_iter

### utf_types v1.0.0

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/hsivonen/utf_types

### uuid v1.29.3

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/uuid-rs/uuid

### vcpkg v0.2.15

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/mcgoo/vcpkg-rs

### version_check v0.9.5

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/SergioBenitez/version_check

### walkdir v2.5.1

- **License**: Unlicense/MIT
- **Repository**: https://github.com/BurntSushi/walkdir

### wasi v0.11.1+wasi-snapshot-preview1

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wasi

### wasm-bindgen v0.2.130

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/wasm-bindgen/wasm-bindgen

### wasm-bindgen-macro v0.2.130

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro

### wasm-bindgen-macro-support v0.2.130

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/wasm-bindgen/wasm-bindgen/tree/main/crates/macro-support

### wasm-bindgen-shared v0.2.130

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared

### web-sys v0.3.107

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/web-sys

### webpki-root-certs v1.0.12

- **License**: CDLA-Permissive-2.0
- **Repository**: https://github.com/rustls/webpki-roots

### webpki-roots v1.0.12

- **License**: CDLA-Permissive-2.0
- **Repository**: https://github.com/rustls/webpki-roots

### winapi v0.3.9

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/retep998/winapi-rs

### winapi-i686-pc-windows-gnu v0.4.0

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/retep998/winapi-rs

### winapi-util v0.1.11

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/winapi-util

### winapi-x86_64-pc-windows-gnu v0.4.0

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/retep998/winapi-rs

### windows-link v0.2.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-sys v0.52.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-sys v0.59.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-sys v0.61.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-targets v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_aarch64_gnullvm v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_aarch64_msvc v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_i686_gnu v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_i686_gnullvm v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_i686_msvc v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_x86_64_gnu v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_x86_64_gnullvm v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_x86_64_msvc v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### winnow v1.0.5

- **License**: MIT
- **Repository**: https://github.com/winnow-rs/winnow

### writeable v0.6.4

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### xattr v1.7.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/Stebalien/xattr

### yoke v0.8.4

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### yoke-derive v0.8.4

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### zerocopy v0.8.64

- **License**: BSD-2-Clause OR Apache-2.0 OR MIT
- **Repository**: https://github.com/google/zerocopy

### zerocopy-derive v0.8.64

- **License**: BSD-2-Clause OR Apache-2.0 OR MIT
- **Repository**: https://github.com/google/zerocopy

### zerofrom v0.1.8

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### zerofrom-derive v0.1.8

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### zeroize v1.9.2

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/RustCrypto/utils

### zerotrie v0.2.5

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### zerovec v0.11.9

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### zerovec-derive v0.11.7

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### zlib-rs v0.6.8

- **License**: Zlib
- **Repository**: https://github.com/trifectatechfoundation/zlib-rs

### zmij v1.0.23

- **License**: MIT
- **Repository**: https://github.com/dtolnay/zmij

### zstd v0.14.4

- **License**: BSD-3-Clause
- **Repository**: https://github.com/gyscos/zstd-rs

### zstd-safe v8.3.0

- **License**: BSD-3-Clause
- **Repository**: https://github.com/gyscos/zstd-rs

### zstd-sys v2.1.3+zstd.1.5.7

- **License**: BSD-3-Clause AND BSD-2-Clause
- **Repository**: https://github.com/gyscos/zstd-rs
