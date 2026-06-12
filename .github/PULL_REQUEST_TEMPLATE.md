## Summary

<!-- What does this PR do and why? -->

## Test plan

- [ ] `cargo test --workspace` green
- [ ] `cargo clippy --workspace` clean
- [ ] `cargo fmt --all` applied

## License checklist (required for any Cargo.toml change)

If this PR adds, removes, or updates any dependency, check all boxes:

- [ ] `cargo deny check` passes (run locally or CI license-check job is green)
- [ ] Compared `aden licenses` output against `NOTICE.md` dep table — no new entries missing
- [ ] If new MPL/LGPL/GPL dep: added preamble entry **and** dep table entry **and** updated license summary count in `NOTICE.md`
- [ ] If new compiled-in MIT dep with a named copyright holder: copyright string present in dep table entry
- [ ] If dep added behind `--features dense`: entry added to the Dense-Feature-Only section in `NOTICE.md`
- [ ] If vendored JS asset added or changed: in-file copyright banner present **and** `CHECKSUMS` regenerated (`sha256sum -c crates/aden-cli/assets/CHECKSUMS`)

*If this PR makes no Cargo.toml changes, check this box instead:*
- [ ] No dependency changes — license checklist not applicable
