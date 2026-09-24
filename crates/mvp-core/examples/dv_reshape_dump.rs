//! Write one decoded frame's Dolby Vision base layer — reshaped, and *not* tone
//! mapped — as a PNG, so it can be compared byte for byte against the same
//! frame rendered by libplacebo.
//!
//! This exists because the reshaping is the one part of the pipeline whose
//! correctness cannot be argued from the code alone: it is a curve with
//! coefficients that came out of somebody else's bitstream. The ffmpeg build
//! this repository links carries the `libplacebo` filter, which applies the same
//! RPU the way mpv does, so rendering the same frame twice and comparing the
//! bytes is a real check rather than a self-consistency test. `tmp/dv-compare.ps1`
//! drives both sides and reports the difference.
//!
//! ```text
//! cargo run -p mvp-core --example dv_reshape_dump -- dolby.mkv 0 out.png
//! ```
//!
//! The output is full-range 8-bit RGB in the signal's own encoding (PQ or HLG,
//! BT.2020 primaries), which is what the scaler produces *before* the tone
//! mapper touches it: that is the stage libplacebo's `apply_dolbyvision` also
//! produces, so the two can be compared directly.

use std::path::PathBuf;

use ffmpeg_next as ffmpeg;

use mvp_core::{FramePool, RgbaConverter};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: dv_reshape_dump <file> [frame-index] [out.png]");
        std::process::exit(2);
    };
    let index = args
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let out = PathBuf::from(args.next().unwrap_or_else(|| "dv-reshape.png".to_string()));

    let info = match mvp_core::info::probe(std::path::Path::new(&path)) {
        Ok(info) => info,
        Err(err) => {
            eprintln!("could not probe {path}: {err}");
            std::process::exit(1);
        }
    };
    if let Some(video) = info.primary_video() {
        println!("{}: {}", path, video.hdr.label());
    }

    let pool = FramePool::new(64 * 1024 * 1024);
    let mut converter = RgbaConverter::new();
    // No tone mapping: the point is to see the reshaping, in the signal's own
    // encoding, before anything else touches it.
    converter.set_tone_map(false);
    converter.set_dv_reshape(true);
    converter.set_dovi_config(info.primary_video().and_then(|video| video.hdr.dovi));

    let frame = match decode_frame(&path, index, &mut converter, &pool) {
        Ok(frame) => frame,
        Err(err) => {
            eprintln!("could not decode frame {index}: {err}");
            std::process::exit(1);
        }
    };
    println!(
        "frame {index}: {}x{}, reshaping {:?}",
        frame.width, frame.height, frame.reshape
    );

    let image = match image::RgbaImage::from_raw(frame.width, frame.height, frame.pixels.clone()) {
        Some(image) => image,
        None => {
            eprintln!("the frame buffer is not a whole image");
            std::process::exit(1);
        }
    };
    match image.save(&out) {
        Ok(()) => println!("wrote {}", out.display()),
        Err(err) => {
            eprintln!("could not write {}: {err}", out.display());
            std::process::exit(1);
        }
    }
}

/// What the dump needs to know about one converted frame.
struct Dumped {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    /// What the converter did with the reshaping.
    reshape: mvp_core::video::DvReshapeState,
}

/// Decode to `index` and convert that frame with `converter`.
fn decode_frame(
    path: &str,
    index: usize,
    converter: &mut RgbaConverter,
    pool: &FramePool,
) -> Result<Dumped, String> {
    mvp_core::init().map_err(|err| err.to_string())?;
    let mut ictx = ffmpeg::format::input(&path).map_err(|err| err.to_string())?;
    let stream = ictx
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or_else(|| "no video stream".to_string())?;
    let stream_index = stream.index();
    let params = {
        let mut owned = ffmpeg::codec::Parameters::new();
        // SAFETY: both pointers are live for the call.
        unsafe {
            ffmpeg::ffi::avcodec_parameters_copy(owned.as_mut_ptr(), stream.parameters().as_ptr());
        }
        owned
    };
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(params)
        .map_err(|err| err.to_string())?
        .decoder()
        .video()
        .map_err(|err| err.to_string())?;

    let mut frame = ffmpeg::frame::Video::empty();
    let mut seen = 0usize;
    for (stream, packet) in ictx.packets() {
        if stream.index() != stream_index || decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            if seen == index {
                let (width, height) = (frame.width(), frame.height());
                let pixels = converter
                    .convert(&frame, width, height, pool)
                    .map_err(|err| err.to_string())?;
                return Ok(Dumped {
                    width,
                    height,
                    pixels,
                    reshape: converter.dv_reshape_state(),
                });
            }
            seen += 1;
        }
    }
    Err(format!("the file has fewer than {} frames", index + 1))
}
