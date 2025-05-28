#include "wrapper.h"

#ifdef __cplusplus
extern "C" {
#endif


namespace draco_wrapper {

// Function to encode points to Draco, returning a buffer of encoded data
EncodeResult* DracoWrapper::encode_points_to_draco(const float* coords, size_t num_points, const uint8_t* colors) {
    EncodeResult* result = new EncodeResult();
    result->success = false;
    result->data = nullptr;
    result->size = 0;
    result->error_msg = nullptr;

    // Error handling: Check if pointers are null
    if (!coords || !colors) {
        result->error_msg = strdup("Invalid input: coords/colors pointers are null.");
        return result;
    }


    try {
        // Initialize point cloud
        draco::PointCloud point_cloud;
        point_cloud.set_num_points(static_cast<uint32_t>(num_points));

        // Create and add the position attribute
        // Create a position attribute using a unique pointer
        std::unique_ptr<draco::PointAttribute> position_attribute = std::make_unique<draco::PointAttribute>();
        std::unique_ptr<draco::PointAttribute> color_attribute = std::make_unique<draco::PointAttribute>();
        
        position_attribute->Init(draco::GeometryAttribute::POSITION, 3, draco::DataType::DT_FLOAT32, false, point_cloud.num_points());
        color_attribute->Init(draco::GeometryAttribute::COLOR, 3, draco::DataType::DT_UINT8, true, point_cloud.num_points());
        float position_value[3];
        uint8_t color_value[3];
        for(auto i = 0; i < point_cloud.num_points(); i++) {
            position_value[0] = static_cast<float>(coords[i * 3]);
            position_value[1] = static_cast<float>(coords[i * 3 + 1]);
            position_value[2] = static_cast<float>(coords[i * 3 + 2]);
            color_value[0] = colors[i * 3];
            color_value[1] = colors[i * 3 + 1];
            color_value[2] = colors[i * 3 + 2];
            position_attribute->SetAttributeValue(draco::AttributeValueIndex(i), position_value);
            //position_attribute->buffer()->Update(position_value, 3 * sizeof(float), i);
            color_attribute->SetAttributeValue(draco::AttributeValueIndex(i), color_value);
            //color_attribute->buffer()->Update(colors, 3 * sizeof(uint8_t), i);
        }

        auto position_attribute_id = point_cloud.AddAttribute(std::move(position_attribute));
        auto color_attribute_id = point_cloud.AddAttribute(std::move(color_attribute));

        // Initialize encoder and buffer
        draco::Encoder encoder;
        draco::EncoderBuffer encoder_buffer;

        // We will use the KD-tree encoding method
        encoder.SetEncodingMethod(draco::POINT_CLOUD_KD_TREE_ENCODING);    
        // We can potentially change quantization here    
        encoder.SetAttributeQuantization(draco::GeometryAttribute::POSITION, 11);  

        // Encode the point cloud into the buffer
        draco::Status status = encoder.EncodePointCloudToBuffer(point_cloud, &encoder_buffer);
        if (!status.ok()) {
            throw std::runtime_error("Failed to encode point cloud: " + std::string(status.error_msg()));
        }

        // Allocate memory for the encoded data and copy it
        uint8_t* encoded_data = new uint8_t[encoder_buffer.size()];
        memcpy(encoded_data, encoder_buffer.data(), encoder_buffer.size());

        result->success = true;
        result->data = encoded_data;
        result->size = encoder_buffer.size();
    } catch (const std::exception& e) {
        result->error_msg = strdup(e.what());
    } catch (...) {
        result->error_msg = strdup("Unknown error occurred during encoding.");
    }

    return result;
}

// Function to decode Draco data into points and colors
DecodeResult* DracoWrapper::decode_draco_data(const uint8_t* encoded_data, size_t encoded_size) {
    DecodeResult* result = new DecodeResult();
    result->success = false;
    result->coords = nullptr;
    result->colors = nullptr;
    result->num_points = 0;
    result->error_msg = nullptr;

    try {
        draco::PointCloud point_cloud;
        draco::DecoderBuffer decoder_buffer;
        decoder_buffer.Init(reinterpret_cast<const char*>(encoded_data), encoded_size);

        draco::Decoder decoder;

        // Decode the point cloud
        draco::Status status = decoder.DecodeBufferToGeometry(&decoder_buffer, &point_cloud);
        if (!status.ok()) {
            throw std::runtime_error("Failed to decode point cloud: " + std::string(status.error_msg()));
        }

        // Prepare arrays for decoded points and colors
        size_t num_points = point_cloud.num_points();
        std::vector<float> coords;
        std::vector<uint8_t> colors;
        result->num_points = num_points;

        int pos_att_id = point_cloud.GetNamedAttributeId(draco::GeometryAttribute::POSITION);
        if (pos_att_id >= 0) {
            const draco::PointAttribute* pos_att = point_cloud.GetAttributeByUniqueId(pos_att_id);
            coords.resize(point_cloud.num_points() * 3);

            for (draco::PointIndex i(0); i < point_cloud.num_points(); ++i) {
                pos_att->GetValue(draco::AttributeValueIndex(i.value()), &coords[i.value() * 3]);
            }
        } else {
            throw std::runtime_error("Position attribute not found");
        }

        int color_att_id = point_cloud.GetNamedAttributeId(draco::GeometryAttribute::COLOR);
        if (color_att_id >= 0) {
            const draco::PointAttribute* color_att = point_cloud.GetAttributeByUniqueId(color_att_id);
            colors.resize(point_cloud.num_points() * 3);

            for (draco::PointIndex i(0); i < point_cloud.num_points(); ++i) {
                color_att->GetValue(draco::AttributeValueIndex(i.value()), &colors[i.value() * 3]);
            }
        } else {
            std::cerr << "Error: Color attribute not found." << std::endl;
            throw std::runtime_error("Color attribute not found");
        }

        // Copy the decoded data to the result
        result->coords = new float[num_points * 3];
        result->colors = new uint8_t[num_points * 3];
        for (size_t i = 0; i < num_points * 3; i++) {
            result->coords[i] = coords[i];
            result->colors[i] = colors[i];
        }

        result->success = true;
    } catch (const std::exception& e) {
        std::cerr << "Error: " << e.what() << std::endl;
        result->error_msg = strdup(e.what());
    } catch (...) {
        std::cerr << "Unknown error occurred during decoding." << std::endl;
        result->error_msg = strdup("Unknown error occurred during decoding.");
    }

    return result;
}

// Function to free the memory allocated for the encoded result
void DracoWrapper::free_encode_result(EncodeResult* result) {
    if (result) {
        if (result->data) {
            delete[] result->data;
        }
        if (result->error_msg) {
            free(result->error_msg); // Free instead of delete[] because strdup uses malloc
        }
        delete result;
    }
}

void DracoWrapper::free_decode_result(DecodeResult* result) {
    if (result) {
        if (result->coords) {
            delete[] result->coords;
        }
        if (result->colors) {
            delete[] result->colors;
        }
        if (result->error_msg) {
            free(result->error_msg); // Free instead of delete[] because strdup uses malloc
        }
        delete result;
    } else {
        std::cerr << "Error: Attempted to free a null DecodeResult." << std::endl;
    }
}

}

#ifdef __cplusplus
}
#endif
