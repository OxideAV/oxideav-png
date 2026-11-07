//! Caller-controlled chunk order (#28): `EncodeOptions::chunk_order`
//! places the ancillary chunks (and `PLTE` / `tRNS` / `IDAT`) in the
//! order the caller lists, within W3C PNG3 §5.6 Table 7, and
//! `parse_chunk_order` reads a file's order back in the same form.
//!
//! The two `magick_order_*` fixtures were written by ImageMagick
//! (`magick -size 4x3 gradient:red-blue -profile "Generic RGB
//! Profile.icc" -density 72 …` and `magick -size 4x3 xc:red -alpha set
//! -channel A -fx 'i/3' +channel -set comment x PNG8:…`); their chunk
//! sequences differ from this encoder's default (a `zTXt` before the
//! `tEXt`s, `cHRM` after `iCCP`), so a re-encode under their parsed
//! order reproducing their sequence exercises the whole path.

use oxideav_png::{
    Bkgd, ChunkOrder, ChunkSlot, EncodeOptions, Gama, Phys, PhysUnit, PngImage, PngMetadata, Text,
    Time, UnknownChunk, Ztxt, decode, encode, encode_apng_with_options, encode_into, parse_apng,
    parse_chunk_order, parse_metadata,
};

const ICCP_ZTXT: &[u8] = include_bytes!("fixtures/magick_order_iccp_ztxt.png");
const PALETTE_TRNS: &[u8] = include_bytes!("fixtures/magick_order_palette_trns.png");

/// The chunk sequence of `png`, each text chunk tagged with its keyword.
fn sequence(png: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 8;
    while i + 8 <= png.len() {
        let n = u32::from_be_bytes(png[i..i + 4].try_into().unwrap()) as usize;
        let t = String::from_utf8_lossy(&png[i + 4..i + 8]).into_owned();
        let data = &png[i + 8..i + 8 + n];
        if matches!(t.as_str(), "tEXt" | "zTXt" | "iTXt") {
            let kw = data.split(|&b| b == 0).next().unwrap();
            out.push(format!("{t}({})", String::from_utf8_lossy(kw)));
        } else {
            out.push(t);
        }
        i += 12 + n;
    }
    out
}

fn rgb_4x2() -> PngImage {
    PngImage::from_rgb8(4, 2, (0..24).collect()).unwrap()
}

fn texts(n: usize) -> Vec<Text> {
    (0..n)
        .map(|i| Text::new(format!("k{i}"), format!("v{i}")))
        .collect()
}

/// Decode a magick file, then encode its pixels with its metadata under
/// its own parsed order: the chunk sequence (text keywords included),
/// the metadata and the pixels all come back.
#[test]
fn reencode_under_parsed_order_reproduces_magick_layout() {
    for (name, file) in [("iccp_ztxt", ICCP_ZTXT), ("palette_trns", PALETTE_TRNS)] {
        let image = decode(file).unwrap();
        let meta = parse_metadata(file).unwrap();
        let order = parse_chunk_order(file).unwrap();
        let opts = EncodeOptions::new()
            .with_metadata(meta.clone())
            .with_chunk_order(order.clone());
        let out = encode(&image, &opts).unwrap();
        assert_eq!(sequence(&out), sequence(file), "{name}: order {order}");
        assert_eq!(parse_metadata(&out).unwrap(), meta, "{name}");
        assert_eq!(parse_chunk_order(&out).unwrap(), order, "{name}");
        assert_eq!(decode(&out).unwrap().planes, image.planes, "{name}");
    }
}

/// The parsed order of the iCCP fixture is what magick wrote: `iCCP`
/// first, the `zTXt` ahead of the three `tEXt`s.
#[test]
fn parse_chunk_order_reports_file_order() {
    let order = parse_chunk_order(ICCP_ZTXT).unwrap();
    assert_eq!(
        order.to_string(),
        "iCCP,cHRM,bKGD,pHYs,tIME,zTXt#0,tEXt#0,tEXt#1,tEXt#2,IDAT"
    );
    // The default order differs: text before compressed text, gAMA /
    // cHRM after iCCP but cHRM before bKGD either way.
    let image = decode(ICCP_ZTXT).unwrap();
    let meta = parse_metadata(ICCP_ZTXT).unwrap();
    let out = encode(&image, &EncodeOptions::new().with_metadata(meta)).unwrap();
    assert_ne!(sequence(&out), sequence(ICCP_ZTXT));
}

/// Text chunks and `tIME` may go after `IDAT` (Table 7 "None"); single
/// instances interleave with whole-type slots.
#[test]
fn text_and_time_after_idat_and_interleaved_instances() {
    let meta = PngMetadata::default()
        .with_texts(texts(3))
        .with_ztxts(vec![Ztxt::new("z".into(), "zz".into())])
        .with_time(Time::new(2026, 11, 8, 12, 0, 0))
        .with_phys(Phys::new(2835, 2835, PhysUnit::Metre))
        .with_gama(Gama::new(45455));
    let order = ChunkOrder::new()
        .with(*b"pHYs")
        .with(ChunkSlot::instance(*b"tEXt", 1))
        .with(*b"zTXt")
        .with(*b"IDAT")
        .with(*b"tEXt")
        .with(*b"tIME");
    let opts = EncodeOptions::new()
        .with_metadata(meta.clone())
        .with_chunk_order(order);
    let out = encode(&rgb_4x2(), &opts).unwrap();
    assert_eq!(
        sequence(&out),
        [
            "IHDR", "pHYs", "tEXt(k1)", "zTXt(z)", "gAMA", "IDAT", "tEXt(k0)", "tEXt(k2)", "tIME",
            "IEND"
        ]
    );
    // The decoder reads text after IDAT back in file order.
    let back = parse_metadata(&out).unwrap();
    let kws: Vec<_> = back.texts.iter().map(|t| t.keyword.as_str()).collect();
    assert_eq!(kws, ["k1", "k0", "k2"]);
    assert_eq!(back.time, meta.time);
}

/// Slots naming chunks the encode does not write are skipped, so one
/// order serves images with and without the chunk; unlisted chunks keep
/// their default place.
#[test]
fn absent_slots_are_skipped_and_unlisted_chunks_keep_defaults() {
    let order: ChunkOrder = "sRGB,iCCP,cHRM,gAMA,PLTE,bKGD,pHYs,tIME,tEXt,IDAT"
        .parse()
        .unwrap();
    let meta = PngMetadata::default()
        .with_gama(Gama::new(50000))
        .with_bkgd(Bkgd::Rgb(1, 2, 3))
        .with_texts(texts(1));
    let opts = EncodeOptions::new()
        .with_metadata(meta)
        .with_chunk_order(order);
    let out = encode(&rgb_4x2(), &opts).unwrap();
    assert_eq!(
        sequence(&out),
        ["IHDR", "gAMA", "bKGD", "tEXt(k0)", "IDAT", "IEND"]
    );
    // Unlisted chunks go just before IDAT (before PLTE for the colour
    // chunks, when there is one), after the listed chunks.
    let meta = PngMetadata::default()
        .with_time(Time::new(2026, 1, 1, 0, 0, 0))
        .with_texts(texts(1))
        .with_gama(Gama::new(50000));
    let opts = EncodeOptions::new()
        .with_metadata(meta)
        .with_chunk_order("tEXt".parse::<ChunkOrder>().unwrap());
    let out = encode(&rgb_4x2(), &opts).unwrap();
    assert_eq!(
        sequence(&out),
        ["IHDR", "tEXt(k0)", "gAMA", "tIME", "IDAT", "IEND"]
    );
}

/// An empty order (or `None`) writes the default layout byte for byte.
#[test]
fn empty_order_is_the_default_layout() {
    let meta = PngMetadata::default()
        .with_gama(Gama::new(45455))
        .with_texts(texts(2))
        .with_time(Time::new(2026, 1, 1, 0, 0, 0));
    let a = encode(
        &rgb_4x2(),
        &EncodeOptions::new().with_metadata(meta.clone()),
    )
    .unwrap();
    let b = encode(
        &rgb_4x2(),
        &EncodeOptions::new()
            .with_metadata(meta)
            .with_chunk_order(ChunkOrder::new()),
    )
    .unwrap();
    assert_eq!(a, b);
}

/// Every Table 7 / §14.2 violation is an encode error naming the rule,
/// and `encode_into` leaves the caller's buffer as it was.
#[test]
fn table7_violations_are_rejected() {
    let pal = decode(PALETTE_TRNS).unwrap();
    let pal_meta = parse_metadata(PALETTE_TRNS).unwrap();
    let cases: [(&str, &str); 6] = [
        // cHRM is "Before PLTE and IDAT".
        ("PLTE,cHRM", "shall precede PLTE"),
        // bKGD / tRNS are "After PLTE; before IDAT".
        ("bKGD,PLTE", "shall follow PLTE"),
        ("tRNS,PLTE", "shall follow PLTE"),
        ("IDAT,bKGD", "shall follow PLTE and precede IDAT"),
        // IHDR's place is fixed.
        ("IHDR,cHRM", "fixed position"),
        ("tEXt#0,tEXt#0", "listed twice"),
    ];
    for (order, needle) in cases {
        let opts = EncodeOptions::new()
            .with_metadata(pal_meta.clone())
            .with_chunk_order(order.parse::<ChunkOrder>().unwrap());
        let err = encode(&pal, &opts).unwrap_err().to_string();
        assert!(err.contains(needle), "{order}: {err}");
        let mut buf = vec![7u8; 3];
        assert!(encode_into(&pal, &opts, &mut buf).is_err());
        assert_eq!(buf, [7, 7, 7]);
    }
    // pHYs is "Before IDAT".
    let opts = EncodeOptions::new()
        .with_metadata(PngMetadata::default().with_phys(Phys::new(1, 1, PhysUnit::Unknown)))
        .with_chunk_order("IDAT,pHYs".parse::<ChunkOrder>().unwrap());
    let err = encode(&rgb_4x2(), &opts).unwrap_err().to_string();
    assert!(err.contains("pHYs shall precede IDAT"), "{err}");
    // An unrecognised chunk stays on its side of IDAT (§14.2).
    let opts = EncodeOptions::new()
        .with_metadata(PngMetadata::default().with_unknowns(vec![UnknownChunk::new(
            *b"prVt",
            vec![1, 2],
            true,
        )]))
        .with_chunk_order("prVt,IDAT".parse::<ChunkOrder>().unwrap());
    let err = encode(&rgb_4x2(), &opts).unwrap_err().to_string();
    assert!(err.contains("§14.2"), "{err}");
}

/// Without a `PLTE`, the colour chunks and `bKGD` may come in any order
/// before `IDAT` (only the `IDAT` half of their rule applies).
#[test]
fn no_plte_relaxes_the_plte_constraints() {
    let meta = PngMetadata::default()
        .with_gama(Gama::new(45455))
        .with_bkgd(Bkgd::Rgb(0, 0, 0));
    let opts = EncodeOptions::new()
        .with_metadata(meta)
        .with_chunk_order("bKGD,PLTE,gAMA".parse::<ChunkOrder>().unwrap());
    let out = encode(&rgb_4x2(), &opts).unwrap();
    assert_eq!(sequence(&out), ["IHDR", "bKGD", "gAMA", "IDAT", "IEND"]);
    parse_metadata(&out).unwrap();
}

/// The order applies to APNG: `IDAT` stands for the whole animation, so
/// a text chunk placed after it follows the last `fdAT`.
#[test]
fn apng_honours_the_order() {
    let frames = [rgb_4x2(), rgb_4x2()];
    let meta = PngMetadata::default()
        .with_texts(texts(2))
        .with_gama(Gama::new(45455));
    let opts = EncodeOptions::new()
        .with_metadata(meta)
        .with_chunk_order("tEXt#1,IDAT,tEXt".parse::<ChunkOrder>().unwrap());
    let out = encode_apng_with_options(&frames, 10, 0, &opts).unwrap();
    assert_eq!(
        sequence(&out),
        [
            "IHDR", "acTL", "tEXt(k1)", "gAMA", "fcTL", "IDAT", "fcTL", "fdAT", "tEXt(k0)", "IEND"
        ]
    );
    assert_eq!(parse_apng(&out).unwrap().frames.len(), 2);
    assert_eq!(
        parse_chunk_order(&out).unwrap().to_string(),
        "tEXt#0,gAMA,IDAT,tEXt#1"
    );
}

/// Black-box: ImageMagick reads a file whose text and `tIME` follow
/// `IDAT` and whose palette fixture was re-laid out, and finds the text
/// after `IDAT`. Skipped when `magick` is not on `PATH`.
#[test]
fn magick_reads_reordered_files() {
    use std::process::Command;
    let have = Command::new("magick")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !have {
        eprintln!("magick not on PATH; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("oxideav-png-chunk-order-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let meta = PngMetadata::default()
        .with_texts(vec![Text::new("comment".into(), "after IDAT".into())])
        .with_time(Time::new(2026, 11, 8, 12, 0, 0))
        .with_gama(Gama::new(45455));
    let opts = EncodeOptions::new()
        .with_metadata(meta)
        .with_chunk_order("IDAT,tIME,tEXt".parse::<ChunkOrder>().unwrap());
    let a = dir.join("text_after_idat.png");
    std::fs::write(&a, encode(&rgb_4x2(), &opts).unwrap()).unwrap();

    let pal = decode(PALETTE_TRNS).unwrap();
    let opts = EncodeOptions::new()
        .with_metadata(parse_metadata(PALETTE_TRNS).unwrap())
        .with_chunk_order(
            "tEXt#1,cHRM,PLTE,bKGD,tRNS,IDAT,tEXt,tIME"
                .parse::<ChunkOrder>()
                .unwrap(),
        );
    let b = dir.join("palette_relaid.png");
    std::fs::write(&b, encode(&pal, &opts).unwrap()).unwrap();

    let out = Command::new("magick")
        .args(["identify", "-format", "%c|%[type]\n"])
        .arg(&a)
        .arg(&b)
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].starts_with("after IDAT|"), "{text}");
    assert!(lines[1].starts_with("x|"), "{text}");
}
