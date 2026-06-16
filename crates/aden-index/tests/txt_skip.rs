// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
// `Index::from_directory` indexes AsciiDoc (.adoc/.aden) on disk but deliberately
// skips `.txt`: gen emits one paragraph `Note` per `.txt` paragraph into the
// store, which load_or_build_index merges in. A file-level `.txt` blob here would
// only duplicate that paragraph-granular coverage with a coarser, less-dense
// entry (the `note` vs `note.txt#p1` pair).

use aden_index::Index;

#[test]
fn from_directory_skips_txt_but_indexes_adoc() {
    let dir = std::env::temp_dir().join(format!("aden_txt_skip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("note.txt"), "zzqxnotetoken plain text body\n").unwrap();
    std::fs::write(
        dir.join("doc.adoc"),
        "[[d]]\n= Doc\n\nzzqxadoctoken in asciidoc\n",
    )
    .unwrap();

    let index = Index::from_directory(&dir).unwrap();

    assert!(
        !index.query("zzqxadoctoken").is_empty(),
        "the .adoc file must be indexed on disk"
    );
    assert!(
        index.query("zzqxnotetoken").is_empty(),
        ".txt must NOT be indexed file-level — its paragraph Notes come from the store"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
