//! Throwaway: dump every (artist, album) pair as TSV for collection diffing.

fn main() -> Result<(), String> {
    let lib = zytunes::load_library(None)?;
    for (artist, album) in lib.albums() {
        println!("{artist}\t{album}");
    }
    Ok(())
}
