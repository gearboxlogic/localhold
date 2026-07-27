use super::syntax::TestLineCollector;

#[test]
fn inline_test_module_lines_are_classified_as_test() {
    let source = "fn production() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn works() {}\n}\n";
    let syntax = syn::parse_file(source).expect("fixture parses");
    let mut collector = TestLineCollector::new(7);
    collector.visit_file(&syntax).expect("classification succeeds");
    assert_eq!(collector.test_line_count(), 5);
}

#[test]
fn opaque_syntax_fails_closed() {
    let syntax = syn::File {
        shebang: None,
        attrs: Vec::new(),
        items: vec![syn::Item::Verbatim(quote::quote!(opaque tokens))],
    };
    let mut collector = TestLineCollector::new(1);
    assert!(collector.visit_file(&syntax).unwrap_err().to_string().contains("opaque item syntax"));
}

#[test]
fn rust_source_includes_fail_closed() {
    let syntax = syn::parse_file("include!(\"../tests/helper.rs\");\n").expect("fixture parses");
    let mut collector = TestLineCollector::new(1);
    assert!(collector.visit_file(&syntax).unwrap_err().to_string().contains("include!"));
}
