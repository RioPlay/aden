# NOTICE

**aden** — A Dense Referential Context Compiler.

© 2026 RioPlay <rioplay@rioplay.dev>.

Licensed under AGPL-3.0-or-later (see [LICENSE](LICENSE)).

This NOTICE file additionally lists the licenses of the third-party
dependencies aden builds against, for attribution and license-compatibility
purposes.

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

The **`view`** feature (browser graph viewer, **on by default**) embeds one
vendored frontend asset: **`force-graph`** (vasturiano), **MIT**, pinned at
v1.51.4 and recorded with a sha256 in
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
at v1.80.0 (sha256 in the same `CHECKSUMS` file) for the `aden view --3d`
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

# Third-Party Dependencies

This project uses the following open-source packages.
Generated by `aden licenses`.

## Dependencies with Licenses

### adler2 v2.0.1

- **License**: 0BSD OR MIT OR Apache-2.0
- **Repository**: https://github.com/oyvindln/adler2

### ahash v0.8.12

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/tkaitchuck/ahash

### aho-corasick v1.1.4

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/aho-corasick

### android_system_properties v0.1.5

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/nical/android_system_properties

### anes v0.1.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/zrzka/anes-rs

### anstream v1.0.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-cli/anstyle.git

### anstyle v1.0.14

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-cli/anstyle.git

### anstyle-parse v1.0.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-cli/anstyle.git

### anstyle-query v1.1.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-cli/anstyle.git

### anstyle-wincon v3.0.11

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-cli/anstyle.git

### anyhow v1.0.102

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/anyhow

### arrayref v0.3.9

- **License**: BSD-2-Clause
- **Repository**: https://github.com/droundy/arrayref

### arrayvec v0.7.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/bluss/arrayvec

### async-trait v0.1.89

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/async-trait

### atomic-polyfill v1.0.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/embassy-rs/atomic-polyfill

### auto_impl v1.3.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/auto-impl-rs/auto_impl/

### autocfg v1.5.1

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/cuviper/autocfg

### base64 v0.22.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/marshallpierce/rust-base64

### bitflags v1.3.2

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/bitflags/bitflags

### bitflags v2.11.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/bitflags/bitflags

### blake3 v1.8.5

- **License**: CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception
- **Repository**: https://github.com/BLAKE3-team/BLAKE3

### block-buffer v0.12.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RustCrypto/utils

### block2 v0.6.2

- **License**: MIT
- **Repository**: https://github.com/madsmtm/objc2

### bstr v1.12.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/BurntSushi/bstr

### bumpalo v3.20.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/fitzgen/bumpalo

### byteorder v1.5.0

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/byteorder

### byteorder-lite v0.1.0

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/image-rs/byteorder-lite

### bytes v1.11.1

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/bytes

### byteview v0.10.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/fjall-rs/byteview

### cast v0.3.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/japaric/cast.rs

### cc v1.2.62

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/cc-rs

### cfg-if v1.0.4

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/cfg-if

### cfg_aliases v0.2.1

- **License**: MIT
- **Repository**: https://github.com/katharostech/cfg_aliases

### chrono v0.4.44

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/chronotope/chrono

### ciborium v0.2.2

- **License**: Apache-2.0
- **Repository**: https://github.com/enarx/ciborium

### ciborium-io v0.2.2

- **License**: Apache-2.0
- **Repository**: https://github.com/enarx/ciborium

### ciborium-ll v0.2.2

- **License**: Apache-2.0
- **Repository**: https://github.com/enarx/ciborium

### clap v4.6.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/clap-rs/clap

### clap_builder v4.6.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/clap-rs/clap

### clap_derive v4.6.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/clap-rs/clap

### clap_lex v1.1.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/clap-rs/clap

### cobs v0.3.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/jamesmunns/cobs.rs

### colorchoice v1.0.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-cli/anstyle.git

### compare v0.0.6

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/contain-rs/compare

### const-oid v0.10.2

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/RustCrypto/formats

### const-random v0.1.18

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/tkaitchuck/constrandom

### const-random-macro v0.1.16

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/tkaitchuck/constrandom

### constant_time_eq v0.4.2

- **License**: CC0-1.0 OR MIT-0 OR Apache-2.0
- **Repository**: https://github.com/cesarb/constant_time_eq

### core-foundation-sys v0.8.7

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/servo/core-foundation-rs

### cpufeatures v0.3.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RustCrypto/utils

### crc32fast v1.5.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/srijs/rust-crc32fast

### criterion v0.5.1

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/bheisler/criterion.rs

### criterion-plot v0.5.0

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/bheisler/criterion.rs

### critical-section v1.2.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-embedded/critical-section

### crossbeam-deque v0.8.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/crossbeam-rs/crossbeam

### crossbeam-epoch v0.9.18

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/crossbeam-rs/crossbeam

### crossbeam-skiplist v0.1.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/crossbeam-rs/crossbeam

### crossbeam-utils v0.8.21

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/crossbeam-rs/crossbeam

### crunchy v0.2.4

- **License**: MIT
- **Repository**: https://github.com/eira-fransham/crunchy

### crypto-common v0.2.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RustCrypto/traits

### ctrlc v3.5.2

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/Detegr/rust-ctrlc.git

### darling v0.23.0

- **License**: MIT
- **Repository**: https://github.com/TedDriggs/darling

### darling_core v0.23.0

- **License**: MIT
- **Repository**: https://github.com/TedDriggs/darling

### darling_macro v0.23.0

- **License**: MIT
- **Repository**: https://github.com/TedDriggs/darling

### dashmap v5.5.3

- **License**: MIT
- **Repository**: https://github.com/xacrimon/dashmap

### dashmap v6.2.1

- **License**: MIT
- **Repository**: https://github.com/xacrimon/dashmap

### digest v0.11.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RustCrypto/traits

### dirs v6.0.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/soc/dirs-rs

### dirs-sys v0.5.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dirs-dev/dirs-sys-rs

### dispatch2 v0.3.1

- **License**: Zlib OR Apache-2.0 OR MIT
- **Repository**: https://github.com/madsmtm/objc2

### displaydoc v0.2.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/yaahc/displaydoc

### double-ended-peekable v0.1.0

- **License**: MIT
- **Repository**: https://github.com/dodomorandi/double-ended-peekable

### dyn-clone v1.0.20

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/dyn-clone

### dyn-eq v0.1.3

- **License**: MPL-2.0
- **Repository**: https://github.com/Voultapher/dyn-eq
- **Note**: Dense-feature-only transitive dependency (pulled via the `tract` ONNX stack).

### either v1.16.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rayon-rs/either

### embedded-io v0.4.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/embassy-rs/embedded-io

### embedded-io v0.6.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-embedded/embedded-hal

### encoding_rs v0.8.35

- **License**: (Apache-2.0 OR MIT) AND BSD-3-Clause
- **Repository**: https://github.com/hsivonen/encoding_rs

### encoding_rs_io v0.1.7

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/BurntSushi/encoding_rs_io

### enum_dispatch v0.3.13

- **License**: MIT OR Apache-2.0
- **Repository**: https://gitlab.com/antonok/enum_dispatch

### equivalent v1.0.2

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/indexmap-rs/equivalent

### errno v0.3.14

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/lambda-fairy/rust-errno

### fastrand v2.4.1

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/smol-rs/fastrand

### filetime v0.2.29

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/alexcrichton/filetime

### find-msvc-tools v0.1.9

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/cc-rs

### fixedbitset v0.5.7

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/petgraph/fixedbitset

### fjall v3.1.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/fjall-rs/fjall

### flate2 v1.1.9

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/flate2-rs

### flume v0.12.0

- **License**: Apache-2.0/MIT
- **Repository**: https://github.com/zesterer/flume

### fnv v1.0.7

- **License**: Apache-2.0 / MIT
- **Repository**: https://github.com/servo/rust-fnv

### foldhash v0.1.5

- **License**: Zlib
- **Repository**: https://github.com/orlp/foldhash

### form_urlencoded v1.2.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/servo/rust-url

### fsevent-sys v4.1.0

- **License**: MIT
- **Repository**: https://github.com/octplane/fsevent-rust/tree/master/fsevent-sys

### futures v0.3.32

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-channel v0.3.32

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-core v0.3.32

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-executor v0.3.32

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-io v0.3.32

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-macro v0.3.32

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-sink v0.3.32

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-task v0.3.32

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### futures-util v0.3.32

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/futures-rs

### getrandom v0.2.17

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-random/getrandom

### getrandom v0.3.4

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-random/getrandom

### getrandom v0.4.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-random/getrandom

### globset v0.4.18

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/ripgrep/tree/master/crates/globset

### grep-matcher v0.1.8

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/ripgrep/tree/master/crates/matcher

### grep-regex v0.1.14

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/ripgrep/tree/master/crates/regex

### grep-searcher v0.1.16

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/ripgrep/tree/master/crates/searcher

### guardian v1.3.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/jonhoo/guardian.git

### half v2.7.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/VoidStarKat/half-rs

### hash32 v0.2.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/japaric/hash32

### hashbrown v0.14.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/hashbrown

### hashbrown v0.15.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/hashbrown

### hashbrown v0.16.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/hashbrown

### hashbrown v0.17.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/hashbrown

### heapless v0.7.17

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/japaric/heapless

### heck v0.5.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/withoutboats/heck

### hermit-abi v0.5.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/hermit-os/hermit-rs

### http v1.4.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/hyperium/http

### httparse v1.10.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/seanmonstar/httparse

### hybrid-array v0.4.12

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RustCrypto/hybrid-array

### iana-time-zone v0.1.65

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/strawlab/iana-time-zone

### iana-time-zone-haiku v0.1.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/strawlab/iana-time-zone

### icu_collections v2.1.1

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### icu_locale_core v2.1.1

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### icu_normalizer v2.1.1

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### icu_normalizer_data v2.1.1

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### icu_properties v2.1.2

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### icu_properties_data v2.1.2

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### icu_provider v2.1.1

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### id-arena v2.3.0

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/fitzgen/id-arena

### ident_case v1.0.1

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/TedDriggs/ident_case

### idna v1.1.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/servo/rust-url/

### idna_adapter v1.2.1

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/hsivonen/idna_adapter

### ignore v0.4.25

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/ripgrep/tree/master/crates/ignore

### indexmap v2.14.0

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/indexmap-rs/indexmap

### inotify v0.11.1

- **License**: ISC
- **Repository**: https://github.com/hannobraun/inotify

### inotify-sys v0.1.5

- **License**: ISC
- **Repository**: https://github.com/hannobraun/inotify-sys

### interval-heap v0.0.5

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/contain-rs/interval-heap

### is-terminal v0.4.17

- **License**: MIT
- **Repository**: https://github.com/sunfishcode/is-terminal

### is_terminal_polyfill v1.70.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/polyfill-rs/is_terminal_polyfill

### itertools v0.10.5

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/rust-itertools/itertools

### itoa v1.0.18

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/itoa

### jobserver v0.1.34

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/jobserver-rs

### js-sys v0.3.99

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys

### kqueue v1.1.1

- **License**: MIT
- **Repository**: https://gitlab.com/rust-kqueue/rust-kqueue

### kqueue-sys v1.1.2

- **License**: MIT
- **Repository**: https://gitlab.com/rust-kqueue/rust-kqueue-sys

### leb128fmt v0.1.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/bluk/leb128fmt

### libc v0.2.186

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/libc

### libloading v0.9.0

- **License**: ISC
- **Repository**: https://github.com/nagisa/rust_libloading/

### libredox v0.1.16

- **License**: MIT
- **Repository**: https://gitlab.redox-os.org/redox-os/libredox.git

### linux-raw-sys v0.12.1

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/sunfishcode/linux-raw-sys

### litemap v0.8.2

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### lock_api v0.4.14

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/Amanieu/parking_lot

### log v0.4.29

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/log

### lsm-tree v3.1.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/fjall-rs/lsm-tree

### lsp-types v0.94.1

- **License**: MIT
- **Repository**: https://github.com/gluon-lang/lsp-types

### lz4_flex v0.13.1

- **License**: MIT
- **Repository**: https://github.com/pseitz/lz4_flex

### memchr v2.8.0

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/memchr

### memmap2 v0.9.10

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RazrFalcon/memmap2-rs

### miniz_oxide v0.8.9

- **License**: MIT OR Zlib OR Apache-2.0
- **Repository**: https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide

### mio v1.2.0

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/mio

### nix v0.31.3

- **License**: MIT
- **Repository**: https://github.com/nix-rust/nix

### notify v8.2.0

- **License**: CC0-1.0
- **Repository**: https://github.com/notify-rs/notify.git

### notify-types v2.1.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/notify-rs/notify.git

### num-traits v0.2.19

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-num/num-traits

### objc2 v0.6.4

- **License**: MIT
- **Repository**: https://github.com/madsmtm/objc2

### objc2-encode v4.1.0

- **License**: MIT
- **Repository**: https://github.com/madsmtm/objc2

### once_cell v1.21.4

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/matklad/once_cell

### once_cell_polyfill v1.70.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/polyfill-rs/once_cell_polyfill

### oorandom v11.1.5

- **License**: MIT
- **Repository**: https://hg.sr.ht/~icefox/oorandom

### option-ext v0.2.0

- **License**: MPL-2.0
- **Repository**: https://github.com/soc/option-ext.git

### parking_lot_core v0.9.12

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/Amanieu/parking_lot

### pastey v0.2.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/as1100k/pastey

### path-absolutize v3.1.1

- **License**: MIT
- **Repository**: https://github.com/magiclen/path-absolutize

### path-dedot v3.1.1

- **License**: MIT
- **Repository**: https://github.com/magiclen/path-dedot

### percent-encoding v2.3.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/servo/rust-url/

### petgraph v0.8.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/petgraph/petgraph

### pin-project v1.1.13

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/taiki-e/pin-project

### pin-project-internal v1.1.13

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/taiki-e/pin-project

### pin-project-lite v0.2.17

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/taiki-e/pin-project-lite

### pkg-config v0.3.33

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

### postcard v1.1.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/jamesmunns/postcard

### potential_utf v0.1.5

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### prettyplease v0.2.37

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/prettyplease

### proc-macro2 v1.0.106

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/proc-macro2

### quick_cache v0.6.22

- **License**: MIT
- **Repository**: https://github.com/arthurprs/quick-cache

### quote v1.0.45

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/quote

### r-efi v5.3.0

- **License**: MIT OR Apache-2.0 OR LGPL-2.1-or-later
- **Repository**: https://github.com/r-efi/r-efi

### r-efi v6.0.0

- **License**: MIT OR Apache-2.0 OR LGPL-2.1-or-later
- **Repository**: https://github.com/r-efi/r-efi

### rayon v1.12.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rayon-rs/rayon

### rayon-core v1.13.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rayon-rs/rayon

### redox_syscall v0.5.18

- **License**: MIT
- **Repository**: https://gitlab.redox-os.org/redox-os/syscall

### redox_users v0.5.2

- **License**: MIT
- **Repository**: https://gitlab.redox-os.org/redox-os/users

### ref-cast v1.0.25

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/ref-cast

### ref-cast-impl v1.0.25

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/ref-cast

### regex v1.12.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/regex

### regex-automata v0.4.14

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/regex

### regex-syntax v0.8.10

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rust-lang/regex

### ring v0.17.14

- **License**: Apache-2.0 AND ISC
- **Repository**: https://github.com/briansmith/ring

### rmcp v1.7.0

- **License**: Apache-2.0
- **Repository**: https://github.com/modelcontextprotocol/rust-sdk/

### rmcp-macros v1.7.0

- **License**: Apache-2.0
- **Repository**: https://github.com/modelcontextprotocol/rust-sdk/

### rust-stemmers v1.2.0

- **License**: MIT
- **Repository**: https://github.com/SeekStorm/rust-stemmers

### rustc-hash v2.1.2

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/rust-lang/rustc-hash

### rustc_version v0.4.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/djc/rustc-version-rs

### rustix v1.1.4

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/rustix

### rustls v0.23.40

- **License**: Apache-2.0 OR ISC OR MIT
- **Repository**: https://github.com/rustls/rustls

### rustls-pki-types v1.14.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/rustls/pki-types

### rustls-webpki v0.103.13

- **License**: ISC
- **Repository**: https://github.com/rustls/webpki

### rustversion v1.0.22

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/rustversion

### ryu v1.0.23

- **License**: Apache-2.0 OR BSL-1.0
- **Repository**: https://github.com/dtolnay/ryu

### same-file v1.0.6

- **License**: Unlicense/MIT
- **Repository**: https://github.com/BurntSushi/same-file

### schemars v1.2.1

- **License**: MIT
- **Repository**: https://github.com/GREsau/schemars

### schemars_derive v1.2.1

- **License**: MIT
- **Repository**: https://github.com/GREsau/schemars

### scopeguard v1.2.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/bluss/scopeguard

### self_cell v1.2.2

- **License**: Apache-2.0 OR GPL-2.0-only
- **Repository**: https://github.com/Voultapher/self_cell

### semver v1.0.28

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/semver

### serde v1.0.228

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/serde-rs/serde

### serde_core v1.0.228

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/serde-rs/serde

### serde_derive v1.0.228

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/serde-rs/serde

### serde_derive_internals v0.29.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/serde-rs/serde

### serde_json v1.0.150

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/serde-rs/json

### serde_repr v0.1.20

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/serde-repr

### serde_spanned v0.6.9

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/toml-rs/toml

### serde_yaml v0.9.34+deprecated

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/serde-yaml

### sfa v1.0.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/fjall-rs/sfa

### sha2 v0.11.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/RustCrypto/hashes

### shlex v1.3.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/comex/rust-shlex

### signal-hook-registry v1.4.8

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/vorner/signal-hook

### simd-adler32 v0.3.9

- **License**: MIT
- **Repository**: https://github.com/mcountryman/simd-adler32

### slab v0.4.12

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/slab

### smallvec v1.15.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/servo/rust-smallvec

### spin v0.9.8

- **License**: MIT
- **Repository**: https://github.com/mvdnes/spin-rs.git

### stable_deref_trait v1.2.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/storyyeller/stable_deref_trait

### std-semaphore v0.1.0

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/invenia/std-semaphore

### streaming-iterator v0.1.9

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/sfackler/streaming-iterator

### strsim v0.11.1

- **License**: MIT
- **Repository**: https://github.com/rapidfuzz/strsim-rs

### subtle v2.6.1

- **License**: BSD-3-Clause
- **Repository**: https://github.com/dalek-cryptography/subtle

### syn v2.0.117

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/syn

### synstructure v0.13.2

- **License**: MIT
- **Repository**: https://github.com/mystor/synstructure

### tar v0.4.46

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/composefs/tar-rs

### tempfile v3.27.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/Stebalien/tempfile

### thiserror v2.0.18

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/thiserror

### thiserror-impl v2.0.18

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/dtolnay/thiserror

### tiny-keccak v2.0.2

- **License**: CC0-1.0

### tinystr v0.8.3

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### tinytemplate v1.2.1

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/bheisler/TinyTemplate

### tokio v1.52.3

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/tokio

### tokio-macros v2.7.0

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/tokio

### tokio-util v0.7.18

- **License**: MIT
- **Repository**: https://github.com/tokio-rs/tokio

### toml v0.8.23

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/toml-rs/toml

### toml_datetime v0.6.11

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/toml-rs/toml

### toml_edit v0.22.27

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/toml-rs/toml

### toml_write v0.1.2

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

### tree-sitter v0.26.9

- **License**: MIT
- **Copyright**: Copyright (c) 2018-2024 Max Brunsfeld
- **Repository**: https://github.com/tree-sitter/tree-sitter

### tree-sitter-language v0.1.7

- **License**: MIT
- **Repository**: https://github.com/tree-sitter/tree-sitter

### tree-sitter-language-pack v1.8.1

- **License**: MIT (pack crate); individual grammars are predominantly MIT and Apache-2.0
- **Repository**: https://github.com/kreuzberg-dev/tree-sitter-language-pack
- **Note**: This crate bundles compiled grammars from numerous upstream repositories. Each grammar carries its own copyright holder and license. Full per-grammar attribution is maintained by the pack at the repository above.

### twox-hash v2.1.2

- **License**: MIT
- **Repository**: https://github.com/shepmaster/twox-hash

### typenum v1.20.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/paholg/typenum

### unicode-ident v1.0.24

- **License**: (MIT OR Apache-2.0) AND Unicode-3.0
- **Repository**: https://github.com/dtolnay/unicode-ident

### unicode-xid v0.2.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/unicode-rs/unicode-xid

### unsafe-libyaml v0.2.11

- **License**: MIT
- **Repository**: https://github.com/dtolnay/unsafe-libyaml

### untrusted v0.9.0

- **License**: ISC
- **Repository**: https://github.com/briansmith/untrusted

### ureq v2.12.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/algesten/ureq

### ureq v3.3.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/algesten/ureq

### ureq-proto v0.6.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/algesten/ureq-proto

### url v2.5.8

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/servo/rust-url

### utf8-zero v0.8.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/algesten/utf8-zero

### utf8_iter v1.0.4

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/hsivonen/utf8_iter

### utf8parse v0.2.2

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/alacritty/vte

### uuid v1.23.1

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/uuid-rs/uuid

### varint-rs v2.2.0

- **License**: Apache-2.0
- **Repository**: https://github.com/LeonskiDev/varint-rs

### version_check v0.9.5

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/SergioBenitez/version_check

### walkdir v2.5.0

- **License**: Unlicense/MIT
- **Repository**: https://github.com/BurntSushi/walkdir

### wasi v0.11.1+wasi-snapshot-preview1

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wasi

### wasip2 v1.0.3+wasi-0.2.9

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wasi-rs

### wasip3 v0.4.0+wasi-0.3.0-rc-2026-01-06

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wasi-rs

### wasm-bindgen v0.2.122

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/wasm-bindgen/wasm-bindgen

### wasm-bindgen-macro v0.2.122

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro

### wasm-bindgen-macro-support v0.2.122

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro-support

### wasm-bindgen-shared v0.2.122

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared

### wasm-encoder v0.244.0

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-encoder

### wasm-metadata v0.244.0

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-metadata

### wasmparser v0.244.0

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasmparser

### web-sys v0.3.99

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/web-sys

### webpki-roots v0.26.11

- **License**: CDLA-Permissive-2.0
- **Repository**: https://github.com/rustls/webpki-roots

### webpki-roots v1.0.7

- **License**: CDLA-Permissive-2.0
- **Repository**: https://github.com/rustls/webpki-roots

### winapi-util v0.1.11

- **License**: Unlicense OR MIT
- **Repository**: https://github.com/BurntSushi/winapi-util

### windows-core v0.62.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-implement v0.60.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-interface v0.59.3

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-link v0.2.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-result v0.4.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-strings v0.5.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-sys v0.52.0

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-sys v0.60.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-sys v0.61.2

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-targets v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows-targets v0.53.5

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_aarch64_gnullvm v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_aarch64_gnullvm v0.53.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_aarch64_msvc v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_aarch64_msvc v0.53.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_i686_gnu v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_i686_gnu v0.53.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_i686_gnullvm v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_i686_gnullvm v0.53.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_i686_msvc v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_i686_msvc v0.53.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_x86_64_gnu v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_x86_64_gnu v0.53.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_x86_64_gnullvm v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_x86_64_gnullvm v0.53.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_x86_64_msvc v0.52.6

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### windows_x86_64_msvc v0.53.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/microsoft/windows-rs

### winnow v0.7.15

- **License**: MIT
- **Repository**: https://github.com/winnow-rs/winnow

### wit-bindgen v0.51.0

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wit-bindgen

### wit-bindgen v0.57.1

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wit-bindgen

### wit-bindgen-core v0.51.0

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wit-bindgen

### wit-bindgen-rust v0.51.0

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wit-bindgen

### wit-bindgen-rust-macro v0.51.0

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wit-bindgen

### wit-component v0.244.0

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wit-component

### wit-parser v0.244.0

- **License**: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
- **Repository**: https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wit-parser

### writeable v0.6.3

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### xattr v1.6.1

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/Stebalien/xattr

### xxhash-rust v0.8.15

- **License**: BSL-1.0
- **Repository**: https://github.com/DoumanAsh/xxhash-rust

### yoke v0.8.2

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### yoke-derive v0.8.2

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### zerocopy v0.8.48

- **License**: BSD-2-Clause OR Apache-2.0 OR MIT
- **Repository**: https://github.com/google/zerocopy

### zerocopy-derive v0.8.48

- **License**: BSD-2-Clause OR Apache-2.0 OR MIT
- **Repository**: https://github.com/google/zerocopy

### zerofrom v0.1.8

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### zerofrom-derive v0.1.7

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### zeroize v1.8.2

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/RustCrypto/utils

### zerotrie v0.2.4

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### zerovec v0.11.6

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### zerovec-derive v0.11.3

- **License**: Unicode-3.0
- **Repository**: https://github.com/unicode-org/icu4x

### zmij v1.0.21

- **License**: MIT
- **Repository**: https://github.com/dtolnay/zmij

### zstd v0.13.3

- **License**: MIT
- **Repository**: https://github.com/gyscos/zstd-rs

### zstd-safe v7.2.4

- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/gyscos/zstd-rs

### zstd-sys v2.0.16+zstd.1.5.7

- **License**: MIT/Apache-2.0
- **Repository**: https://github.com/gyscos/zstd-rs

## License Summary

| License | Count |
|--------|-------|
| MIT OR Apache-2.0 | 185 |
| MIT | 53 |
| Unicode-3.0 | 18 |
| Apache-2.0 OR MIT | 16 |
| Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | 15 |
| MIT/Apache-2.0 | 13 |
| Unlicense OR MIT | 9 |
| Apache-2.0 | 6 |
| ISC | 5 |
| CC0-1.0 | 2 |
| Unlicense/MIT | 2 |
| CDLA-Permissive-2.0 | 2 |
| BSD-2-Clause OR Apache-2.0 OR MIT | 2 |
| MIT OR Apache-2.0 OR LGPL-2.1-or-later | 2 |
| (Apache-2.0 OR MIT) AND BSD-3-Clause | 1 |
| MIT OR Zlib OR Apache-2.0 | 1 |
| MPL-2.0 | 2 |
| BSD-2-Clause | 1 |
| Apache-2.0 OR ISC OR MIT | 1 |
| Zlib OR Apache-2.0 OR MIT | 1 |
| CC0-1.0 OR MIT-0 OR Apache-2.0 | 1 |
| 0BSD OR MIT OR Apache-2.0 | 1 |
| Apache-2.0 AND ISC | 1 |
| Apache-2.0 OR BSL-1.0 | 1 |
| (MIT OR Apache-2.0) AND Unicode-3.0 | 1 |
| CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception | 1 |
| BSD-3-Clause | 1 |
| BSL-1.0 | 1 |
| Apache-2.0 / MIT | 1 |
| Zlib | 1 |
| Apache-2.0 OR GPL-2.0-only | 1 |

## Dense-Feature-Only Dependencies

The following crates are compiled into the binary only when the `dense` feature is
enabled (`--features dense`). They were omitted from the generated table above
because the NOTICE was generated without that feature flag.

### fancy-regex

- **License**: MIT
- **Repository**: https://github.com/fancy-regex/fancy-regex

### kitoken

- **License**: BSD-2-Clause
- **Repository**: https://github.com/Systemcluster/kitoken

Pure-Rust WordPiece tokenizer; replaced HuggingFace `tokenizers` to drop the
unmaintained `paste` crate (RUSTSEC-2024-0436) and the C/C++ build deps. Pulls
these small permissive transitive crates (dense-only): `bstr`
(`MIT OR Apache-2.0`), `derive_more` (`MIT`), `orx-priority-queue`
(`MIT OR Apache-2.0`), `tinyvec` (`Zlib OR Apache-2.0 OR MIT`), and
`unicode-normalization` (`MIT OR Apache-2.0`), all AGPL-compatible.

### tract / tract-onnx and sub-crates

Sub-crates: `tract-core`, `tract-data`, `tract-extra`, `tract-hir`, `tract-linalg`,
`tract-nnef`, `tract-onnx-opl`, `tract-pulse`, `tract-pulse-opl`, `tract-transformers`.

- **License**: Apache-2.0 OR MIT
- **Repository**: https://github.com/sonos/tract

---

## Attribution

All third-party packages are used in accordance with their respective licenses.
No proprietary code is bundled or modified without explicit permission.

---
Generated by Aden.
