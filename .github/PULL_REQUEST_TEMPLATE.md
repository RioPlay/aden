## Summary

<!-- What does this PR do and why? -->

## Test plan

- [ ] `cargo test --workspace` green
- [ ] `cargo clippy --workspace` clean
- [ ] `cargo fmt --all` applied

## License checklist (required for dependency or lockfile changes)

If this PR adds, removes, or updates any dependency, check all boxes:

- [ ] `cargo deny --workspace --all-features --locked check` passes locally or in the Quality Gate
- [ ] Ran `python scripts/update_notice.py` and `python scripts/update_notice.py --check`; maintained legal preamble preserved
- [ ] If new MPL/LGPL/GPL dep: added preamble entry **and** dep table entry **and** updated license summary count in `NOTICE.md`
- [ ] If new compiled-in MIT dep with a named copyright holder: copyright and permission notices retained in maintained attribution or accompanying license text
- [ ] Optional-feature dependencies appear in the generated all-feature table; update the maintained feature notes when needed
- [ ] If vendored JS asset added or changed: in-file copyright banner present **and** `CHECKSUMS` verified (`cd crates/aden-cli/assets && sha256sum -c CHECKSUMS`)

*If this PR makes no dependency or lockfile changes, check this box instead:*
- [ ] No dependency changes — license checklist not applicable
