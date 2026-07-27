use super::syntax::TestLineCollector;

#[test]
fn inline_test_module_lines_are_classified_as_test() {
    let source = "fn production() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn works() {}\n}\n";
    let syntax = syn::parse_file(source).expect("fixture parses");
    let mut collector = TestLineCollector::new(7);
    collector.visit_file(&syntax).expect("classification succeeds");
    assert_eq!(collector.test_line_count(), 5);
}
