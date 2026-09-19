fn main() {
    use divisi_brand::frames::{sequences, to_json};
    println!("{}", to_json(30, &sequences(30)));
}
