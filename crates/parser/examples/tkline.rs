//! Print tokenize-module tokens with `line`.
fn main() {
    let src = std::env::args().nth(1).unwrap().replace("\\n", "\n");
    for t in rpy_parser::tokenizer::TokenIter::new(&src, true, Default::default()) {
        let t = t.unwrap();
        println!("{} {:?} {:?} {:?} {:?}", t.ty.name(), t.string, t.start, t.end, t.line);
    }
}
