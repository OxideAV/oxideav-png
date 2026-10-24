//! The registry path carries the framework's metadata-blob and
//! palette-alpha records: `icc` / `exif` / `xmp` blobs on a frame (or
//! on the stream's `CodecParameters`) become `iCCP` / `eXIf` / the XMP
//! `iTXt`, a palette-alpha record becomes `tRNS`, and decoding hands
//! the same records back on every frame and on the demuxed stream.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{
    BlobKind, CodecId, CodecParameters, Frame, MetadataBlob, PixelFormat, RuntimeContext,
    VideoFrame, VideoPlane,
};
use oxideav_png::{EncodeOptions, Metadata, PngImage, PngPixelFormat};

const W: u32 = 7;
const H: u32 = 5;

fn context() -> RuntimeContext {
    let mut ctx = RuntimeContext::new();
    oxideav_png::register(&mut ctx);
    ctx
}

fn icc() -> Vec<u8> {
    (0..3000u32).map(|i| (i * 13 % 251) as u8).collect()
}

fn exif() -> Vec<u8> {
    let mut e = b"MM\0*\0\0\0\x08".to_vec();
    e.extend_from_slice(&[0, 0, 0, 0]);
    e
}

fn xmp() -> Vec<u8> {
    b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">blob</x:xmpmeta>".to_vec()
}

fn rgba_frame() -> VideoFrame {
    VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: W as usize * 4,
            data: (0..W * H * 4).map(|i| (i * 7) as u8).collect(),
        }],
    }
}

fn params(pix: PixelFormat) -> CodecParameters {
    let mut p = CodecParameters::video(CodecId::new("png"));
    p.width = Some(W);
    p.height = Some(H);
    p.pixel_format = Some(pix);
    p
}

/// Drive the framework encoder over one frame.
fn trait_encode(params: &CodecParameters, frame: VideoFrame) -> Vec<u8> {
    let mut enc = oxideav_png::make_encoder(params).unwrap();
    enc.send_frame(&Frame::Video(frame)).unwrap();
    enc.flush().unwrap();
    enc.receive_packet().unwrap().data
}

/// The metadata the standalone decoder reads back.
fn decoded_metadata(png: &[u8]) -> Metadata {
    oxideav_png::decode(png).unwrap().metadata
}

#[test]
fn frame_blob_records_become_iccp_exif_and_xmp() {
    let frame = rgba_frame()
        .with_blob(MetadataBlob::new(BlobKind::ICC, icc()))
        .with_blob(MetadataBlob::new(BlobKind::EXIF, exif()))
        .with_blob(MetadataBlob::new(BlobKind::XMP, xmp()));
    let single = oxideav_png::encode_single(&frame, W, H, PixelFormat::Rgba, &[]).unwrap();
    let via_trait = trait_encode(&params(PixelFormat::Rgba), frame.clone());
    assert_eq!(single, via_trait, "encode_single and the Encoder agree");
    let meta = decoded_metadata(&single);
    assert_eq!(meta.icc, Some(icc()));
    assert_eq!(meta.exif, Some(exif()));
    assert_eq!(meta.xmp, Some(xmp()));

    // The same file `encode` writes for an image carrying that metadata.
    let image = PngImage::packed(
        W,
        H,
        PngPixelFormat::Rgba,
        W as usize * 4,
        frame.planes[0].data.clone(),
    )
    .unwrap()
    .with_metadata(
        Metadata::new()
            .with_icc(icc())
            .with_exif(exif())
            .with_xmp(xmp()),
    );
    assert_eq!(
        single,
        oxideav_png::encode(&image, &EncodeOptions::default()).unwrap()
    );
}

#[test]
fn stream_blobs_fill_the_kinds_the_frame_does_not_carry() {
    let stream_icc: Vec<u8> = vec![9; 500];
    let p = params(PixelFormat::Rgba)
        .with_blob(MetadataBlob::new(BlobKind::ICC, stream_icc.clone()))
        .with_blob(MetadataBlob::new(BlobKind::EXIF, exif()));
    // Stream only.
    let meta = decoded_metadata(&trait_encode(&p, rgba_frame()));
    assert_eq!(meta.icc, Some(stream_icc));
    assert_eq!(meta.exif, Some(exif()));
    assert_eq!(meta.xmp, None);
    // The frame's ICC overrides the stream's; the stream's Exif stays.
    let frame = rgba_frame().with_blob(MetadataBlob::new(BlobKind::ICC, icc()));
    let meta = decoded_metadata(&trait_encode(&p, frame));
    assert_eq!(meta.icc, Some(icc()));
    assert_eq!(meta.exif, Some(exif()));
}

#[test]
fn kinds_png_has_no_chunk_for_are_dropped() {
    let plain = trait_encode(&params(PixelFormat::Rgba), rgba_frame());
    let frame = rgba_frame()
        .with_blob(MetadataBlob::new(BlobKind::COVER_ART, vec![1, 2, 3]).with_mime("image/jpeg"))
        .with_blob(MetadataBlob::new(BlobKind::IPTC, vec![0x1C, 2, 0]));
    assert_eq!(trait_encode(&params(PixelFormat::Rgba), frame), plain);
}

#[test]
fn the_codec_declares_the_kinds_it_embeds() {
    let ctx = context();
    let (_, imp) = ctx
        .codecs
        .all_implementations()
        .find(|(id, i)| id.as_str() == "png" && i.make_encoder.is_some())
        .expect("png encoder registered");
    for kind in [BlobKind::ICC, BlobKind::EXIF, BlobKind::XMP] {
        assert!(imp.caps.embeds_blob(&kind), "{kind}");
    }
    assert!(!imp.caps.embeds_blob(&BlobKind::COVER_ART));
}

/// A `Pal8` frame's palette-alpha record is the file's `tRNS`; decoding
/// gives it back as the frame's palette-alpha record.
#[test]
fn palette_alpha_record_round_trips_as_trns() {
    let rgb: Vec<u8> = (0..4 * 3).map(|i| (i * 20) as u8).collect();
    let alpha = vec![0u8, 128];
    let indices: Vec<u8> = (0..W * H).map(|i| (i % 4) as u8).collect();
    let frame = VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: W as usize,
            data: indices.clone(),
        }],
    }
    .with_palette(rgb.clone())
    .with_palette_alpha(alpha.clone());
    let png = trait_encode(&params(PixelFormat::Pal8), frame.clone());
    assert_eq!(
        png,
        oxideav_png::encode_single(&frame, W, H, PixelFormat::Pal8, &[]).unwrap()
    );

    let image = oxideav_png::decode(&png).unwrap();
    let palette = image.palette.as_ref().unwrap();
    assert_eq!(palette.to_rgb(), rgb);
    assert_eq!(palette.alpha_tail(), Some(alpha.clone()));
    let rgba = oxideav_png::decode_rgba8(&png).unwrap();
    assert_eq!(rgba.data[3], 0, "index 0 is transparent");
    assert_eq!(rgba.data[7], 128, "index 1 is half transparent");
    assert_eq!(rgba.data[11], 255, "index 2 is opaque");

    let back = VideoFrame::from(&image);
    assert_eq!(back.palette(), Some(&rgb[..]));
    assert_eq!(back.palette_alpha(), Some(&alpha[..]));

    // An opaque palette carries no alpha record either way.
    let opaque = VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: W as usize,
            data: indices,
        }],
    }
    .with_palette(rgb);
    let png = trait_encode(&params(PixelFormat::Pal8), opaque);
    assert!(!png.windows(4).any(|w| w == b"tRNS"));
    let back = VideoFrame::from(&oxideav_png::decode(&png).unwrap());
    assert_eq!(back.palette_alpha(), None);
}

/// Decoding through the registry hands the file's ICC / Exif / XMP
/// back as blob records on the stream and on every frame, so a
/// decode → encode chain keeps them.
#[test]
fn decoded_frames_and_the_demuxed_stream_carry_the_blobs() {
    let frame = rgba_frame()
        .with_blob(MetadataBlob::new(BlobKind::ICC, icc()))
        .with_blob(MetadataBlob::new(BlobKind::EXIF, exif()))
        .with_blob(MetadataBlob::new(BlobKind::XMP, xmp()));
    let png = trait_encode(&params(PixelFormat::Rgba), frame);

    let ctx = context();
    let mut dm = ctx
        .containers
        .open_demuxer("png", Box::new(Cursor::new(png.clone())), &ctx.codecs)
        .unwrap();
    let info = dm.streams()[0].clone();
    let expect = [
        (BlobKind::ICC, icc()),
        (BlobKind::EXIF, exif()),
        (BlobKind::XMP, xmp()),
    ];
    for (kind, data) in &expect {
        assert_eq!(
            info.params.blob(kind).map(|b| b.data.clone()),
            Some(data.clone()),
            "stream {kind}"
        );
    }
    let mut dec = ctx.codecs.first_decoder(&info.params).unwrap();
    dec.send_packet(&dm.next_packet().unwrap()).unwrap();
    let Frame::Video(decoded) = dec.receive_frame().unwrap() else {
        panic!("video frame");
    };
    for (kind, data) in &expect {
        assert_eq!(
            decoded.blob(kind).map(|b| b.data),
            Some(data.clone()),
            "frame {kind}"
        );
    }

    // Re-encoding the decoded frame writes the same file.
    let again = trait_encode(&info.params, decoded);
    assert_eq!(again, png);
}

/// `PngImage::from_video_frame` reads the same records.
#[test]
fn from_video_frame_reads_blob_records() {
    let p = params(PixelFormat::Rgba).with_blob(MetadataBlob::new(BlobKind::XMP, xmp()));
    let frame = rgba_frame().with_blob(MetadataBlob::new(BlobKind::ICC, icc()));
    let image = PngImage::from_video_frame(&frame, &p).unwrap();
    assert_eq!(image.metadata.icc, Some(icc()));
    assert_eq!(image.metadata.xmp, Some(xmp()));
    assert_eq!(image.metadata.exif, None);
}
