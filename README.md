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

On linux, you can install these dependencies by running the following command:

```bash
sudo apt-get install cmake ninja-build mingw-w64 smcroute

rustup target add x86_64-pc-windows-gnu
```

In addition, you need to install the Rust toolchain. You can do this by running the following command:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Finally, you need to install Docker and Docker Compose. You can do this by following the instructions on the [Docker Compose website](https://docs.docker.com/compose/install/).

# Building the Project

To build the project, you need to run the following commands:

```bash
./build.sh
```
Parameters are defined in the build script and the scripts called by it.

The following parameters are recommend to build the project:
```bash
/build.sh --unstable --release
```
To speed up the build process during development, you can use `--no-tests`, but this will not run the unit tests and also not update the non-headless client.

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
@INPROCEEDINGS{Haems2509MutliPathXR,
    AUTHOR="Casper Haems and Matthias {De Fr{\'e}} and Tim Wauters and Filip {De Turck}",
    TITLE="Towards Efficient Transport for {Real-Time} Immersive Applications over Hybrid Networks",
    BOOKTITLE="2025 16th International Conference on Network of the Future (NoF) (NoF 2025)",
    ADDRESS="Montreal, Canada",
    PAGES=9,
    KEYWORDS="volumetric video; hybrid broadcast-unicast; multi-path transport real-time streaming; immersive media; 6DoF communication",
    ABSTRACT="Immersive telepresence applications demand significant data rates with real-time delivery targets that no single commercial data path can consistently meet. Moreover, existing adaptive strategies with fine-grained content selection remain underdeveloped. This paper introduces a hybrid, multi-path delivery framework that fuses broadcast and unicast communication into one coherent service. Lightweight volumetric video is delivered via broadcast using File Delivery over Unidirectional Transport (FLUTE), guaranteeing that every viewer maintains at least a never-blank scene. Viewer-specific enhancement content is steered over unicast channels by a scheduler that keeps all volumetric video frames within a common playout deadline. This work releases an open-source testbed that emulates network impairments, instruments the common protocols of the different stages in the pipeline, and allows reproducible experimentation. Results on a high-quality, volumetric video of up to 100k points per frame show that the hybrid design (i) is capable of keeping the transport latency below 40ms while scaling quality with available unicast bandwidth, (ii) cuts server traffic and network load significantly compared with pure-unicast delivery, and (iii) masks typical wireless loss patterns with only a 15\% Forward Error Correction (FEC) overhead on the broadcast link. These findings demonstrate that treating broadcast and unicast as complementary pipes, rather than competing alternatives, is essential for practical, large-scale Extended Reality (XR) services on emerging 5G/6G networks. All code is publicly released to accelerate further research on hybrid, multi-path delivery."
}

```