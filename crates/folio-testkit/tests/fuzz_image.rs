//! Replays the seed inputs of the `image` fuzz target
//! (`folio_testkit::fuzz_image`): every corpus image and the hostile
//! figures of #333, embedded and converted, finish without a panic.

#[test]
fn image_seeds() {
    let seeds = folio_testkit::fuzz_image::seeds();
    assert!(seeds.len() >= 20, "{} seeds", seeds.len());
    for (name, input) in seeds {
        eprintln!("{name}");
        folio_testkit::fuzz_image::image(&input);
    }
}
