#!/bin/bash
# Runs in the build container (see build.sh): builds Oddech against the
# bundled FFmpeg, lays out the application directory and packs it as .deb
# and AppImage.
set -euo pipefail

src=/src
pkg=$src/packaging
downloads=$pkg/cache/downloads
out=$pkg/out
work=$(mktemp -d)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$src/Cargo.toml" | head -1)

# --- Dependencies ---------------------------------------------------------
mkdir -p "$work/ffmpeg" "$work/ort"
tar -xf "$downloads/ffmpeg.tar.xz" -C "$work/ffmpeg" --strip-components=1
tar -xzf "$downloads/onnxruntime.tgz" -C "$work/ort" --strip-components=1

# --- Build ----------------------------------------------------------------
# The program finds its libraries in lib/ next to itself.
FFMPEG_DIR=$work/ffmpeg RUSTFLAGS='-C link-arg=-Wl,-rpath,$ORIGIN/lib' \
    cargo build --release --locked -p oddech
bin=$CARGO_TARGET_DIR/release/oddech

# --- Application directory ------------------------------------------------
# Same layout as dist/ in the source tree: the program, lib/, models/.
app=$work/oddech
mkdir -p "$app/lib" "$app/models" "$app/licenses"
cp "$bin" "$app/oddech"
strip --strip-debug "$app/oddech"
# Only the FFmpeg libraries the program uses (and what they need).
for soname in libavcodec.so.62 libavformat.so.62 libavutil.so.60 libswresample.so.6; do
    cp -L "$work/ffmpeg/lib/$soname" "$app/lib/$soname"
    # They find each other in their own directory (the program's run path
    # covers only its direct dependencies).
    patchelf --set-rpath '$ORIGIN' "$app/lib/$soname"
done
cp -L "$work/ort/lib/libonnxruntime.so.1.22.0" "$app/lib/libonnxruntime.so"
cp -L "$work/ort/lib/libonnxruntime_providers_shared.so" "$app/lib/"
cp "$src/models/respiro-en.onnx" "$app/models/"
cp "$work/ffmpeg/LICENSE.txt" "$app/licenses/FFmpeg.txt"
cp "$work/ort/LICENSE" "$app/licenses/ONNX-Runtime.txt"
cp "$work/ort/ThirdPartyNotices.txt" "$app/licenses/ONNX-Runtime-ThirdPartyNotices.txt"
cp "$pkg/licenses/"*.txt "$app/licenses/"
cp "$src/LICENSE" "$app/licenses/Oddech.txt"
python3 "$pkg/crate_licenses.py" oddech > "$app/licenses/Rust-crates.txt"
cat > "$app/licenses/README.txt" <<EOF
Oddech $version
Copyright (c) 2026 Karol Nowicki, licencja MIT (Oddech.txt).

Dołączone składniki:
- FFmpeg $(basename "$(tar -tf "$downloads/ffmpeg.tar.xz" | head -1)") (LGPL 2.1 lub nowsza),
  niezmieniona kompilacja z https://github.com/BtbN/FFmpeg-Builds;
  źródła: https://ffmpeg.org/download.html, https://github.com/FFmpeg/FFmpeg
- ONNX Runtime 1.22.0 (MIT), https://github.com/microsoft/onnxruntime
- model Respiro-en (MIT), https://github.com/ydqmkkx/Respiro-en
- biblioteki Rusta wkompilowane w program: Rust-crates.txt
EOF

# Every library must resolve inside the application directory.
missing=$(for f in "$app/oddech" "$app"/lib/*.so*; do ldd "$f" | grep "not found" || true; done)
[ -z "$missing" ] || { echo "brakujące biblioteki:"; echo "$missing"; exit 1; } >&2

# --- .deb -----------------------------------------------------------------
deb=$work/deb
mkdir -p "$deb/DEBIAN" "$deb/opt" "$deb/usr/bin" "$deb/usr/share/applications" \
    "$deb/usr/share/icons/hicolor/scalable/apps" "$deb/usr/share/doc/oddech"
cp -a "$app" "$deb/opt/oddech"
ln -s /opt/oddech/oddech "$deb/usr/bin/oddech"
cp "$pkg/oddech.desktop" "$deb/usr/share/applications/"
cp "$pkg/oddech.svg" "$deb/usr/share/icons/hicolor/scalable/apps/"
cp "$app/licenses/README.txt" "$deb/usr/share/doc/oddech/copyright"
cat > "$deb/DEBIAN/control" <<EOF
Package: oddech
Version: $version
Architecture: amd64
Maintainer: Karol Nowicki <oddech-dev@stsai.pl>
Installed-Size: $(du -sk "$deb" | cut -f1)
Section: video
Priority: optional
Depends: libc6 (>= 2.35), libgcc-s1, libstdc++6, libasound2 | libasound2t64, libdbus-1-3,
 libx11-6, libxcursor1, libxi6, libxrandr2, libxkbcommon0, libxkbcommon-x11-0,
 libvulkan1, libegl1
Recommends: mesa-vulkan-drivers, xdg-desktop-portal | zenity, fonts-dejavu-core,
 libwayland-client0, libwayland-cursor0, libwayland-egl1
Suggests: oddech-cuda
Description: Wykrywanie i ściszanie wdechów w nagraniach wideo
 Znajduje wdechy na wybranych ścieżkach audio pliku wideo, pozwala je
 przejrzeć i eksportuje wideo, pliki WAV albo oś czasu FCPXML ze
 ściszonymi wdechami. Oryginalne nagranie nie jest zmieniane.
EOF
mkdir -p "$out"
dpkg-deb --root-owner-group -Zxz --build "$deb" "$out/oddech_${version}_amd64.deb"

# --- AppImage -------------------------------------------------------------
appdir=$work/Oddech.AppDir
cp -a "$app" "$appdir"
cp "$pkg/AppRun" "$appdir/AppRun"
cp "$pkg/oddech.desktop" "$pkg/oddech.svg" "$appdir/"
ln -s oddech.svg "$appdir/.DirIcon"
cp "$downloads/appimagetool" "$work/appimagetool"
chmod +x "$work/appimagetool" "$appdir/AppRun"
# No FUSE in the container: the tool runs from its extracted self.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 "$work/appimagetool" --no-appstream \
    --runtime-file "$downloads/runtime-x86_64" "$appdir" "$out/Oddech-${version}-x86_64.AppImage"

# --- GPU add-on (.deb) ----------------------------------------------------
# The CUDA provider of ONNX Runtime and the NVIDIA libraries it needs, in
# the application directory of the oddech package. Oddech uses the GPU
# when the provider is present (see DECISIONS.md, "CUDA i GPU").
if [ "${ODDECH_CUDA:-1}" = 1 ]; then
    cudadeb=$work/cuda-deb
    lib=$cudadeb/opt/oddech/lib
    licenses=$cudadeb/opt/oddech/licenses
    mkdir -p "$lib/cuda" "$licenses" "$cudadeb/DEBIAN" "$cudadeb/usr/share/doc/oddech-cuda"
    cp -L "$work/ort/lib/libonnxruntime_providers_cuda.so" "$lib/"
    patchelf --set-rpath '$ORIGIN/cuda' "$lib/libonnxruntime_providers_cuda.so"
    declare -A sonames=(
        [cudart]="libcudart.so.12"
        [cublas]="libcublas.so.12 libcublasLt.so.12"
        [curand]="libcurand.so.10"
        [cufft]="libcufft.so.11"
        [nvrtc]="libnvrtc.so.12 libnvrtc-builtins.so.12.9"
        [cudnn]="libcudnn.so.9 libcudnn_adv.so.9 libcudnn_cnn.so.9 libcudnn_engines_precompiled.so.9
                 libcudnn_engines_runtime_compiled.so.9 libcudnn_engines_tensor_ir.so.9
                 libcudnn_ext.so.9 libcudnn_graph.so.9 libcudnn_heuristic.so.9 libcudnn_ops.so.9"
    )
    for archive in "${!sonames[@]}"; do
        mkdir -p "$work/$archive"
        tar -xJf "$downloads/$archive.tar.xz" -C "$work/$archive" --strip-components=1 \
            --wildcards '*/lib/*.so*' '*/LICENSE'
        for soname in ${sonames[$archive]}; do
            cp -L "$work/$archive/lib/$soname" "$lib/cuda/"
        done
        cp "$work/$archive/LICENSE" "$licenses/NVIDIA-$archive.txt"
    done
    # libnvrtc loads its builtins with dlopen and has no run path of its own.
    patchelf --set-rpath '$ORIGIN' "$lib/cuda/libnvrtc.so.12"

    missing=$(for f in "$lib"/*.so "$lib"/cuda/*.so*; do ldd "$f" | grep "not found" || true; done)
    [ -z "$missing" ] || { echo "brakujące biblioteki (GPU):"; echo "$missing"; exit 1; } >&2

    cudnn_version=$(cd "$work/cudnn/lib" && ls libcudnn.so.9.*)
    cat > "$licenses/README-GPU.txt" <<EOF
Oddech $version, obsługa kart NVIDIA (paczka oddech-cuda)

Dołączone składniki, rozpowszechniane na warunkach licencji NVIDIA
(pliki NVIDIA-*.txt); korzysta z nich wyłącznie Oddech:
- ONNX Runtime 1.22.0, dostawca CUDA (MIT, ONNX-Runtime.txt)
- CUDA 12.9: cudart, cuBLAS, cuRAND, cuFFT, NVRTC,
  https://developer.download.nvidia.com/compute/cuda/redist/
- cuDNN ${cudnn_version#libcudnn.so.},
  https://developer.download.nvidia.com/compute/cudnn/redist/
EOF
    cp "$licenses/README-GPU.txt" "$cudadeb/usr/share/doc/oddech-cuda/copyright"
    cat > "$cudadeb/DEBIAN/control" <<EOF
Package: oddech-cuda
Version: $version
Architecture: amd64
Maintainer: Karol Nowicki <oddech-dev@stsai.pl>
Installed-Size: $(du -sk "$cudadeb" | cut -f1)
Section: video
Priority: optional
Depends: oddech (= $version)
Description: Wykrywanie wdechów w Oddechu na karcie NVIDIA
 Biblioteki CUDA 12 i cuDNN 9 oraz dostawca CUDA dla ONNX Runtime.
 Wymaga karty NVIDIA i sterownika w wersji 525 lub nowszej. Bez nich
 (albo gdy test karty się nie powiedzie) Oddech wykrywa na procesorze.
EOF
    dpkg-deb --root-owner-group -Zxz --build "$cudadeb" "$out/oddech-cuda_${version}_amd64.deb"
fi

rm -rf "$work"
ls -la "$out"
