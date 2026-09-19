fn main() {
    use divisi_brand::frames::{loop_frames, to_json};
    println!("{}", to_json(30, &loop_frames(30)));
}
