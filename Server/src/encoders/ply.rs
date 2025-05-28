use ply_rs::{ply::{DefaultElement, ElementDef, Encoding, Ply, Property, PropertyDef, PropertyType, ScalarType}, writer::Writer};
use tracing::instrument;

use shared_utils::types::PointCloudData;
use std::error::Error;

#[instrument(skip_all)]
pub fn encode_ply(point_cloud: PointCloudData) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut buf = Vec::<u8>::new();

    // Create a ply object
    let mut ply = Ply::<DefaultElement>::new();
    ply.header.encoding = Encoding::Ascii;

    // Define the elements we want to write. In our case we write a 2D Point.
    // When writing, the `count` will be set automatically to the correct value by calling `make_consistent`
    let mut point_element = ElementDef::new("point");
    let p = PropertyDef::new("x", PropertyType::Scalar(ScalarType::Float));
    point_element.properties.push(p);
    let p = PropertyDef::new("y", PropertyType::Scalar(ScalarType::Float));
    point_element.properties.push(p);
    let p = PropertyDef::new("z", PropertyType::Scalar(ScalarType::Float));
    point_element.properties.push(p);
    let p = PropertyDef::new("red", PropertyType::Scalar(ScalarType::UChar));
    point_element.properties.push(p);
    let p = PropertyDef::new("green", PropertyType::Scalar(ScalarType::UChar));
    point_element.properties.push(p);
    let p = PropertyDef::new("blue", PropertyType::Scalar(ScalarType::UChar));
    point_element.properties.push(p);
    ply.header.elements.push(point_element);

    let mut points = Vec::with_capacity(point_cloud.points.len());

    // Fill the points with DefaultElements
    for point in &point_cloud.points {
        let mut point_element = DefaultElement::new();
        point_element.insert("x".to_string(), Property::Float(point.x));
        point_element.insert("y".to_string(), Property::Float(point.y));
        point_element.insert("z".to_string(), Property::Float(point.z));
        point_element.insert("red".to_string(), Property::UChar(point.r));
        point_element.insert("green".to_string(), Property::UChar(point.g));
        point_element.insert("blue".to_string(), Property::UChar(point.b));
        points.push(point_element);
    }

    ply.payload.insert("point".to_string(), points);

    ply.make_consistent().unwrap();

    // set up a writer
    let w = Writer::new();
    let _written = w.write_ply(&mut buf, &mut ply).unwrap();

    // Implement PLY encoding logic
    // For demonstration, return an empty vector
    Ok(buf)
}
