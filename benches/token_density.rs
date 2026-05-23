use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_token_density(c: &mut Criterion) {
    let tmp = std::env::temp_dir().join("aden_bench_token_density");
    std::fs::create_dir_all(&tmp).unwrap();

    let source = r#"
fn main() {
    println!("Hello, world!");
}

fn add(a: i32, b: i32) -> i32 {
    a + b
}
"#;
    let src_path = tmp.join("sample.rs");
    std::fs::write(&src_path, source).unwrap();

    c.bench_function("token_density", |b| {
        b.iter(|| {
            let docs = aden_parse::parse_file(&src_path, black_box(source)).unwrap();
            let emitted = aden_emit::emit(&docs);
            let source_lines = source.lines().count();
            let emitted_len = emitted.len();
            black_box((source_lines, emitted_len));
        })
    });
}

criterion_group!(benches, bench_token_density);
criterion_main!(benches);
