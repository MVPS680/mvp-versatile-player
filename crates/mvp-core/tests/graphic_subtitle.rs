//! Graphical (bitmap) subtitle material.
//!
//! These tests are opt-in: a real PGS/VobSub sample is not something the
//! repository can carry, and the FFmpeg shared build used here has no PGS
//! *encoder* to generate one from a text track. They read the path from the
//! `MVP_PGS_SAMPLE` environment variable and skip when it is unset:
//!
//! ```text
//! set MVP_PGS_SAMPLE=C:\path\to\movie.sup
//! cargo test -p mvp-core --test graphic_subtitle -- --nocapture
//! ```
//!
//! What they guard: that an external graphical file is demuxed, decoded to
//! bitmap cues and carries usable pixels — a route the unit tests cover only
//! with a hand-built `AVSubtitle`.

use std::path::PathBuf;

use mvp_core::bitmap_subtitle::{decode_external, is_graphic_subtitle};

/// Path of the sample, or `None` when the test should skip.
fn sample() -> Option<PathBuf> {
    let raw = std::env::var_os("MVP_PGS_SAMPLE")?;
    let path = PathBuf::from(raw);
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipping: {} does not exist", path.display());
        None
    }
}

#[test]
fn an_external_graphical_file_decodes_into_bitmap_cues() {
    let Some(path) = sample() else {
        return;
    };

    assert!(
        is_graphic_subtitle(&path),
        "{} should be recognised as a graphical subtitle",
        path.display()
    );

    let track = match decode_external(&path) {
        Ok(track) => track,
        Err(err) => panic!("decoding {} failed: {err}", path.display()),
    };
    assert!(!track.is_empty(), "no cues were decoded from the sample");

    for (index, cue) in track.cues.iter().enumerate() {
        assert!(
            cue.end > cue.start,
            "cue {index} has no duration: {}..{}",
            cue.start,
            cue.end
        );
        assert!(!cue.rects.is_empty(), "cue {index} has no rectangles");
        for rect in &cue.rects {
            assert!(rect.width > 0 && rect.height > 0);
            assert_eq!(
                rect.indices.len(),
                rect.width as usize * rect.height as usize,
                "cue {index} index plane has the wrong size"
            );
            let rgba = rect.rgba();
            assert_eq!(rgba.len(), rect.indices.len() * 4);
        }
    }

    // The first cue must be found by the lookup the interface uses.
    let first = &track.cues[0];
    assert!(
        track.active_at((first.start + first.end) / 2.0).is_some(),
        "the first cue is not active in the middle of its own range"
    );
}
