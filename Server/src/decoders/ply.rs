use std::io::{Cursor, BufReader};
use ply_rs::parser::Parser;
use tracing::{warn, instrument};

use shared_utils::types::{Point3D, PointCloudData};



#[instrument(skip_all)]
pub fn decode_ply(data: Vec<u8>) -> Result<PointCloudData, Box<dyn std::error::Error>> {
    // Wrap the Vec<u8> in a Cursor, then in a BufReader to handle line reading.
    let cursor = Cursor::new(data);
    let mut reader = BufReader::new(cursor);
    
    // Create a parser for VertexWithColor
    let parser = Parser::<Point3D>::new();

    // Try reading the header
    let header = match parser.read_header(&mut reader) {
        Ok(h) => h,
        Err(_) => {
            // Return an error if the header cannot be read
            return Err("Failed to read PLY header".into());
        }
    };

    let mut pcd = PointCloudData::default();

    // Parse payload based on the element type (vertex in this case)
    for element in &header.elements {
        match element.name.as_ref() {
            "vertex" => {
                // Handle potential errors in `read_payload_for_element`
                let vertex_list = match parser.read_payload_for_element(&mut reader, element, &header) {
                    Ok(v) => v,
                    Err(_) => {
                        // If there is an error in reading the payload, increment the error count and skip to the next element
                        pcd.error_count += 1;
                        continue;
                    }
                };

                pcd.points.extend(vertex_list);
            }
            _ => {
                // Ignore other elements for now
            }
        }
    }

    Ok(pcd)
}