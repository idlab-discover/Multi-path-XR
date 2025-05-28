use std::io::{Cursor, BufReader};
use ply_rs::parser::Parser;

use shared_utils::types::Point3D;

use super::DecodeResult;

pub fn decode_ply_from_bytes(data: Vec<u8>) -> DecodeResult {
    let cursor = Cursor::new(data);
    let mut reader = BufReader::new(cursor);
    let parser = Parser::<Point3D>::new();
    let header = parser.read_header(&mut reader)?;

    let vertex_count: usize = header.elements.iter().filter(|e| e.name == "vertex").map(|e| e.count).sum();

    let mut vertices = Vec::with_capacity(vertex_count * 3_usize);
    let mut colors = Vec::with_capacity(vertex_count * 3);
    let error_count = 0;

    for element in &header.elements {
        if element.name == "vertex" {
            let vertex_list = parser.read_payload_for_element(&mut reader, element, &header)?;
            for vertex in vertex_list {
                vertices.push(vertex.x);
                vertices.push(vertex.y);
                vertices.push(vertex.z);
                colors.push(vertex.r);
                colors.push(vertex.g);
                colors.push(vertex.b);
            }
        }
    }
    Ok((error_count, vertices, colors))
}
