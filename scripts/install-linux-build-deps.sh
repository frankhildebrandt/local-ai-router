#!/usr/bin/env bash
set -euo pipefail

# Debian/Ubuntu packages needed to compile the desktop shell and GGUF sidecar.
# Runtime .deb installs pull WebKit/GTK automatically; this is the *build* set.
sudo apt-get update
sudo apt-get install -y \
  build-essential \
  cmake \
  curl \
  file \
  libayatana-appindicator3-dev \
  libssl-dev \
  libwebkit2gtk-4.1-dev \
  libxdo-dev \
  librsvg2-dev \
  patchelf \
  pkg-config \
  wget \
  libvulkan-dev \
  vulkan-tools \
  glslang-tools
