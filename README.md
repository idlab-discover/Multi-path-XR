# Hybrid Unicast-Broadcast for XR
A multi-path solution for transmitting data between devices using multiple protocols, with a focus on real-time point cloud video transmission.

## Introduction

This project provides a simple way to transmit data between devices using numerous protocols. The project is designed to be simple to use and easy to understand. The main focus is on transmitting point cloud data in real-time between devices, but the project can be used to transmit any data.

# Supported Protocols

- DASH
- Websockets
- WebRTC
- FLUTE

# Getting Started

To start using this project, you first need to clone the repository. You can do this by running the following command:

```bash
git clone <repository-url.git>
```

After cloning the repository, the submodules need to cloned as well. You can do this by running the following command:

```bash
git submodule update --init --recursive
```

Now, proceed by making the scripts executable, using this recursive command:

```bash
chmod -R +x *.sh
```

Now, the next steps are to install the dependencies and build the project.

# Dependencies

The project has the following dependencies, which need to be installed:

- CMake
- Ninja
- MinGW (Used for cross-compiling to Windows)
- smcroute
- Python 3
- libclang (used for generating bindings)
- libfontconfig (used for rendering the network graph visualizations in the controller)
- g++
- build-essential
- The correction dev version of libstdc++

On linux, you can install these dependencies by running the following commands:

```bash
sudo apt update
sudo apt install cmake ninja-build mingw-w64 smcroute libssl-dev python3 libclang-dev libfontconfig1-dev build-essential g++
GCC_TOOLCHAIN=$(clang++ -v 2>&1 | grep "Selected GCC installation" | awk '{print $4}')
GCC_VERSION=$(basename "$GCC_TOOLCHAIN")
sudo apt install -y "libstdc++-${GCC_VERSION}-dev"
```


```
In addition, you need to install the Rust toolchain. You can do this by running the following command:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add x86_64-pc-windows-gnu
```

Next, you need to install Docker and Docker Compose. You can do this by following the instructions on the [Docker Compose website](https://docs.docker.com/compose/install/).

Finally, to support the Slices CLI, you need to follow the steps in the [Slices README](./Environments/VirtualWall/README.md).

# Building the Project

To build the project, you need to run the following commands:

```bash
./build.sh
```
Parameters are defined in the build script and the scripts called by it.

The following parameters are recommended to build the project:
```bash
./build.sh --unstable --release
```
To speed up the build process during development, you can use `--no-bindings`, but this will not create the bindings for the headfull client.
When building for Windows, you can use `--windows` to cross-compile the project for Windows.
If building for execution on the local machine, you can enable `--native-opt` to optimize the build for the local machine architecture.

# Running the Project

To run the project, you need to run the following commands:

```bash
./run.sh
```
Parameters are defined in the run script and the scripts called by it.

The first parameter is the component to run, which can be one of the following:
- `--client`: Runs the client component.
- `--server`: Runs the server component.
- `--metrics`: Runs the metrics component.
- `--monitoring`: Runs the monitoring component.
- `--controller`: Runs the controller component, used to manage nodes, experiments and data. `sudo` is required if you want to use Mininet.
- `--agent`: Runs agent component, used to connect a node to the controller.
- `--update-targets`: Used to update the monitoring targets.

The following command runs the controller component in release mode.
```bash
sudo ./run.sh --controller --release
```
This is the recommended way to test the project. The controller can now be managed using the web interface at `http://localhost:3000/?release=true`.

## Contact

If you have any questions or concerns, please feel free to contact us at [casper.haems@ugent.be](mailto:casper.haems@ugent.be) or [tim.wauters@ugent.be](mailto:jeroen.vanderhooft@ugent.be).

# References

If you use (parts of) this code, please cite the following paper:
```bibtex
@INPROCEEDINGS{11223289,
  author={Haems, Casper and De Fré, Matthias and Wauters, Tim and De Turck, Filip},
  booktitle={2025 16th International Conference on Network of the Future (NoF)}, 
  title={Towards Efficient Transport for Real-Time Immersive Applications over Hybrid Networks}, 
  year={2025},
  volume={},
  number={},
  pages={209-213},
  keywords={Measurement;Wireless communication;Telepresence;Unicast;Bandwidth;Forward error correction;Throughput;User experience;WebRTC;Videos;Volumetric video;hybrid broadcast-unicast;multi-path transport;real-time streaming;immersive media},
  doi={10.1109/NoF66640.2025.11223289}}

```

# Funding
Work up to and including commit [`4631e27`](https://github.com/idlab-discover/Multi-path-XR/commit/4631e27dab1f122940af16ab326133e0055bbd89)
was funded by the European Union's [SPIRIT project](https://www.spiritproject.eu/)
(Grant Agreement 101070672) and belongs to the work doi: [10.1109/NoF66640.2025.11223289](https://doi.org/10.1109/NoF66640.2025.11223289).

Work introduced after that commit has been funded by the imec.icon project
MAGNOLIA, co-financed by imec and Flanders Innovation & Entrepreneurship
(VLAIO) under project HBC.2025.0436.

# License
This project is licensed under the MIT License.
See the [LICENSE](LICENSE) file for details.
