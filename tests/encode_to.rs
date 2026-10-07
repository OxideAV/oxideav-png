//! `encode_to` streams the file to its writer a run of chunks at a
//! time instead of building the whole file first.

use oxideav_png::{encode, encode_to, EncodeOptions, Iccp, PngError, PngImage, PngMetadata};

/// Records every `write` call so a test can see how the bytes arrived.
#[derive(Default)]
struct Recorder {
    writes: Vec<Vec<u8>>,
}

impl std::io::Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writes.push(buf.to_vec());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Offset of the first chunk of type `ty` in a PNG file.
fn chunk_offset(png: &[u8], ty: &[u8; 4]) -> usize {
    let mut p = 8;
    while p + 12 <= png.len() {
        let len = u32::from_be_bytes([png[p], png[p + 1], png[p + 2], png[p + 3]]) as usize;
        if &png[p + 4..p + 8] == ty {
            return p;
        }
        p += 12 + len;
    }
    panic!("no {ty:?} chunk");
}

fn image() -> PngImage {
    let data: Vec<u8> = (0..64 * 32 * 3).map(|i| (i * 7 % 251) as u8).collect();
    PngImage::from_rgb8(64, 32, data).unwrap()
}

fn opts_with_profile() -> EncodeOptions {
    let profile: Vec<u8> = (0..20_000u32).map(|i| (i * 31 % 253) as u8).collect();
    EncodeOptions::default()
        .with_metadata(PngMetadata::default().with_iccp(Iccp::new("p".to_string(), profile)))
}

#[test]
fn encode_to_writes_the_header_run_then_the_idat_chunk_then_the_trailer() {
    let img = image();
    let opts = opts_with_profile();
    let whole = encode(&img, &opts).unwrap();
    let mut rec = Recorder::default();
    encode_to(&img, &opts, &mut rec).unwrap();

    assert_eq!(rec.writes.concat(), whole, "same bytes as encode");
    let idat = chunk_offset(&whole, b"IDAT");
    let iend = chunk_offset(&whole, b"IEND");
    assert_eq!(
        rec.writes.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![idat, iend - idat, whole.len() - iend],
        "one write per run: signature to the last chunk before IDAT, IDAT, the trailer"
    );
}

/// An error in the options or the metadata is found before the first
/// run goes out, so the writer is left untouched.
#[test]
fn encode_to_writes_nothing_when_the_metadata_is_rejected() {
    let bad = EncodeOptions::default().with_metadata(
        PngMetadata::default().with_iccp(Iccp::new(" leading space".to_string(), vec![1])),
    );
    let mut rec = Recorder::default();
    assert!(encode_to(&image(), &bad, &mut rec).is_err());
    assert!(rec.writes.is_empty(), "wrote {} runs", rec.writes.len());
}

#[test]
fn encode_to_reports_a_failing_writer() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    match encode_to(&image(), &EncodeOptions::default(), Broken) {
        Err(PngError::Io(e)) => assert_eq!(e.to_string(), "disk full"),
        other => panic!("expected the writer's error, got {other:?}"),
    }
}
