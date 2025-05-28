use tracing::{error, info};

#[allow(unreachable_patterns)]
#[tokio::main]
async fn main() {
    // let mpd_url = "https://livesim2.dashif.org/livesim2/scte35_2/testpic_2s/Manifest.mpd";
    // let mpd_url = "https://livesim2.dashif.org/livesim2/segtimeline_1/testpic_2s/Manifest.mpd";
    // let mpd_url = "https://akamaibroadcasteruseast.akamaized.net/cmaf/live/657078/akasource/out.mpd";
    // let mpd_url = "https://dash.akamaized.net/akamai/bbb_30fps/bbb_30fps.mpd";
    let mpd_url = "http://11.0.1.2:3001/dash/client_0_.mpd";

    let callback = |event: dash_player::DashEvent| {
        
        match event {
            dash_player::DashEvent::Segment { data, content_type, representation_id: _, segment_number, duration: _, url: _, playback_rate } => {
                info!("Received {} segment of size: {} at rate: {} and segment number: {}", content_type, data.len(), playback_rate, segment_number);
                // TODO: write to file, buffer, feed to decoder, etc.
            }
            dash_player::DashEvent::Info(msg) => {
                info!("Info: {}", msg);
            }
            dash_player::DashEvent::Warning(msg) => {
                info!("Warning: {}", msg);
            }
            dash_player::DashEvent::DownloadError { url, reason } => {
                error!("Error downloading {}: {}", url, reason);
            }
            _ => {
                info!("Unhandled event");
            }
        }
    };

    let player = dash_player::DashPlayer::new(mpd_url, std::sync::Arc::new(callback)).await.unwrap();
    player.set_target_latency(0.001).await;
    info!("Player initialized");
    player.start().await.unwrap();
    info!("Player started");

    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    
    player.stop();
/*
    // Spawn a loop to keep the main thread alive
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
    */
}
