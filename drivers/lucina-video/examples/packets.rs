//! Lists a video file's packets as this crate reads them: `cargo run -p lucina-video --example packets -- <file>`.

fn main() {
    let path = std::env::args().nth(1).expect("a file");
    let file = std::io::BufReader::new(std::fs::File::open(path).expect("open"));
    let mut d = lucina_video::open(file).expect("a video");
    println!("{:?}", d.track().codec);
    println!("{} x {}, {:?} us", d.track().width, d.track().height, d.track().duration_us);
    let mut n = 0;
    while let Some(p) = d.next_packet().expect("a packet") {
        println!("{n} pts_us={} key={} bytes={}", p.pts_us, p.key, p.data.len());
        n += 1;
    }
}
