use blast::{
    audio_processing::runtime::run_blast,
    file_parsing::{decode_helpers::DecodeResult, load_assets},
};

fn main() -> DecodeResult<()> {
    // assets dir can be given as the first argument
    let dir: String = std::env::args()
        .nth(1)
        .unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/assets/").to_string());

    let (tracks, mutual_rate, num_channels) = match load_assets(&dir) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("Error: could not load assets from '{dir}': {:?}", error);
            std::process::exit(1);
        }
    };

    println!("Mutual sample_rate: {mutual_rate}");
    println!("Num channels: {num_channels}");

    println!("Loaded tracks [");
    for (track, _) in &tracks {
        println!("\t{}", track);
    }
    println!("]");

    run_blast(tracks, mutual_rate, num_channels);

    Ok(())
}
