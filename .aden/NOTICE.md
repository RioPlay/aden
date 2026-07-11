# Third-Party Dependencies

This project uses open-source packages. Keep this file current whenever
dependencies change so consumers can verify license compatibility.

## Generating Attribution

Depending on your project's language/package manager:

- **Rust/Cargo:**   `aden licenses --out NOTICE.md` (reads Cargo.lock)
- **Node/npm:**     `npx license-checker --out NOTICE.md`
- **Python:**       `pip-licenses --format=markdown > NOTICE.md`
- **Go:**           `go-licenses save ./... --save_path=NOTICE.md`

Always verify that third-party licenses are compatible with your project's license.
