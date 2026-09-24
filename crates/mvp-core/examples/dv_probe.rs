//! Print what a file says about its Dolby Vision, frame by frame at the start.
//!
//! A Dolby Vision file's behaviour depends on two records that are easy to
//! confuse: the *configuration record* the container declares (profile, layers,
//! and the compatibility id that says what the base layer is) and the *reference
//! picture unit* each frame carries (the brightness range, the reshaping curves,
//! the colour matrices). This tool prints both, so a file that looks wrong can be
//! asked what it actually contains instead of being guessed at.
//!
//! ```text
//! cargo run -p mvp-core --example dv_probe -- "C:\path\to\dolby.mkv"
//! cargo run -p mvp-core --example dv_probe -- dolby.mp4 12   # twelve frames
//! ```
//!
//! The second argument is how many frames to read (the default is 8). A file
//! without Dolby Vision prints its dynamic range and nothing else, which is the
//! answer rather than a failure.

use std::path::PathBuf;

use mvp_core::dolby::{self, DvMappingMethod, DvRpu};
use mvp_core::hdr::DoviConfig;
use mvp_core::DvPlan;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: dv_probe <file> [frames]");
        std::process::exit(2);
    };
    let frames = args
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(8);
    let path = PathBuf::from(&path);

    let info = match mvp_core::info::probe(&path) {
        Ok(info) => info,
        Err(err) => {
            eprintln!("could not probe {}: {err}", path.display());
            std::process::exit(1);
        }
    };

    let video = info.primary_video();
    match video {
        Some(video) => println!(
            "{}: {}x{} {}, {} video stream(s) — {}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            video.width,
            video.height,
            video.codec,
            info.video.len(),
            video.hdr.label()
        ),
        None => {
            println!("{}: no video stream", path.display());
            return;
        }
    }
    let record: Option<DoviConfig> = video.and_then(|video| video.hdr.dovi);
    print_record(record);

    if let Some(plan) = DvPlan::resolve(record, None, mvp_core::HdrKind::Sdr) {
        println!("  plan: {}", plan.summary(false));
        for caveat in plan.caveats() {
            println!("  caveat: {caveat}");
        }
    }

    println!("reading up to {frames} frame(s)…");
    let rpus = match dolby::probe_rpus(&path, frames) {
        Ok(rpus) => rpus,
        Err(err) => {
            eprintln!("could not decode: {err}");
            std::process::exit(1);
        }
    };
    if rpus.is_empty() {
        println!("  no reference picture unit on any of those frames");
        return;
    }
    for (index, rpu) in rpus.iter().enumerate() {
        print_rpu(index, rpu);
    }
    let reshaped = rpus.iter().filter(|rpu| rpu.reshapes()).count();
    println!(
        "  {} of {} frames carry a reshaping this player does not apply",
        reshaped,
        rpus.len()
    );
}

fn print_record(record: Option<DoviConfig>) {
    match record {
        Some(record) => {
            println!("  record: {}", record.label());
            println!(
                "    profile {} level {}, rpu {}, el {}, bl {}, compatibility id {}",
                record.profile,
                record.level,
                record.rpu_present,
                record.el_present,
                record.bl_present,
                record.bl_compatibility_id
            );
        }
        None => println!("  record: the container declares none"),
    }
}

fn print_rpu(index: usize, rpu: &DvRpu) {
    if let Some(header) = &rpu.header {
        println!(
            "  frame {index}: rpu type {} format {}, {} -> {} bit, residual disabled {}",
            header.rpu_type,
            header.rpu_format,
            header.bl_bit_depth,
            header.vdr_bit_depth,
            header.disable_residual_flag
        );
    }
    if let Some(signal) = rpu.signal() {
        println!("    signal: {}", signal.describe());
    }
    if let Some(mapping) = &rpu.mapping {
        println!(
            "    mapping: luma {}, chroma {}, nlq {:?}, pivots {:?}",
            method(mapping.luma),
            mapping
                .chroma
                .iter()
                .copied()
                .map(method)
                .collect::<Vec<_>>()
                .join(" / "),
            mapping.nlq,
            mapping.pivots
        );
    }
    if let Some(scene) = rpu.scene() {
        println!(
            "    level 1: {:.1} … {:.1} nits, {:.1} nits average",
            scene.min_nits, scene.max_nits, scene.avg_nits
        );
    }
    if let Some(level2) = rpu.level2 {
        println!(
            "    level 2: {} target(s), first targets {:.0} nits",
            rpu.level2_count,
            mvp_core::pq_code_to_nits(level2.target_max_pq)
        );
    }
    if let Some(area) = rpu.active_area() {
        println!(
            "    level 5: active area {}/{} left-right, {}/{} top-bottom",
            area.left_offset, area.right_offset, area.top_offset, area.bottom_offset
        );
    }
    if let Some(level6) = rpu.level6 {
        println!(
            "    level 6: mastering {} / {}, content light {} / {} (as coded)",
            level6.max_luminance, level6.min_luminance, level6.max_cll, level6.max_fall
        );
    }
}

fn method(method: DvMappingMethod) -> String {
    match method {
        DvMappingMethod::Polynomial { order, pieces } => {
            format!("polynomial order {order}, {pieces} piece(s)")
        }
        DvMappingMethod::Mmr { order, pieces } => {
            format!("MMR order {order}, {pieces} piece(s)")
        }
        DvMappingMethod::None => "none".to_string(),
    }
}
