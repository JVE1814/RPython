// Development aid: compressed .xz sizes of files for each preset (raw LZMA2 size in
// parentheses), with a round-trip check.
// cargo run --release -p rpy-compress --example xzsize -- FILE...
use rpy_compress::lzma::{self, Encoder, FilterSpec, Format, LzOptions};
const FILTER_LZMA2: u64 = 0x21;

fn main() {
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).unwrap();
        let mut line = format!("{path} ({}):", data.len());
        for p in [0u32, 1, 3, 4, 6, 9] {
            let c = lzma::compress_xz(&data, p, 4);
            let (d, _) = lzma::decompress(&c, Format::Xz, None).unwrap();
            assert_eq!(d, data, "round trip failed at preset {p}");
            let mut e = Encoder::new(Format::Raw, 0, vec![FilterSpec::lzma(FILTER_LZMA2, LzOptions::preset(p).unwrap())]).unwrap();
            let mut raw = Vec::new();
            e.compress(&data, &mut raw);
            e.finish(&mut raw);
            line.push_str(&format!(" {p}:{}({})", c.len(), raw.len()));
        }
        println!("{line}");
    }
}
