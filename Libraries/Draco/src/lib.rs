mod bindings;
pub use bindings::*;
use tracing::error;
use std::error::Error;
use std::ffi::CStr;
use std::slice;

/// Encodes a point cloud (coords and colors) to Draco format using the DracoWrapper.
/// Returns the encoded data as a `Vec<u8>`, or an error if the encoding fails.
pub fn encode_draco(coords: Vec<f32>, colors: Vec<u8>) -> Result<Vec<u8>, Box<dyn Error>> {
    // There should be at least one point
    /*if coords.is_empty() {
        return Err("No points to encode".into());
    }*/

    // Verify that the number of coordinates is a multiple of 3
    if coords.len() % 3 != 0 {
        return Err("Number of coordinates must be a multiple of 3".into());
    }

    let num_points = coords.len() / 3;

    // Verify that the number of colors matches the number of points
    if colors.len() != num_points * 3 {
        return Err("Number of colors must match the number of points".into());
    }
    
    unsafe {
        // Call the encode function from the DracoWrapper
        let result_ptr = DracoWrapper_encode_points_to_draco(coords.as_ptr(), num_points, colors.as_ptr());

        // Check if result_ptr is null
        if result_ptr.is_null() {
            return Err("Failed to encode points: result pointer is null".into());
        }

        // Dereference the pointer to get the result
        let result = &*result_ptr;

        if !result.success {
            // Handle error and free memory
            let c_str = CStr::from_ptr(result.error_msg);
            let error_msg = c_str.to_string_lossy().into_owned();
            error!("Failed to encode points: {}", error_msg);
            DracoWrapper_free_encode_result(result_ptr);
            return Err(error_msg.into());
        }

        // Copy the encoded data into a Vec<u8>
        let encoded_data = slice::from_raw_parts(result.data, result.size).to_vec();

        // Free the memory allocated for the result
        DracoWrapper_free_encode_result(result_ptr);

        Ok(encoded_data)
    }
}

/// Decodes Draco-encoded data back into point cloud coordinates and colors.
/// Returns the coordinates and colors as two separate `Vec`s, or an error if decoding fails.
pub fn decode_draco(encoded_data: Vec<u8>) -> Result<(Vec<f32>, Vec<u8>), Box<dyn Error>> {
    unsafe {
        // Call the decode function from the DracoWrapper
        let decoded_result_ptr = DracoWrapper_decode_draco_data(encoded_data.as_ptr(), encoded_data.len());

        if decoded_result_ptr.is_null() {
            return Err("Failed to decode the point cloud: result pointer is null".into());
        }

        let decoded_result = &*decoded_result_ptr;

        // Check if the decoding was successful and if the data is valid
        if !decoded_result.success || decoded_result.coords.is_null() || decoded_result.colors.is_null() {
            let error_msg = if !decoded_result.error_msg.is_null() {
                CStr::from_ptr(decoded_result.error_msg).to_str().unwrap_or("Unknown error")
            } else {
                "Unknown error"
            };
            DracoWrapper_free_decode_result(decoded_result_ptr);
            return Err(error_msg.into());
        }

        // Convert the decoded coordinates and colors into Rust Vecs
        let coords_vec = slice::from_raw_parts(decoded_result.coords, decoded_result.num_points * 3).to_vec();
        let colors_vec = slice::from_raw_parts(decoded_result.colors, decoded_result.num_points * 3).to_vec();

        // Free the memory allocated for the decoded result
        DracoWrapper_free_decode_result(decoded_result_ptr);

        Ok((coords_vec, colors_vec))
    }
}
