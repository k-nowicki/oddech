#!/bin/bash
# Builds the Oddech packages (.deb and AppImage, and the GPU add-on .deb
# unless --no-cuda) in an Ubuntu 22.04 container. Output: packaging/out/.
# See README.md, "Paczki instalacyjne".
set -euo pipefail

cuda=1
case "${1:-}" in
    "") ;;
    --no-cuda) cuda=0 ;;
    *) echo "użycie: $0 [--no-cuda]" >&2; exit 1 ;;
esac

here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")
cache=$here/cache
source "$here/versions.sh"

engine=${CONTAINER_ENGINE:-$(command -v podman || command -v docker || true)}
[ -n "$engine" ] || { echo "potrzebny podman albo docker" >&2; exit 1; }

# Downloads, checked against the pinned hashes.
mkdir -p "$cache/downloads" "$cache/cargo-registry" "$cache/target" "$here/out"
fetch() { # "url [mirror…]" sha256 file
    local file=$cache/downloads/$3 url
    echo "$2  $file" | sha256sum -c --quiet 2>/dev/null && return
    for url in $1; do
        echo "pobieranie $url"
        curl -fSL --retry 3 -o "$file.part" "$url" || continue
        mv "$file.part" "$file"
        echo "$2  $file" | sha256sum -c --quiet && return
        echo "zła suma kontrolna: $url" >&2
    done
    echo "nie udało się pobrać $3" >&2
    exit 1
}
fetch "$FFMPEG_URL $FFMPEG_MIRROR" "$FFMPEG_SHA256" ffmpeg.tar.xz
fetch "$ORT_URL" "$ORT_SHA256" onnxruntime.tgz
fetch "$APPIMAGETOOL_URL" "$APPIMAGETOOL_SHA256" appimagetool
fetch "$APPIMAGE_RUNTIME_URL" "$APPIMAGE_RUNTIME_SHA256" runtime-x86_64
if ((cuda)); then
    for lib in cudart cublas curand cufft nvrtc cudnn; do
        url=${lib^^}_URL sha=${lib^^}_SHA256
        fetch "${!url}" "${!sha}" "$lib.tar.xz"
    done
fi

image=oddech-build:22.04-rust$RUST_VERSION
"$engine" build -t "$image" --build-arg RUST_VERSION="$RUST_VERSION" -f "$here/Containerfile" "$here"

# Docker runs as root unless told otherwise; keep the output owned by us.
user_args=()
[[ $(basename "$engine") == docker ]] && user_args=(--user "$(id -u):$(id -g)" -e HOME=/tmp)

"$engine" run --rm "${user_args[@]}" \
    -v "$repo:/src" \
    -v "$cache/cargo-registry:/opt/cargo/registry" \
    -e CARGO_TARGET_DIR=/src/packaging/cache/target \
    -e ODDECH_CUDA=$cuda \
    -w /src \
    "$image" packaging/inside.sh
