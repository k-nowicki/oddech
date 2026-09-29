# Pinned inputs of the package build. Every download is checked against its
# SHA-256; change a URL only together with its hash.

RUST_VERSION=1.98.1

# The GitHub repository; its release "build-deps" keeps copies of
# downloads that disappear upstream (the release workflow uploads them).
GITHUB_REPO=k-nowicki/oddech
MIRROR=https://github.com/$GITHUB_REPO/releases/download/build-deps

# FFmpeg: unmodified upstream build (LGPL, shared libraries) from
# github.com/BtbN/FFmpeg-Builds. Autobuild tags are removed after some
# weeks, then the copy in build-deps is used. To update, take the newest
# n8.1 lgpl-shared autobuild (ffmpeg-next 9 supports FFmpeg up to 8.1).
FFMPEG_URL=https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-29-13-10/ffmpeg-n8.1.3-6-gff48edd8b2-linux64-lgpl-shared-8.1.tar.xz
FFMPEG_MIRROR=$MIRROR/${FFMPEG_URL##*/}
FFMPEG_SHA256=10058038d7e05869a32bb7d8594dd1a0b4032b8a10f1387dbe7e5ce4f978b8c0

# ONNX Runtime 1.22 (see DECISIONS.md, "CUDA i GPU"). The GPU build: its
# core library runs on the CPU as well and can load the CUDA provider of
# the GPU add-on.
ORT_URL=https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-linux-x64-gpu-1.22.0.tgz
ORT_SHA256=2a19dbfa403672ec27378c3d40a68f793ac7a6327712cd0e8240a86be2b10c55

APPIMAGETOOL_URL=https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage
APPIMAGETOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
APPIMAGE_RUNTIME_URL=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
APPIMAGE_RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

# GPU add-on (oddech-cuda): NVIDIA's redistributable CUDA 12 and cuDNN 9
# libraries for the CUDA provider of ONNX Runtime 1.22. Hashes from the
# redistrib_<version>.json manifests next to the archives.
CUDA_REDIST=https://developer.download.nvidia.com/compute/cuda/redist
CUDART_URL=$CUDA_REDIST/cuda_cudart/linux-x86_64/cuda_cudart-linux-x86_64-12.9.79-archive.tar.xz
CUDART_SHA256=1f6ad42d4f530b24bfa35894ccf6b7209d2354f59101fd62ec4a6192a184ce99
CUBLAS_URL=$CUDA_REDIST/libcublas/linux-x86_64/libcublas-linux-x86_64-12.9.1.4-archive.tar.xz
CUBLAS_SHA256=546addc4a9d82b8f23aa9ba9274b6bc0429a63008a31c759884ac24880796057
CURAND_URL=$CUDA_REDIST/libcurand/linux-x86_64/libcurand-linux-x86_64-10.3.10.19-archive.tar.xz
CURAND_SHA256=48281b4caadb1cf790d44ac76b23c77d06f474c0b1799814f314aafec9258ad6
CUFFT_URL=$CUDA_REDIST/libcufft/linux-x86_64/libcufft-linux-x86_64-11.4.1.4-archive.tar.xz
CUFFT_SHA256=b0e65af59b0c2f6c8ed9f5552a9b375890855b7926ae2c0404d15dcf2565bda4
NVRTC_URL=$CUDA_REDIST/cuda_nvrtc/linux-x86_64/cuda_nvrtc-linux-x86_64-12.9.86-archive.tar.xz
NVRTC_SHA256=82913658363892dbc0f2638b070476234476e06e084fed60db861cb7e161a6af
CUDNN_URL=https://developer.download.nvidia.com/compute/cudnn/redist/cudnn/linux-x86_64/cudnn-linux-x86_64-9.25.0.15_cuda12-archive.tar.xz
CUDNN_SHA256=0fe244321b401758cbb8430d1997f494add139405a4a4091338bcbb6f5024fa8
