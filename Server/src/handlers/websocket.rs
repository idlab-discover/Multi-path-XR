// handlers/websocket.rs

use axum::{extract::{Query, State}, Json};
use serde_json::Value;
use tracing::{debug, error, info, instrument};
use std::sync::Arc;
use crate::{services, types::{AppState, WebRtcOffer, WebRtcIceCandidate}};
use socketioxide::{extract::{Data, SocketRef}, layer::SocketIoLayer, socket::DisconnectReason, SendError, SocketError, SocketIo};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Debug)]
pub struct SimpleSocket {
    pub id: String,
    pub connected: bool,
}

#[derive(Serialize, Debug)]
pub struct SimpleSocketsResponse {
    pub sockets: Vec<SimpleSocket>,
}

#[derive(Deserialize, Debug)]
pub struct CleanSocketsRequest {
    pub sockets: Vec<String>,
}

#[instrument(skip_all)]
pub async fn list_sockets(
    State(app_state): State<AppState>,
) -> Json<SimpleSocketsResponse> {
    let sockets = app_state.socket_io.sockets().unwrap_or_default();
    let mut simple_sockets = Vec::<SimpleSocket>::new();
    for socket in sockets {
        simple_sockets.push(SimpleSocket {
            id: socket.id.to_string(),
            connected: socket.connected(),
        });
    }
    Json(SimpleSocketsResponse {
        sockets: simple_sockets,
    })
}

// Clean up all the closed sockets + the ones in the list
#[instrument(skip_all)]
pub async fn clean_sockets(
    Query(query): Query<CleanSocketsRequest>,
    State(app_state): State<AppState>,
) -> Json<SimpleSocketsResponse> {
    let all_sockets = app_state.socket_io.sockets().unwrap_or_default();
    let mut cleaned_sockets = Vec::<SimpleSocket>::new();
    let socket_to_clean = query.sockets;
    for socket in all_sockets {
        if !socket.connected() || socket_to_clean.contains(&socket.id.to_string()) {
            socket.clone().disconnect().ok();
            cleaned_sockets.push(SimpleSocket {
                id: socket.id.to_string(),
                connected: socket.connected(),
            });
        }
    }

    Json(SimpleSocketsResponse {
        sockets: cleaned_sockets,
    })
}

#[instrument(skip_all)]
pub fn create_websocket_router_layer(stream_manager: Arc<services::stream_manager::StreamManager>) -> (SocketIoLayer, SocketIo) {
    let (layer, io) = SocketIo::new_layer();

    // Track connections and disconnections in "/" the namespace
    let io_clone = io.clone();
    //let io_clone2 = io.clone();
    io_clone.ns("/", move |socket: SocketRef| async move {
        let socket_id = socket.id.to_string();
        debug!("Setting up websocket connection with id {:#?}", socket_id);

        let stream_manager_clone = Arc::clone(&stream_manager);
        //let socket_id_clone = socket_id.clone();
        socket.on_disconnect(move |socket: SocketRef, reason: DisconnectReason| async move {
            let stream_manager = Arc::clone(&stream_manager_clone);
            info!("Socket {} on ns {} disconnected, reason: {:?}", socket.id, socket.ns(), reason);
            socket.leave("broadcast").unwrap();

            // Clean up our data channels
            {
                let webrtc_egress = stream_manager.get_webrtc_egress();
                let webrtc_ingress = stream_manager.get_webrtc_ingress();
                if webrtc_egress.is_some() && webrtc_ingress.is_some() {
                    let client_id = socket.id.to_string();
                    webrtc_egress.unwrap().close_peer_connection(&client_id.clone());
                    webrtc_ingress.unwrap().remove_data_channel(&client_id);
                }
            }

        });

        // Setup websocket ingress
        let socket_id_clone = socket_id.clone();
        let stream_manager_clone = Arc::clone(&stream_manager);
        let stream_id = format!("ws_{}", socket_id_clone);
        if let Some(ws_ingress) = stream_manager_clone.get_websocket_ingress() {
            ws_ingress.add_socket(stream_id.clone(), socket.clone().into());
        };

        // Setup websocket egress
        let _ = socket.join("broadcast"); // Join the broadcast room

        // 1) Client -> Server: "webrtc_offer"
        //    Contains { sdp, client_id }
        let stream_manager_clone = stream_manager.clone();
        socket.on("webrtc_offer", {
            let stream_manager_clone = stream_manager_clone.clone();
            move |s: SocketRef, Data::<WebRtcOffer>(offer)| {
                let socket_id = s.id.to_string();
                let stream_manager_clone = stream_manager_clone.clone();
                async move {
                    if let Some(webrtc_egress) = stream_manager_clone.get_webrtc_egress() {
                        // use that egress's tokio runtime for the offer handling
                        let webrtc_egress = webrtc_egress.clone();
                        let s_clone = s.clone();
                        // The actual handling call:
                        let rt = webrtc_egress.get_runtime();
                        // spawn on egress runtime
                        let _g = rt.enter();
                        rt.spawn(async move {
                            match webrtc_egress
                                .handle_client_offer(socket_id.clone(), offer, s_clone)
                                .await 
                            {
                                Ok(_) => {
                                    debug!("handle_client_offer => success");
                                }
                                Err(e) => error!("Error in handle_client_offer: {:?}", e),
                            }
                        });
                    }
                }
            }
        });

        // 2) ICE candidates from the client
        socket.on("webrtc_ice_candidate", {
            move |s: SocketRef, Data(candidate): Data<WebRtcIceCandidate>| {
                let socket_id = s.id.to_string();
                async move {
                    if let Some(webrtc_egress) = stream_manager_clone.get_webrtc_egress() {
                        // We specifically want WebRTC to run on it's own runtime
                        let rt = webrtc_egress.get_runtime();
                        let _g = rt.enter();
                        rt.spawn(async move {
                            let _ = webrtc_egress.handle_client_ice_candidate(socket_id, candidate).await;
                        });
                    }
                }
            }
        });

        // There are two issues with the Rust socket.io libraries for the server and the client:
        // 1. The server library (socketioxide) -for some reason- occasionaly closes the first socket connection some short time after the client connects. It is not clear why this happens. Luckily, the client library (rust-socketio) is able to reconnect automatically. However, the server leaves the closed socket in the active list and sometimes does not detect the closed connection.
        // 2. The client library (rust-socketio) does not provide any ability to get the socket id of the client.
        // The code below is a workaround to get the socket id of the client by sending an event to the client 2 seconds after the client connects.
        // Additionally, the first connection, which is closed within those first two seconds, will be detected and removed from the active list.
        //let stream_manager_clone = Arc::clone(&stream_manager);
        tokio::spawn(async move {
            let socket_id = socket.id.to_string();
            //let stream_manager = stream_manager_clone;
            // Wait for a few seconds before sending the event
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            // Handle the `Result` returned by `emit_with_ack`
            match socket.emit_with_ack::<String, Value>("has_connected", &socket_id) {
                Ok(ack_stream) => {
                    // Now handle the asynchronous `AckStream`
                    match ack_stream.await {
                        Ok(_) => {
                            info!("Websocket connected with id: {:#?}", socket_id);
                            /*
                            // Add client to WebSocketEgress singleton
                            if let Some(websocket_egress) = stream_manager.get_websocket_egress().await {
                                websocket_egress.add_client(socket_id.clone(), io_clone2.clone().into()).await;
                            }
                            */

                            if let Some(buffer_egress) = stream_manager.get_buffer_egress() {
                                let groups = buffer_egress.get_groups();
                                for group in groups {
                                    let group_id = group.clone();
                                    let _ = socket.emit("mpd::group_id", &group_id);
                                }
                            }
                        },
                        Err(err) => {
                            error!("Ack error from socket {}: {:?}", socket_id, err);
                        },
                    }
                }
                Err(SendError::Socket(socket_error)) => {
                    match socket_error {
                        // Handle the case where the socket is closed
                        SocketError::Closed => {
                            error!("Socket {} is closed. Removing it from active list.", socket_id);
                            // Disconnect the socket and perform any additional cleanup if needed
                            if let Err(err) = socket.disconnect() {
                                error!("Failed to disconnect socket {}: {:?}", socket_id, err);
                            }
                        }
                        _ => {
                            // Handle other socket errors
                            error!("Failed to send 'has_connected' event for socket {}: {:?}", socket_id, socket_error);
                        }
                    }
                }
                Err(SendError::Serialize(err)) => {
                    // Handle serialization errors
                    error!("Failed to serialize 'has_connected' event for socket {}: {:?}", socket_id, err);
                }
            }
        });
    });

    let io_clone = io.clone();
    (layer, io_clone)
}