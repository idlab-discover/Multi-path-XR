#ifndef DRACO_WRAPPER_CPP_H
#define DRACO_WRAPPER_CPP_H

#include <draco/compression/encode.h>
#include <draco/compression/decode.h>
#include <draco/core/encoder_buffer.h>
#include <draco/core/decoder_buffer.h>
#include <draco/point_cloud/point_cloud.h>
#include <draco/point_cloud/point_cloud_builder.h>
#include <draco/attributes/geometry_attribute.h>
#include <draco/attributes/geometry_indices.h>
#include <stdint.h>
#include <stddef.h>
#include <stdexcept>
#include <vector>
#include <cstring>
#include <utility>

#ifdef __cplusplus
extern "C" {
#endif


namespace draco_wrapper {

struct EncodeResult {
    bool success;              // Indicates if encoding was successful
    size_t size;               // Size of the encoded data
    const uint8_t* data;       // Encoded data
    char* error_msg;           // Error message if encoding fails
};

struct DecodeResult {
    bool success;              // Indicates if decoding was successful
    size_t num_points;         // Number of points in the decoded data
    float* coords;            // Decoded coordinates
    uint8_t* colors;           // Decoded colors
    char* error_msg;           // Error message if decoding fails
};

class DracoWrapper {
public:
    // Function to encode points to Draco
    // `coords` is an array of `num_points` * 3 floats, representing X, Y, Z for each point
    // `colors` is an array of `num_points` * 3 uint8_t, representing R, G, B for each point
    static EncodeResult* encode_points_to_draco(const float* coords, size_t num_points, const uint8_t* colors);

    // Function to decode Draco data into points and colors
    // `encoded_data` is a pointer to the encoded buffer, and `encoded_size` is the buffer length
    static DecodeResult* decode_draco_data(const uint8_t* encoded_data, size_t encoded_size);

    // Function to free the encoded result
    static void free_encode_result(EncodeResult* result);

    // Function to free the decoded result
    static void free_decode_result(DecodeResult* result);
};

} // namespace draco_wrapper

#ifdef __cplusplus
}
#endif

#endif // DRACO_WRAPPER_CPP_H