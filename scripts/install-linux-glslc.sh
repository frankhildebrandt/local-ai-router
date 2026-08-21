#!/usr/bin/env bash
set -euo pipefail

# ggml-vulkan requires glslc and recent Vulkan/SPIRV headers. Ubuntu 22.04 main
# ships headers too old for current llama.cpp; pull build deps from LunarG.
if command -v glslc >/dev/null 2>&1 && pkg-config --atleast-version=1.3.290 vulkan 2>/dev/null; then
  glslc --version | head -1
  exit 0
fi

wget -qO- https://packages.lunarg.com/lunarg-signing-key-pub.asc \
  | sudo gpg --dearmor -o /usr/share/keyrings/lunarg.gpg
echo "deb [signed-by=/usr/share/keyrings/lunarg.gpg] https://packages.lunarg.com/vulkan/ jammy main" \
  | sudo tee /etc/apt/sources.list.d/lunarg-vulkan-jammy.list >/dev/null
sudo apt-get update
sudo apt-get install -y shaderc spirv-headers spirv-tools vulkan-headers vulkan-loader-dev
command -v glslc
glslc --version | head -1
pkg-config --modversion vulkan
