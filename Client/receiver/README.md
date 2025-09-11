
Useful command for debugging:
```bash
cd ./Client/receiver
cargo run    -p pc-receiver --example debug_main --release --      --server-url http://11.0.1.2:3001      --multicast-url udp://239.0.0.1:40085      --log-level 2
```
This command uses a debug program that is used to test the receiver through the FFI interface.
When running, it prints log messages to the console and displays some basic stats about the received data per stream.
It is useful to test the receiver without having to run the full non-headless client.
By pressing enter, the program will shut down gracefully, stopping all active streams and cleaning up resources.