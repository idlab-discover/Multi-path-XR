use std::env;
use std::fs;
use std::process;

use mp4_box::reader::{parse_mp4_boxes, extract_mdat_boxes};
use mp4_box::writer::{Mp4StreamConfig, create_init_segment, create_media_segment};

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <mp4_file> | --test", args[0]);
        process::exit(1);
    }

    if args[1] == "--test" {
        run_test_mode();
    } else {
        run_file_mode(&args[1]);
    }
}

fn run_file_mode(filename: &str) {
    let data = match fs::read(filename) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to read file '{}': {}", filename, e);
            process::exit(1);
        }
    };

    let boxes = match parse_mp4_boxes(&data) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to parse MP4 boxes: {}", e);
            process::exit(1);
        }
    };

    println!("Parsed {} top-level boxes from '{}':\n", boxes.len(), filename);
    for (i, mp4_box) in boxes.iter().enumerate() {
        println!("Box {}:\n{:#?}\n", i + 1, mp4_box);
    }
}

fn run_test_mode() {
    println!("Running in TEST mode...");

    let config = Mp4StreamConfig {
        timescale: 30 * 1000,
        width: 1920,
        height: 1080,
        codec_fourcc: *b"dra ",
        track_id: 1,
        default_sample_duration: 1000,
        codec_name: "PointCloudCodec_dra".to_string(),
    };

    // 1️⃣ Create INIT segment
    let init_buffer = create_init_segment(&config);
    println!("Generated INIT segment ({} bytes)", init_buffer.len());

    let init_boxes = match parse_mp4_boxes(&init_buffer) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to parse INIT segment: {}", e);
            process::exit(1);
        }
    };

    println!("Parsed {} boxes from INIT segment:\n", init_boxes.len());
    for (i, mp4_box) in init_boxes.iter().enumerate() {
        println!("Init Box {}:\n{:#?}\n", i + 1, mp4_box);
    }

    // 2️⃣ Create MEDIA segment with static frame data
    let frame_data = vec![0u8; 1024];  // Static dummy frame data
    let media_buffer = create_media_segment(&config, &frame_data, 1, 0);
    println!("Generated MEDIA segment ({} bytes)", media_buffer.len());

    let media_boxes = match parse_mp4_boxes(&media_buffer) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to parse MEDIA segment: {}", e);
            process::exit(1);
        }
    };

    println!("Parsed {} boxes from MEDIA segment:\n", media_boxes.len());
    for (i, mp4_box) in media_boxes.iter().enumerate() {
        println!("Media Box {}:\n{:#?}\n", i + 1, mp4_box);
    }

    // 3️⃣ Optionally extract mdat boxes
    match extract_mdat_boxes(&media_buffer) {
        Ok(mdat_boxes) => {
            println!("Extracted {} mdat box(es) from MEDIA segment.\n", mdat_boxes.len());
        }
        Err(e) => {
            eprintln!("Failed to extract mdat boxes: {}", e);
        }
    }
}
