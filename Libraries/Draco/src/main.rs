use draco_wrapper::*;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("Running draco example");

    // Sample point cloud data
    let coords: Vec<f32> = vec![
        0.0, 0.0, 1.0,
        0.0, 1.0, 0.0,
        1.0, 0.0, 0.0,
    ];
    let colors: Vec<u8> = vec![
        255, 0, 0,
        0, 255, 0,
        0, 0, 255,
    ];

    // Encode the point cloud to Draco format
    let encoded_data = encode_draco(coords, colors)?;

    println!("Encoding successful! Encoded size: {} bytes", encoded_data.len());

    // Decode the Draco-encoded data back into point cloud
    let (decoded_coords, decoded_colors) = decode_draco(encoded_data)?;

    println!("Decoding successful! Number of points: {}", decoded_coords.len() / 3);
    println!("Decoded coordinates: {:?}", decoded_coords);
    println!("Decoded colors: {:?}", decoded_colors);

    Ok(())
}
